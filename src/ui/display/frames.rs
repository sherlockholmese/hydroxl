use super::{DisplayView, GuestResize, RECONNECT_DELAYS, STABLE_DISPLAY_AFTER};
use crate::backend::State;
use crate::display::frame_stats::{FrameRates, FrameStats};
use crate::display::geometry::{GuestViewport, Scaling, guest_resize_target};
use crate::display::scanout;
use crate::display::session::{Resize, Session};
use crate::theme::{CAPTION_SIZE, Palette};
use gpui_kit::component::*;
use gpui_kit::{prelude::*, *};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

impl DisplayView {
    pub(super) fn schedule_spice_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.frame_scheduled || !window.is_visible() {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        self.frame_scheduled = true;
        let view = cx.weak_entity();
        // GPUI runs these callbacks before its draw decision. Upload the latest
        // owned snapshot, and asynchronously request the one for the next frame.
        window.on_next_frame(move |window, cx| {
            let _ = view.update(cx, |this, cx| {
                if !this.session.as_ref().is_some_and(|current| Arc::ptr_eq(current, &session)) {
                    return;
                }
                this.frame_scheduled = false;
                if this.upload(window, cx) {
                    this.schedule_spice_upload(window, cx);
                }
            });
        });
    }

    /// Queues the GPU copy of an offered scanout. The producer is released from the
    /// copy's completion, not from this frame.
    pub(super) fn copy_gpu_draw(
        &mut self,
        session: &Session,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draw = session.frame().lock().unwrap_or_else(|e| e.into_inner()).gpu_draw.take();
        if let Some(scanout::GpuDraw {
            mut frame,
            reply,
        }) = draw
        {
            if !window.is_visible() {
                // Nothing on screen to update; the next visible copy is taken whole.
                self.gpu_stale = true;
                reply.finish(true);
                return;
            }
            if std::mem::take(&mut self.gpu_stale) {
                frame.damage = None;
            }
            let (width, height) = (frame.extent.width.0 as u32, frame.extent.height.0 as u32);
            // The producer keeps its buffer reserved until the GPU finishes the copy;
            // this frame is not held up waiting for it.
            frame.on_complete = Some(Box::new(move |copied| reply.finish(copied)));
            match unsafe { window.copy_dma_buf(self.image, frame) } {
                Ok(Some(texture)) => {
                    if let Some(previous) = self.image.replace(texture)
                        && previous != texture
                    {
                        window.drop_texture(previous);
                    }
                    self.gpu_image = true;
                    self.frame_revision = self.frame_revision.wrapping_add(1);
                    self.size = (width, height);
                    self.status = None;
                    cx.notify();
                }
                // The callback, and with it the reply, was dropped: the producer
                // hears that the copy failed and reads the frame back instead.
                result => {
                    if let Err(error) = result {
                        eprintln!("Vulkan DMA-BUF fallback: {error:#}");
                    }
                    self.gpu_stale = true;
                }
            }
        }
    }

    /// Uploads at most one new frame, freeing the texture it replaces.
    pub(super) fn upload(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(session) = self.session.clone() else {
            return false;
        };
        session.request_frame();
        self.copy_gpu_draw(&session, window, cx);
        if self.scaling == Scaling::GuestResize && window.is_visible() {
            self.update_guest_resolution(&session);
        }
        self.update_overview_preview(&session, window, cx);
        self.sync_clipboard(&session, false, window, cx);
        let mut frame = session.frame().lock().unwrap_or_else(|e| e.into_inner());
        let relative = frame.absolute == Some(false);
        if self.captured {
            drop(frame);
            if !window.is_window_active() {
                self.release(window);
                cx.notify();
            } else {
                self.input.set_relative(window, relative);
                match self.input.poll(window) {
                    Some((dx, dy)) if dx != 0 || dy != 0 => {
                        session.relative_pointer(dx, dy, self.buttons);
                        self.move_relative_cursor(dx, dy);
                        cx.notify();
                    }
                    None => {
                        self.release(window);
                        cx.notify();
                    }
                    _ => {}
                }
            }
            frame = session.frame().lock().unwrap_or_else(|e| e.into_inner());
        }
        if frame.generation == self.generation {
            return true;
        }
        // SPICE already coalesces canvas damage into requested snapshots on its
        // owner thread. Keep the existing settling for the other transports only.
        if !matches!(session.as_ref(), Session::Spice(_))
            && !frame.pixels.is_empty()
            && frame.closed.is_none()
            && self.image.is_some()
        {
            let now = Instant::now();
            if self.settle.0 != frame.generation {
                self.settle.0 = frame.generation;
                self.settle.1 = now;
            }
            let first = *self.settle.2.get_or_insert(now);
            // QEMU sends a refreshed screen every 30 ms, so a burst rarely lasts longer.
            if now - self.settle.1 < Duration::from_millis(5)
                && now - first < Duration::from_millis(40)
            {
                return true;
            }
        }
        self.settle.2 = None;
        self.generation = frame.generation;
        if let Some(error) = frame.closed.clone() {
            drop(frame);
            self.detach_session(window);
            if self.connected_at.take().is_some_and(|since| since.elapsed() >= STABLE_DISPLAY_AFTER)
            {
                self.reconnect_attempts = 0;
            }
            if self.machine_is_active() && self.reconnect_attempts < RECONNECT_DELAYS.len() {
                self.status = Some(crate::ui::primitives::plain_error(&error));
                self.connecting = true;
                self.connect_session(true, window, cx);
            } else if self.machine_is_active() {
                self.status = Some(crate::ui::primitives::plain_error(&error));
            } else {
                self.status = Some(self.power_status.into());
            }
            cx.notify();
            return false;
        }
        if !window.is_visible() {
            if std::env::var_os("HYDROXL_DEBUG_DISPLAY").is_some() {
                eprintln!("upload: window not visible");
            }
            self.generation = u64::MAX;
            return true;
        }
        if let Some(error) = &frame.display_error {
            if self.image.is_none() {
                self.status = Some(crate::ui::primitives::plain_error(error));
                cx.notify();
            }
            return true;
        }
        let (w, h) = (frame.width, frame.height);
        if w == 0 || h == 0 {
            return true;
        }
        let gpu_only = frame.pixels.is_empty();
        if std::env::var_os("HYDROXL_DEBUG_DISPLAY").is_some() {
            eprintln!(
                "upload: gen {} {}x{} pixels {} gpu_only {gpu_only} image {:?} size {:?}",
                frame.generation,
                w,
                h,
                frame.pixels.len(),
                self.image.is_some(),
                self.size
            );
        }
        let resized = !gpu_only && (self.image.is_none() || self.size != (w, h) || self.gpu_image);
        let mut changed = resized;
        // Borrow rows while locked instead of allocating packed patches or a
        // full-frame clone. The texture API copies into its retained CPU image
        // before returning and defers GPU writes until paint.
        let upload = (|| -> anyhow::Result<()> {
            if gpu_only {
                frame.discard_damage();
            } else if resized {
                let next = window.create_texture(
                    size(DevicePixels(w as i32), DevicePixels(h as i32)),
                    &frame.pixels,
                )?;
                if let Some(old) = self.image.replace(next) {
                    window.drop_texture(old);
                }
                self.gpu_image = false;
                frame.discard_damage();
            } else if let Some(id) = self.image {
                for rect in frame.take_damage_rects() {
                    changed = true;
                    let start = ((rect.y as usize * w as usize) + rect.x as usize) * 4;
                    window.update_texture_with_stride(
                        id,
                        Bounds::new(
                            point(DevicePixels(rect.x as i32), DevicePixels(rect.y as i32)),
                            size(DevicePixels(rect.width as i32), DevicePixels(rect.height as i32)),
                        ),
                        &frame.pixels[start..],
                        w as usize * 4,
                    )?;
                }
            }
            Ok(())
        })();
        let cursor = (frame.cursor_generation != self.cursor_generation)
            .then(|| (frame.cursor_generation, frame.cursor.clone()));
        drop(frame);
        if let Some((generation, shape)) = cursor {
            self.cursor_generation = generation;
            let image = shape.as_ref().and_then(|c| {
                image::RgbaImage::from_raw(c.width, c.height, c.pixels.clone())
                    .map(|b| Arc::new(RenderImage::new(vec![image::Frame::new(b)])))
            });
            if let Some((old, _)) = self.cursor.take() {
                cx.drop_image(old, Some(window));
            }
            self.cursor = image.zip(shape);
            cx.notify();
        }
        if gpu_only {
            return true;
        }
        if let Err(error) = upload {
            eprintln!("Guest texture upload: {error}");
            session.frame().lock().unwrap_or_else(|e| e.into_inner()).damage_all();
            self.generation = u64::MAX;
            return true;
        }
        self.size = (w, h);
        self.status = None;
        if changed {
            self.frame_revision = self.frame_revision.wrapping_add(1);
        }
        cx.notify();
        true
    }

    pub(super) fn toggle_performance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.performance.take().is_some() {
            self.performance_readout = FrameRates::default();
            cx.notify();
            return;
        }
        let stats = Rc::new(RefCell::new(FrameStats::default()));
        self.performance = Some(stats.clone());
        cx.notify();
        cx.spawn_in(window, async move |view, cx| {
            loop {
                let updating = view.update_in(cx, |view, window, cx| {
                    if view.performance.as_ref().is_none_or(|current| !Rc::ptr_eq(current, &stats))
                    {
                        return false;
                    }
                    let ready = window.is_visible()
                        && view.state == Some(State::Running)
                        && view.session.is_some()
                        && view.image.is_some();
                    let mut stats = stats.borrow_mut();
                    let readout = if ready {
                        stats.sample(Instant::now())
                    } else {
                        stats.reset();
                        FrameRates::default()
                    };
                    if readout != view.performance_readout {
                        view.performance_readout = readout;
                        cx.notify();
                    }
                    true
                });
                if !matches!(updating, Ok(true)) {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(250)).await;
            }
        })
        .detach();
    }

    pub(super) fn performance_overlay(&self, p: Palette) -> impl IntoElement {
        let rate = |value: Option<f64>| value.map_or_else(|| "—".into(), |fps| format!("{fps:.1}"));
        let samples = self.performance_readout;
        v_flex()
            .absolute()
            .right_3()
            .top_3()
            .gap_1()
            .bg(p.canvas)
            .text_color(p.text)
            .border_1()
            .border_color(p.border)
            .rounded_md()
            .px_3()
            .py_2()
            .text_size(px(CAPTION_SIZE))
            .children(
                [
                    ("Live FPS", samples.live),
                    ("Average FPS", samples.average),
                    ("1% low", samples.low),
                ]
                .map(|(label, fps)| {
                    h_flex()
                        .justify_between()
                        .gap_3()
                        .child(div().text_color(p.muted).child(label))
                        .child(
                            div()
                                .w_12()
                                .text_right()
                                .font_features(crate::ui::primitives::tabular())
                                .font_weight(FontWeight::MEDIUM)
                                .child(rate(fps)),
                        )
                }),
            )
    }

    fn update_overview_preview(
        &mut self,
        session: &Session,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let state = workspace.read(cx);
        if state.preferences.connection != self.connection
            || state.tab != 0
            || state.selected.as_deref() != Some(&self.machine)
        {
            return;
        }
        let has_preview = state.live_preview.as_ref().is_some_and(|(id, _)| id == &self.machine);
        if has_preview
            && self.last_preview.is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(4))
        {
            return;
        }
        let mut frame = session.frame().lock().unwrap_or_else(|error| error.into_inner());
        let generation = frame.generation;
        if has_preview && self.last_preview.is_some_and(|(last, _)| last == generation) {
            return;
        }
        let thumbnail = if self.gpu_image {
            let thumbnail = frame.thumbnail.take();
            frame.preview_requested = thumbnail.is_none();
            thumbnail
        } else {
            crate::display::thumbnail::sample(frame.width, frame.height, &frame.pixels)
        };
        drop(frame);
        let Some(thumbnail) = thumbnail else {
            return;
        };
        self.last_preview = Some((generation, Instant::now()));
        let preview = Arc::new(RenderImage::new(vec![image::Frame::new(thumbnail)]));
        if let Some((_, previous)) = workspace.update(cx, |workspace, cx| {
            let previous = workspace.live_preview.replace((self.machine.clone(), preview));
            cx.notify();
            previous
        }) {
            cx.drop_image(previous, Some(window));
        }
    }

    fn update_guest_resolution(&mut self, session: &Session) {
        let Some(dimensions) = guest_resize_target(self.bounds.get(), self.device_scale) else {
            self.guest_resize = None;
            return;
        };
        if self.guest_resize.as_ref().is_none_or(|request| request.dimensions != dimensions) {
            self.guest_resize = Some(GuestResize {
                dimensions,
                changed_at: Instant::now(),
                sent: false,
            });
            return;
        }
        let Some(request) = &mut self.guest_resize else {
            return;
        };
        if request.sent || request.changed_at.elapsed() < Duration::from_millis(250) {
            return;
        }
        let (screen, current) = {
            let frame = session.frame().lock().unwrap_or_else(|e| e.into_inner());
            let support = if frame.spice_resize {
                Some(Resize::Spice)
            } else if frame.dbus_resize {
                Some(Resize::Dbus)
            } else {
                frame.resize_screen.map(Resize::Vnc)
            };
            (support, (frame.width as u16, frame.height as u16))
        };
        if let Some(screen) = screen {
            request.sent = true;
            if current != dimensions
                && let Err(error) = session.resize_desktop(dimensions, screen)
            {
                eprintln!("Could not request guest resolution: {error}");
            }
        }
    }

    pub(super) fn viewport(&self) -> Option<GuestViewport> {
        GuestViewport::new(self.size, self.bounds.get(), self.scaling, self.device_scale)
    }
}
