//! The guest display window renders QEMU displays, captures keyboard and pointer on
//! click (Ctrl+Alt releases), and carries power controls and special keys in a top bar.
use super::primitives::{MENU_MIN_WIDTH, glyph};
use crate::app::{ActionSource, Job, Workspace};
use crate::backend::{Action, Connection, DisplayTarget, Machine, State};
use crate::display::frame_stats::{FrameRates, FrameStats};
use crate::display::geometry::{GuestViewport, Scaling, guest_resize_target};
use crate::display::scanout;
use crate::display::session::{Resize, Session};
use crate::display::vnc::{self, Endpoint};
use crate::operations::Operation;
use crate::theme::{BODY_SIZE, CAPTION_SIZE, Palette};
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{button::*, *};
use gpui_kit::{prelude::*, *};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const TOOLBAR_HEIGHT: f32 = 44.;
const COMPACT_TOOLBAR_WIDTH: f32 = 480.;

struct GuestResize {
    dimensions: (u16, u16),
    changed_at: Instant,
    sent: bool,
}

const CTRL: u32 = 0xffe3;
const ALT: u32 = 0xffe9;
const SUPER: u32 = 0xffeb;
/// Special key combinations the host would otherwise intercept, as keycaps. Groups are
/// separated in the menu: session, consoles, then window keys.
const SPECIAL_KEYS: &[(&[&str], &[u32], u8)] = &[
    (&["Ctrl", "Alt", "Del"], &[CTRL, ALT, 0xffff], 0),
    (&["Ctrl", "Alt", "Backspace"], &[CTRL, ALT, 0xff08], 0),
    (&["Ctrl", "Alt", "F1"], &[CTRL, ALT, 0xffbe], 1),
    (&["Ctrl", "Alt", "F2"], &[CTRL, ALT, 0xffbf], 1),
    (&["Ctrl", "Alt", "F3"], &[CTRL, ALT, 0xffc0], 1),
    (&["Ctrl", "Alt", "F4"], &[CTRL, ALT, 0xffc1], 1),
    (&["Ctrl", "Alt", "F5"], &[CTRL, ALT, 0xffc2], 1),
    (&["Ctrl", "Alt", "F6"], &[CTRL, ALT, 0xffc3], 1),
    (&["Ctrl", "Alt", "F7"], &[CTRL, ALT, 0xffc4], 1),
    (&["Alt", "Tab"], &[ALT, 0xff09], 2),
    (&["Alt", "F4"], &[ALT, 0xffc1], 2),
    (&["Ctrl", "Esc"], &[CTRL, 0xff1b], 2),
    (&["Super"], &[SUPER], 2),
    (&["Print Screen"], &[0xff61], 2),
];

/// A key drawn as a keycap: the same shape in menus and in the capture hint.
fn keycap(label: &'static str, cx: &App) -> Div {
    let p = Palette::get(cx);
    div()
        .px(px(5.))
        .min_w(px(20.))
        .h(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .border_b_2()
        .border_color(p.border)
        .bg(p.well)
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.text)
        .child(label)
}

fn scaling_icon(mode: Scaling) -> &'static str {
    match mode {
        Scaling::Fit => "shrink",
        Scaling::GuestResize => "scan",
        Scaling::Stretch => "move-diagonal-2",
        Scaling::Fill => "expand",
        Scaling::ActualSize => "ratio",
    }
}

pub(crate) struct DisplayView {
    session: Option<Arc<Session>>,
    image: Option<TextureId>,
    gpu_image: bool,
    /// The GPU texture missed a frame, so the next copy must be whole, not just damage.
    gpu_stale: bool,
    input: crate::display::input::InputCapture,
    generation: u64,
    size: (u32, u32),
    bounds: Rc<Cell<Bounds<Pixels>>>,
    scaling: Scaling,
    guest_resize: Option<GuestResize>,
    toolbar_pinned: bool,
    toolbar_revealed: bool,
    toolbar_menu_open: bool,
    connection: Connection,
    device_scale: f32,
    focus: FocusHandle,
    buttons: u8,
    modifiers: Modifiers,
    status: Option<String>,
    /// Keyboard and pointer go to the guest only while captured.
    captured: bool,
    /// Capture on click rather than when the pointer enters the guest image.
    capture_on_click: bool,
    /// Set by Ctrl+Alt in hover mode, so releasing does not recapture at once.
    /// Cleared when the pointer leaves the guest image.
    hover_suppressed: bool,
    /// Tile settling for software frames: last generation seen, when it was seen, and
    /// when the first not-yet-shown change arrived.
    settle: (u64, Instant, Option<Instant>),
    /// Last guest clipboard announcement copied to the host.
    guest_clipboard_seen: u64,
    /// The manual "Copy guest clipboard" is waiting for text, to confirm when it arrives.
    copy_requested: bool,
    /// Last text exchanged either way, so a copy is never echoed back.
    clipboard_text: Option<String>,
    /// Last host pointer position, where the guest cursor is drawn.
    pointer: Option<Point<Pixels>>,
    cursor: Option<(Arc<RenderImage>, vnc::Cursor)>,
    /// The guest cursor as the system cursor: which shape and scale the platform shows,
    /// and that shape's pixels. The compositor moves it with the pointer, so it never
    /// trails the way an in-window overlay does.
    system_cursor: Option<(u64, u32)>,
    system_cursor_pixels: Option<(u64, Arc<[u8]>)>,
    system_cursor_unsupported: bool,
    cursor_generation: u64,
    /// At most one SPICE upload callback is pending on the platform frame clock.
    frame_scheduled: bool,
    last_preview: Option<(u64, Instant)>,
    workspace: WeakEntity<Workspace>,
    machine: String,
    /// Copied from the workspace when it changes; render never reads the workspace.
    state: Option<State>,
    busy: bool,
    connecting: bool,
    frame_revision: u64,
    performance: Option<Rc<RefCell<FrameStats>>>,
    performance_readout: FrameRates,
    _observe: Option<Subscription>,
    _focus_out: Subscription,
    _activation: Subscription,
}

impl Drop for DisplayView {
    fn drop(&mut self) {
        if let Some(session) = &self.session {
            session.close();
        }
    }
}

impl DisplayView {
    /// Connects to the guest display through libvirt.
    pub(crate) fn connect(
        connection: Connection,
        id: String,
        state: State,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let machine = id.clone();
        // A guest capture has its own explicit hide/show lifecycle. Typing must
        // not leave a second automatic hide active after the capture releases.
        window.set_cursor_hide_mode_override(Some(CursorHideMode::Never));
        let observe = workspace.upgrade().map(|w| {
            cx.observe(&w, |this: &mut Self, workspace, cx| {
                let w = workspace.read(cx);
                let state = w.machines.iter().find(|m| m.id == this.machine).map(|m| m.state);
                let busy = w.busy();
                if (this.state, this.busy) != (state, busy) {
                    this.state = state;
                    this.busy = busy;
                    cx.notify();
                }
            })
        });
        cx.on_release_in(window, |this, window, _| {
            this.release(window);
            if let Some(id) = this.image.take() {
                window.drop_texture(id);
            }
        })
        .detach();
        let focus = cx.focus_handle();
        // Losing focus (another window, host Alt+Tab) releases the capture.
        let focus_out = cx.on_focus_out(&focus, window, |this, _, window, cx| {
            if this.captured {
                this.release(window);
                cx.notify();
            }
        });
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() && this.captured {
                this.release(window);
                cx.notify();
            }
            if window.is_window_active() {
                // Coming back from another app is when a host copy is about to be pasted.
                this.offer_host_clipboard(cx);
            }
        });
        let (scaling, toolbar_pinned, capture_on_click) = workspace
            .upgrade()
            .map(|workspace| {
                let preferences = &workspace.read(cx).preferences;
                (
                    preferences.scaling_for(&machine),
                    preferences.toolbar_pinned,
                    preferences.capture_on_click,
                )
            })
            .unwrap_or((Scaling::default(), false, false));
        let mut view = Self {
            session: None,
            connection,
            toolbar_pinned,
            toolbar_revealed: false,
            toolbar_menu_open: false,
            image: None,
            gpu_image: false,
            gpu_stale: false,
            input: Default::default(),
            generation: u64::MAX,
            size: (0, 0),
            bounds: Rc::new(Cell::new(Bounds::default())),
            scaling,
            guest_resize: None,
            device_scale: window.scale_factor(),
            focus,
            buttons: 0,
            modifiers: Modifiers::default(),
            status: Some(state.label().into()),
            captured: false,
            capture_on_click,
            hover_suppressed: false,
            guest_clipboard_seen: 0,
            settle: (u64::MAX, Instant::now(), None),
            clipboard_text: None,
            copy_requested: false,
            pointer: None,
            cursor: None,
            system_cursor: None,
            system_cursor_pixels: None,
            system_cursor_unsupported: false,
            cursor_generation: 0,
            frame_scheduled: false,
            last_preview: None,
            state: Some(state),
            busy: false,
            connecting: false,
            frame_revision: 0,
            performance: None,
            performance_readout: FrameRates::default(),
            workspace,
            machine,
            _observe: observe,
            _focus_out: focus_out,
            _activation: activation,
        };
        if matches!(state, State::Running | State::Paused)
            || std::env::var_os("HYDROXL_VNC_SOCKET").is_some()
        {
            view.ensure_session(window, cx);
        }
        view
    }

    pub(crate) fn ensure_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.is_none() && !self.connecting {
            self.status = Some("Connecting".into());
            self.connecting = true;
            Self::connect_session(self.connection, self.machine.clone(), false, window, cx);
            cx.notify();
        }
    }

    fn connect_session(
        connection: Connection,
        id: String,
        retry: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let gpu = window.supports_dma_buf();
        cx.spawn_in(window, async move |this, cx| {
            loop {
                if retry {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                }
                if this.upgrade().is_none() {
                    return;
                }
                let id = id.clone();
                let prefer_spice = this
                    .update(cx, |view, cx| view.machine(cx).is_none_or(|m| m.spice_viewer))
                    .unwrap_or(true);
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        match crate::backend::display_target(connection, &id, prefer_spice)? {
                            DisplayTarget::Vnc(uri) => {
                                let endpoint = Endpoint::parse(&uri).ok_or_else(|| {
                                    anyhow::anyhow!("Invalid guest display address.")
                                })?;
                                let session = Session::vnc(&endpoint)?;
                                if std::env::var_os("HYDROXL_VNC_SOCKET").is_none()
                                    && let Err(error) =
                                        crate::backend::ensure_live_tablet(connection, &id)
                                {
                                    eprintln!(
                                        "Could not add a tablet to the running guest: {error}"
                                    );
                                }
                                Ok(session)
                            }
                            DisplayTarget::Spice(options) => {
                                Session::spice(connection, id, options, gpu)
                            }
                            DisplayTarget::Dbus {
                                graphics,
                                clipboard,
                                usb,
                            } => Session::dbus(connection, &id, graphics, gpu, clipboard, usb),
                        }
                    })
                    .await;
                let mut result = Some(result);
                let keep_waiting = this
                    .update_in(cx, |this, window, cx| {
                        let wait = match result.take().expect("one display connection result") {
                            Ok(session) => {
                                this.connecting = false;
                                this.attach(session, window, cx);
                                false
                            }
                            Err(error) => {
                                if retry && this.state != Some(State::Stopped) {
                                    this.status = Some(format!("{error:#}"));
                                    cx.notify();
                                    return true;
                                }
                                this.connecting = false;
                                this.status =
                                    Some(if retry && this.state == Some(State::Stopped) {
                                        State::Stopped.label().into()
                                    } else {
                                        format!("{error:#}")
                                    });
                                false
                            }
                        };
                        cx.notify();
                        wait
                    })
                    .unwrap_or(false);
                if let Some(Ok(session)) = result {
                    session.close();
                }
                if !keep_waiting {
                    return;
                }
            }
        })
        .detach();
    }

    fn attach(&mut self, session: Arc<Session>, window: &mut Window, cx: &mut Context<Self>) {
        let frame_clock = matches!(session.as_ref(), Session::Spice(_));
        self.session = Some(session.clone());
        if let Some(stats) = &self.performance {
            stats.borrow_mut().reset();
            self.performance_readout = FrameRates::default();
        }
        self.frame_scheduled = false;
        self.guest_resize = None;
        self.last_preview = None;
        self.guest_clipboard_seen = 0;
        self.copy_requested = false;
        self.clipboard_text = None;
        self.generation = u64::MAX;
        self.cursor_generation = 0;
        if let Some((cursor, _)) = self.cursor.take() {
            cx.drop_image(cursor, Some(window));
        }
        // A reconnect keeps showing the last frame rather than a spinner.
        if self.image.is_none() {
            self.status = Some("Connecting".into());
        }
        self.focus.focus(window, cx);
        if matches!(session.as_ref(), Session::Dbus(_)) && window.is_window_active() {
            self.offer_host_clipboard(cx);
        }
        if frame_clock {
            self.schedule_spice_upload(window, cx);
        }
        // GPU draws are copied the moment they arrive; the frame loop only presents.
        {
            let session = session.clone();
            cx.spawn_in(window, async move |this, cx| {
                loop {
                    scanout::DrawOffered(session.frame()).await;
                    let current = this.update_in(cx, |this, window, cx| {
                        let current = this
                            .session
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, &session));
                        if current {
                            this.copy_gpu_draw(&session, window, cx);
                        }
                        current
                    });
                    if !matches!(current, Ok(true)) {
                        break;
                    }
                }
            })
            .detach();
        }
        // SPICE uses platform frame opportunities while visible. A slow timer
        // handles hidden-window housekeeping and restarts the frame loop on show.
        // Other transports retain their existing low-latency polling.
        cx.spawn_in(window, async move |this, cx| {
            let mut interval = Duration::from_millis(if frame_clock {
                50
            } else {
                4
            });
            loop {
                cx.background_executor().timer(interval).await;
                match this.update_in(cx, |this, window, cx| {
                    if !this.session.as_ref().is_some_and(|current| Arc::ptr_eq(current, &session))
                    {
                        return (false, false);
                    }
                    let visible = window.is_visible();
                    let alive = if frame_clock && visible {
                        this.schedule_spice_upload(window, cx);
                        true
                    } else {
                        this.upload(window, cx)
                    };
                    (alive, visible)
                }) {
                    Ok((true, visible)) => {
                        interval = Duration::from_millis(if visible && !frame_clock {
                            4
                        } else {
                            50
                        });
                    }
                    _ => break,
                }
            }
        })
        .detach();
    }

    fn schedule_spice_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn copy_gpu_draw(&mut self, session: &Session, window: &mut Window, cx: &mut Context<Self>) {
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
    fn upload(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(session) = self.session.clone() else {
            return false;
        };
        session.request_frame();
        self.copy_gpu_draw(&session, window, cx);
        if self.scaling == Scaling::GuestResize && window.is_visible() {
            self.update_guest_resolution(&session);
        }
        self.update_overview_preview(&session, window, cx);
        self.copy_guest_clipboard(&session, window, cx);
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
            self.release(window);
            session.close();
            self.session = None;
            if let Some(image) = self.image.take() {
                window.drop_texture(image);
            }
            self.gpu_image = false;
            self.status = Some(error);
            self.connecting = true;
            Self::connect_session(self.connection, self.machine.clone(), true, window, cx);
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

    fn toggle_performance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    fn performance_overlay(&self, p: Palette) -> impl IntoElement {
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
        if state.tab != 0 || state.selected.as_deref() != Some(&self.machine) {
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

    fn viewport(&self) -> Option<GuestViewport> {
        GuestViewport::new(self.size, self.bounds.get(), self.scaling, self.device_scale)
    }

    /// Guest framebuffer coordinates for a logical window position, letterboxing included.
    fn guest_point(&self, position: Point<Pixels>) -> Option<(u16, u16)> {
        let viewport = self.viewport()?;
        let result = viewport.guest_point(f32::from(position.x), f32::from(position.y));
        if std::env::var_os("HYDROXL_DEBUG_POINTER").is_some() {
            eprintln!(
                "pointer {position:?} bounds {:?} fitted {viewport:?} -> guest {result:?}",
                self.bounds.get()
            );
        }
        Some(result)
    }

    fn send_pointer(&mut self, position: Point<Pixels>) {
        if !self.input.locked() {
            self.pointer = Some(position);
        }
        if !self.captured {
            return;
        }
        if let (Some((x, y)), Some(s)) = (self.guest_point(position), &self.session) {
            if self.input.locked() {
                s.relative_pointer(0, 0, self.buttons);
            } else {
                s.pointer(x, y, self.buttons);
            }
        }
    }

    fn move_relative_cursor(&mut self, dx: i32, dy: i32) {
        let (Some(pointer), Some(viewport)) = (self.pointer, self.viewport()) else {
            return;
        };
        let (x, y) = viewport.move_pointer(f32::from(pointer.x), f32::from(pointer.y), dx, dy);
        self.pointer = Some(point(px(x), px(y)));
    }

    /// Hands the guest cursor to the platform while input is captured with a free
    /// pointer. A locked pointer stays put, so that case keeps the in-window overlay.
    fn sync_system_cursor(&mut self, window: &mut Window) {
        if self.system_cursor_unsupported {
            return;
        }
        let wanted = if self.captured && !self.input.locked() {
            self.cursor.as_ref().map(|(_, shape)| shape.clone()).zip(self.viewport())
        } else {
            None
        };
        let generation = self.cursor_generation;
        let key = wanted.as_ref().map(|(_, viewport)| (generation, viewport.scale_x.to_bits()));
        if key == self.system_cursor {
            return;
        }
        let image = wanted.map(|(shape, viewport)| {
            let bgra = match &self.system_cursor_pixels {
                Some((cached, pixels)) if *cached == generation => pixels.clone(),
                _ => {
                    let pixels: Arc<[u8]> = shape.pixels.clone().into();
                    self.system_cursor_pixels = Some((generation, pixels.clone()));
                    pixels
                }
            };
            gpui_kit::CursorImage {
                width: shape.width,
                height: shape.height,
                hot_x: shape.hot_x,
                hot_y: shape.hot_y,
                bgra,
                scale: viewport.scale_x,
            }
        });
        let showing = image.is_some();
        match window.set_cursor_image(image) {
            Ok(()) => self.system_cursor = key,
            Err(error) => {
                if showing {
                    eprintln!("Guest cursor as system cursor: {error}");
                    self.system_cursor_unsupported = true;
                }
                self.system_cursor = None;
            }
        }
    }

    /// The guest cursor at the host pointer, scaled like the framebuffer. Drawn locally,
    /// so it never trails the pointer.
    fn cursor_overlay(&self) -> Option<AnyElement> {
        if !self.captured || self.system_cursor.is_some() {
            return None;
        }
        let (image, shape) = self.cursor.clone()?;
        let pointer = self.pointer?;
        let viewport = self.viewport()?;
        let b = self.bounds.get();
        let (sx, sy) = (viewport.scale_x, viewport.scale_y);
        let left = f32::from(pointer.x) - f32::from(b.origin.x) - shape.hot_x as f32 * sx;
        let top = f32::from(pointer.y) - f32::from(b.origin.y) - shape.hot_y as f32 * sy;
        Some(
            img(image)
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(shape.width as f32 * sx))
                .h(px(shape.height as f32 * sy))
                .into_any_element(),
        )
    }

    fn mask(button: MouseButton) -> u8 {
        match button {
            MouseButton::Left => 1,
            MouseButton::Middle => 2,
            MouseButton::Right => 4,
            _ => 0,
        }
    }

    /// Hands keyboard and pointer back to the host, releasing anything held in the guest.
    fn release(&mut self, window: &mut Window) {
        if self.system_cursor.take().is_some() {
            let _ = window.set_cursor_image(None);
        }
        self.input.release(window);
        if let Some(s) = &self.session {
            s.release_input();
        }
        self.modifiers = Modifiers::default();
        self.buttons = 0;
        self.captured = false;
    }

    /// Hands keyboard and pointer to the guest. `click` is false for hover capture.
    fn capture(
        &mut self,
        position: Point<Pixels>,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.focus.focus(window, cx);
        self.captured = true;
        self.toolbar_revealed = false;
        self.modifiers = modifiers;
        self.pointer = Some(position);
        session.reset_pointer();
        let relative =
            session.frame().lock().unwrap_or_else(|e| e.into_inner()).absolute == Some(false);
        self.input.begin(window, relative);
        for (held, key) in [
            (modifiers.shift, vnc::MODIFIERS[0]),
            (modifiers.control, vnc::MODIFIERS[1]),
            (modifiers.alt, vnc::MODIFIERS[2]),
            (modifiers.platform, vnc::MODIFIERS[3]),
        ] {
            if held {
                session.key(key, true);
            }
        }
        self.offer_host_clipboard(cx);
        cx.notify();
    }

    /// Whether a window position is over the guest image (not the letterbox around it).
    fn over_guest(&self, position: Point<Pixels>) -> bool {
        self.viewport().is_some_and(|viewport| viewport.bounds().contains(&position))
    }

    /// Whether moving over the guest image captures input. Needs an absolute pointer:
    /// a relative one is locked to the window and could never leave again.
    fn hover_capture(&self) -> bool {
        !self.capture_on_click && self.absolute_pointer()
    }

    /// The guest follows the host pointer's position (a tablet), so the pointer can
    /// leave the guest image and release input in either capture mode.
    fn absolute_pointer(&self) -> bool {
        self.session.as_ref().is_some_and(|session| {
            session.frame().lock().unwrap_or_else(|e| e.into_inner()).absolute != Some(false)
        })
    }

    fn set_capture_mode(&mut self, on_click: bool, cx: &mut Context<Self>) {
        self.capture_on_click = on_click;
        self.hover_suppressed = false;
        let _ = self.workspace.update(cx, |workspace, _| {
            workspace.preferences.capture_on_click = on_click;
            workspace.preferences.save();
        });
        cx.notify();
    }

    /// Host to guest: offers the host's clipboard text to the guest agent when it changed.
    fn offer_host_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let shared = {
            let frame = session.frame().lock().unwrap_or_else(|e| e.into_inner());
            frame.shares_clipboard()
        };
        if !shared {
            return;
        }
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if self.clipboard_text.as_ref() != Some(&text) {
            self.clipboard_text = Some(text.clone());
            session.send_clipboard(text);
        }
    }

    /// Guest to host: copies new guest clipboard text to the host as soon as it is announced.
    fn copy_guest_clipboard(
        &mut self,
        session: &Arc<Session>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (serial, vnc_text) = {
            let mut frame = session.frame().lock().unwrap_or_else(|e| e.into_inner());
            if !frame.shares_clipboard() || frame.guest_clipboard == self.guest_clipboard_seen {
                return;
            }
            (frame.guest_clipboard, frame.clipboard_text.take())
        };
        self.guest_clipboard_seen = serial;
        // VNC delivers the text with the announcement. SPICE and D-Bus request it.
        if let Some(text) = vnc_text {
            self.clipboard_text = Some(text.clone());
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            if std::mem::take(&mut self.copy_requested) {
                window.push_notification("Guest clipboard copied", cx);
            }
            return;
        }
        let announce = std::mem::take(&mut self.copy_requested);
        let Ok(rx) = session.read_clipboard() else {
            return;
        };
        let origin = Arc::downgrade(session);
        cx.spawn_in(window, async move |view, cx| {
            let text =
                cx.background_executor()
                    .spawn(async move {
                        rx.recv_timeout(Duration::from_secs(4)).ok().and_then(Result::ok)
                    })
                    .await;
            if let Some(text) = text.filter(|t| !t.is_empty()) {
                let _ = view.update_in(cx, |view, window, cx| {
                    if view.guest_clipboard_seen != serial
                        || !origin.upgrade().is_some_and(|session| {
                            view.session
                                .as_ref()
                                .is_some_and(|current| Arc::ptr_eq(current, &session))
                        })
                    {
                        return;
                    }
                    view.clipboard_text = Some(text.clone());
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    if announce {
                        window.push_notification("Guest clipboard copied", cx);
                    }
                });
            }
        })
        .detach();
    }

    /// Manual guest-to-host copy, for when automatic sync is off or missed a change.
    fn copy_guest_clipboard_now(
        &mut self,
        session: &Session,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (shared, vnc) = {
            let frame = session.frame().lock().unwrap_or_else(|e| e.into_inner());
            (frame.shares_clipboard(), frame.vnc_clipboard)
        };
        if vnc {
            self.copy_requested = true;
            if let Err(error) = session.request_vnc_clipboard() {
                self.copy_requested = false;
                window.push_notification(error.to_string(), cx);
            }
        } else if shared {
            let view = cx.entity().downgrade();
            Self::receive_session_result(
                session.read_clipboard(),
                move |text, window, cx| {
                    let _ = view.update(cx, |view, _| view.clipboard_text = Some(text.clone()));
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    window.push_notification("Guest clipboard copied", cx);
                },
                window,
                cx,
            );
        } else {
            window.push_notification(
                "Clipboard sharing needs the guest agent (spice-vdagent) in the guest.",
                cx,
            );
        }
    }

    fn machine(&self, cx: &App) -> Option<Machine> {
        let workspace = self.workspace.upgrade()?;
        workspace.read(cx).machines.iter().find(|m| m.id == self.machine).cloned()
    }

    fn run(&self, job: impl FnOnce(Machine) -> Job, cx: &mut Context<Self>) {
        let Some(machine) = self.machine(cx) else {
            return;
        };
        let job = job(machine);
        let _ = self.workspace.update(cx, |w, cx| w.begin(job, None, cx));
    }

    fn power(&self, action: Action, window: &Window, cx: &mut Context<Self>) {
        let source = ActionSource::Viewer(window.window_handle());
        self.run(|machine| Job::Action(machine, action, source), cx);
    }

    fn receive_session_result<T: Send + 'static>(
        result: anyhow::Result<mpsc::Receiver<anyhow::Result<T>>>,
        done: impl FnOnce(T, &mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = match result {
            Ok(rx) => rx,
            Err(error) => {
                window.push_notification(error.to_string(), cx);
                return;
            }
        };
        cx.spawn_in(window, async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    rx.recv_timeout(Duration::from_secs(8))
                        .map_err(|_| anyhow::anyhow!("The guest did not respond."))
                        .and_then(|result| result)
                })
                .await;
            let _ = view.update_in(cx, |_, window, cx| match result {
                Ok(value) => done(value, window, cx),
                Err(error) => window.push_notification(error.to_string(), cx),
            });
        })
        .detach();
    }

    /// Asks before an action that discards guest state, in this window.
    fn confirm(
        &self,
        title: &'static str,
        action: &'static str,
        op: Operation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            let op = op.clone();
            dialog
                .title(title)
                .width(px(400.))
                .child("Unsaved work inside the guest is lost.")
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("cancel-power")
                                .outline()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("confirm-power").danger().label(action).on_click(
                            move |_, window, cx| {
                                window.close_dialog(cx);
                                let op = op.clone();
                                let _ = this
                                    .update(cx, |this, cx| this.run(|m| Job::Operation(m, op), cx));
                            },
                        )),
                )
        });
    }

    /// Input state and capture mode in one control: the label says where input goes, the
    /// menu says how it gets there.
    fn capture_control(
        &self,
        p: Palette,
        compact: bool,
        track_menu: impl Fn(&bool, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let on_click = self.capture_on_click;
        let this = cx.entity().downgrade();
        Button::new("capture-mode")
            .ghost()
            .small()
            .icon(
                glyph(if self.captured {
                    "mouse-pointer-click"
                } else {
                    "mouse-pointer-2"
                })
                .text_color(if self.captured {
                    p.positive
                } else {
                    p.muted
                }),
            )
            .when(!compact, |button| {
                button.label(if self.captured {
                    "Captured"
                } else {
                    "Released"
                })
            })
            .dropdown_caret(true)
            .disabled(self.session.is_none())
            .accessibility_label("Input capture")
            .tooltip(if self.captured {
                "Ctrl Alt releases"
            } else {
                "Input capture"
            })
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let item = |label: &'static str, click: bool| {
                    let this = this.clone();
                    PopupMenuItem::new(label).checked(on_click == click).on_click(
                        move |_, _, cx| {
                            let _ = this.update(cx, |view, cx| view.set_capture_mode(click, cx));
                        },
                    )
                };
                menu.min_w(px(MENU_MIN_WIDTH))
                    .check_side(Side::Right)
                    .item(item("Capture when pointer enters", false))
                    .item(item("Capture on click", true))
            })
            .on_open_change(track_menu)
    }

    fn top_bar(&self, p: Palette, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let compact = window.viewport_size().width < px(COMPACT_TOOLBAR_WIDTH);
        let state = self.state;
        let running = state == Some(State::Running);
        let paused = state == Some(State::Paused);
        let on = running || paused;
        let busy = self.busy;
        let icon = |id: &'static str, glyph: Glyph, label: &'static str| {
            Button::new(id)
                .ghost()
                .small()
                .icon(Icon::new(glyph))
                .accessibility_label(label)
                .tooltip(label)
        };
        let weak = cx.entity().downgrade();
        let track_menu = move |open: &bool, window: &mut Window, cx: &mut App| {
            let _ = weak.update(cx, |view, cx| {
                view.toolbar_menu_open = *open;
                if !open && window.mouse_position().y > px(TOOLBAR_HEIGHT) {
                    view.toolbar_revealed = false;
                }
                cx.notify();
            });
        };
        let track_menu_capture = track_menu.clone();
        let session = self.session.clone();
        let usb = session.as_ref().is_some_and(|session| session.can_toggle_usb());
        let view = cx.entity().downgrade();
        let keys = Button::new("special-keys")
            .ghost()
            .small()
            .icon(glyph("keyboard"))
            .label("Keys")
            .dropdown_caret(true)
            .disabled(session.is_none())
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |mut menu, _, _| {
                menu = menu.min_w(px(MENU_MIN_WIDTH));
                let mut group = None;
                for &(caps, keys, g) in SPECIAL_KEYS {
                    if group.is_some_and(|last| last != g) {
                        menu = menu.separator();
                    }
                    group = Some(g);
                    let session = session.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| {
                            h_flex().gap_1().children(caps.iter().map(|cap| keycap(cap, cx)))
                        })
                        .on_click(move |_, _, _| {
                            if let Some(s) = &session {
                                s.combo(keys);
                            }
                        }),
                    );
                }
                let copy = {
                    let view = view.clone();
                    let session = session.clone();
                    PopupMenuItem::new("Copy guest clipboard")
                        .icon(glyph("clipboard-copy"))
                        .on_click(move |_, window, cx| {
                            let Some(session) = session.clone() else {
                                return;
                            };
                            let _ = view.update(cx, |view, cx| {
                                view.copy_guest_clipboard_now(&session, window, cx)
                            });
                        })
                };
                let view = view.clone();
                menu.separator().item(copy).item(
                    PopupMenuItem::new("Type clipboard text")
                        .icon(glyph("clipboard-paste"))
                        .on_click(move |_, window, cx| {
                            let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
                            else {
                                window.push_notification("The clipboard has no text.", cx);
                                return;
                            };
                            let _ = view.update(cx, |view, cx| {
                                view.run(
                                    |m| {
                                        Job::Operation(
                                            m,
                                            Operation::Guest(crate::operations::GuestSetup::Type(
                                                text,
                                            )),
                                        )
                                    },
                                    cx,
                                )
                            });
                        }),
                )
            })
            .on_open_change(track_menu.clone());
        let scaling = {
            let current = self.scaling;
            let performance_enabled = self.performance.is_some();
            let this = cx.entity().downgrade();
            Button::new("scaling")
                .ghost()
                .small()
                .icon(glyph(scaling_icon(current)))
                .label(current.label())
                .accessibility_label(format!("Display options: {}", current.menu_label()))
                .tooltip(match current {
                    Scaling::Fit => "Fit the existing guest image inside the window",
                    Scaling::GuestResize => {
                        "Match the window when the guest display driver supports resizing"
                    }
                    _ => "Display scaling",
                })
                .dropdown_caret(true)
                .dropdown_menu(move |mut menu, _, _| {
                    menu = menu.min_w(px(MENU_MIN_WIDTH)).check_side(Side::Right);
                    for mode in Scaling::ALL {
                        let this = this.clone();
                        menu = menu.item(
                            PopupMenuItem::new(mode.menu_label())
                                .icon(glyph(scaling_icon(mode)))
                                .checked(mode == current)
                                .on_click(move |_, _, cx| {
                                    let _ = this.update(cx, |view, cx| {
                                        view.scaling = mode;
                                        view.guest_resize = None;
                                        let _ = view.workspace.update(cx, |workspace, _| {
                                            workspace
                                                .preferences
                                                .vm_scaling
                                                .insert(view.machine.clone(), mode);
                                            workspace.preferences.save();
                                        });
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                    let this = this.clone();
                    menu.separator().item(
                        PopupMenuItem::new("Performance overlay")
                            .checked(performance_enabled)
                            .on_click(move |_, window, cx| {
                                let _ =
                                    this.update(cx, |view, cx| view.toggle_performance(window, cx));
                            }),
                    )
                })
                .on_open_change(track_menu.clone())
        };
        let danger = p.danger;
        let power = {
            let this = cx.entity().downgrade();
            icon("power-menu", Glyph::Ellipsis, "More power options")
                .disabled(!on || busy)
                .dropdown_menu(move |menu, _, _| {
                    let item = |label: &'static str, f: fn(&mut DisplayView, &mut Window, &mut Context<DisplayView>)| {
                        let this = this.clone();
                        PopupMenuItem::new(label).on_click(move |_, window, cx| {
                            let _ = this.update(cx, |this, cx| f(this, window, cx));
                        })
                    };
                    menu.min_w(px(180.))
                        .item(item("Reset…", |this, window, cx| {
                            this.confirm("Reset", "Reset", Operation::Reset, window, cx)
                        })
                        .icon(glyph("refresh-ccw-dot").text_color(danger)))
                        .item(item("Force stop…", |this, window, cx| {
                            this.confirm("Force stop", "Force stop", Operation::ForceStop, window, cx)
                        })
                        .icon(glyph("power-off").text_color(danger)))
                })
                .on_open_change(track_menu)
        };
        h_flex()
            .h(px(TOOLBAR_HEIGHT))
            .flex_shrink_0()
            .bg(p.canvas)
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(p.border)
            // The machine's name leads, set off from its controls like the other groups.
            .when_some(
                self.machine(cx).filter(|_| !compact).map(|machine| machine.name),
                |toolbar, name| {
                    toolbar
                        .child(
                            div()
                                .px_2()
                                .max_w(px(240.))
                                .min_w_0()
                                .flex_shrink(1.)
                                .truncate()
                                .text_size(px(BODY_SIZE))
                                .font_weight(FontWeight::MEDIUM)
                                .child(name),
                        )
                        .child(div().w(px(1.)).h(px(20.)).mx_1().flex_shrink_0().bg(p.border))
                },
            )
            .when(!on, |t| {
                t.child(
                    icon("start", Glyph::Play, "Start")
                        .disabled(busy || state != Some(State::Stopped))
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.power(Action::Start, window, cx)
                            }),
                        ),
                )
            })
            .when(paused, |t| {
                t.child(icon("resume", Glyph::Play, "Resume").disabled(busy).on_click(
                    cx.listener(|this, _, window, cx| this.power(Action::Resume, window, cx)),
                ))
            })
            .when(running, |t| {
                t.child(icon("pause", Glyph::Pause, "Pause").disabled(busy).on_click(
                    cx.listener(|this, _, window, cx| this.power(Action::Pause, window, cx)),
                ))
            })
            .when(on, |toolbar| {
                toolbar
                    .child(icon("shutdown", Glyph::Power, "Shut down").disabled(busy).on_click(
                        cx.listener(|view, _, window, cx| view.power(Action::Shutdown, window, cx)),
                    ))
                    .child(
                        Button::new("restart")
                            .ghost()
                            .small()
                            .icon(Icon::new(Glyph::RotateCcw).text_color(p.danger))
                            .accessibility_label("Restart")
                            .tooltip("Restart")
                            .disabled(busy)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.run(|m| Job::Operation(m, Operation::Reboot), cx)
                            })),
                    )
            })
            .child(power)
            .child(div().w(px(1.)).h(px(20.)).mx_1().bg(p.border))
            .child(keys)
            .child(scaling)
            .when(usb, |toolbar| {
                let session = self.session.clone();
                let view = cx.entity().downgrade();
                toolbar.child(
                    icon("viewer-usb", Glyph::Usb, "Redirect USB devices")
                        .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                            let devices = crate::operations::host_usb();
                            if devices.is_empty() {
                                return menu.label("No USB devices");
                            }
                            menu = menu.label("Select a device to connect or disconnect");
                            if devices.len() > 8 {
                                menu = menu.max_h(px(360.)).scrollable(true);
                            }
                            for (address, label) in devices {
                                let Some((bus, address)) =
                                    address.split_once(':').and_then(|(bus, addr)| {
                                        Some((bus.parse::<u8>().ok()?, addr.parse::<u8>().ok()?))
                                    })
                                else {
                                    continue;
                                };
                                let session = session.clone();
                                let view = view.clone();
                                menu = menu.item(PopupMenuItem::new(label).on_click(
                                    move |_, window, cx| {
                                        if let Some(session) = &session {
                                            let _ = view.update(cx, |_, cx| {
                                                Self::receive_session_result(
                                                    session.toggle_usb(bus, address),
                                                    |attached, window, cx| {
                                                        window.push_notification(
                                                            if attached {
                                                                "USB device connected"
                                                            } else {
                                                                "USB device disconnected"
                                                            },
                                                            cx,
                                                        )
                                                    },
                                                    window,
                                                    cx,
                                                )
                                            });
                                        }
                                    },
                                ));
                            }
                            menu
                        }),
                )
            })
            .child(div().flex_1())
            .when(self.captured, |toolbar| {
                // While captured, the way out is the only thing worth saying.
                toolbar.child(
                    h_flex()
                        .gap_1()
                        .mr_1()
                        .text_size(px(CAPTION_SIZE))
                        .text_color(p.muted)
                        .child(keycap("Ctrl", cx))
                        .child(keycap("Alt", cx)),
                )
            })
            .child(self.capture_control(p, compact, track_menu_capture, cx))
            .when(window.is_fullscreen(), |toolbar| {
                toolbar.child(
                    icon(
                        "pin-toolbar",
                        Glyph::Pin,
                        if self.toolbar_pinned {
                            "Unpin toolbar"
                        } else {
                            "Pin toolbar"
                        },
                    )
                    .selected(self.toolbar_pinned)
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.toolbar_pinned = !view.toolbar_pinned;
                        let pinned = view.toolbar_pinned;
                        let _ = view.workspace.update(cx, |workspace, _| {
                            workspace.preferences.toolbar_pinned = pinned;
                            workspace.preferences.save();
                        });
                        cx.notify();
                    })),
                )
            })
            .child(
                icon(
                    "fullscreen",
                    if window.is_fullscreen() {
                        Glyph::Minimize
                    } else {
                        Glyph::Maximize
                    },
                    if window.is_fullscreen() {
                        "Exit full screen"
                    } else {
                        "Full screen"
                    },
                )
                .on_click(cx.listener(|view, _, window, cx| {
                    view.toolbar_revealed = false;
                    window.toggle_fullscreen();
                    cx.notify();
                })),
            )
    }
}

impl Render for DisplayView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::get(cx);
        self.device_scale = window.scale_factor();
        self.sync_system_cursor(window);
        let bounds = self.bounds.clone();
        let (dimensions, scaling) = (self.size, self.scaling);
        let performance = self.performance.clone().filter(|_| self.state == Some(State::Running));
        let revision = self.frame_revision;
        let surface = div()
            .id("guest-display")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .bg(gpui_kit::black())
            .track_focus(&self.focus)
            .child(canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {}).absolute().size_full())
            .when_some(self.image, |this, image| {
                this.child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            if let Some(viewport) = GuestViewport::new(
                                dimensions,
                                bounds,
                                scaling,
                                window.scale_factor(),
                            ) {
                                match window.paint_texture_in(image, bounds, viewport.bounds()) {
                                    Ok(()) => {
                                        if let Some(stats) = &performance {
                                            stats.borrow_mut().painted(
                                                Instant::now(),
                                                revision,
                                                dimensions,
                                            );
                                        }
                                    }
                                    Err(error) => {
                                        eprintln!("Could not draw guest display: {error}")
                                    }
                                }
                            }
                        },
                    )
                    .size_full(),
                )
            })
            .when(self.performance.is_some(), |surface| surface.child(self.performance_overlay(p)))
            .children(self.cursor_overlay())
            .when_some(self.status.clone(), |this, status| {
                this.child(
                    h_flex()
                        .absolute()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .px_6()
                        .text_color(p.muted)
                        .when(status == "Connecting", |t| t.child(Spinner::new().small()))
                        .child(div().min_w_0().max_w(px(480.)).child(status)),
                )
            })
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, cx| {
                if std::env::var_os("HYDROXL_DEBUG_POINTER").is_some() {
                    eprintln!(
                        "viewport {:?} scale {} fullscreen {}",
                        window.viewport_size(),
                        window.scale_factor(),
                        window.is_fullscreen()
                    );
                }
                if !this.input.locked() {
                    let over = this.over_guest(e.position);
                    if !over {
                        this.hover_suppressed = false;
                    }
                    if this.hover_capture()
                        && over
                        && !this.captured
                        && !this.hover_suppressed
                        && !this.toolbar_menu_open
                        && e.pressed_button.is_none()
                        && window.is_window_active()
                    {
                        this.capture(e.position, e.modifiers, window, cx);
                    } else if !over && this.captured && this.buttons == 0 && this.absolute_pointer()
                    {
                        // Leaving the guest image hands input back, so the toolbar and
                        // the rest of the desktop are always one move away.
                        this.release(window);
                        cx.notify();
                    }
                    this.send_pointer(e.position);
                }
                // Only the in-window overlay needs a redraw to follow the pointer.
                if this.captured && this.cursor.is_some() && this.system_cursor.is_none() {
                    cx.notify();
                }
            }))
            .on_mouse_exit(cx.listener(|this, _: &MouseExitEvent, window, cx| {
                // The pointer physically leaving the window hands input back in hover
                // mode. Hover state is not used: GPUI clears it on any key press.
                if this.captured && this.absolute_pointer() && !this.input.locked() {
                    this.release(window);
                    cx.notify();
                }
            }))
            .on_any_mouse_down(cx.listener(|this, e: &MouseDownEvent, window, cx| {
                this.focus.focus(window, cx);
                if this.session.is_some() && !this.captured {
                    // Only the guest image captures; the letterbox around it does not.
                    if (e.modifiers.control && e.modifiers.alt) || !this.over_guest(e.position) {
                        return;
                    }
                    // The capturing click only captures; it is not sent to the guest.
                    this.hover_suppressed = false;
                    this.capture(e.position, e.modifiers, window, cx);
                    return;
                }
                this.buttons |= Self::mask(e.button);
                this.send_pointer(e.position);
            }))
            .capture_any_mouse_up(cx.listener(|this, e: &MouseUpEvent, _, _| {
                if this.buttons & Self::mask(e.button) != 0 {
                    this.buttons &= !Self::mask(e.button);
                    this.send_pointer(e.position);
                }
            }))
            .on_scroll(cx.listener(|this, e: &ScrollEvent, window, _| {
                if !this.captured {
                    return;
                }
                let dy = f32::from(e.delta.pixel_delta(window.line_height()).y);
                if dy == 0. {
                    return;
                }
                if let (Some((x, y)), Some(s)) = (this.guest_point(e.position), &this.session) {
                    let buttons = this.buttons
                        | if dy > 0. {
                            8
                        } else {
                            16
                        };
                    if this.input.locked() {
                        s.relative_pointer(0, 0, buttons);
                        s.relative_pointer(0, 0, this.buttons);
                    } else {
                        s.pointer(x, y, buttons);
                        s.pointer(x, y, this.buttons);
                    }
                }
            }))
            .capture_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if !this.captured {
                    return;
                }
                if let Some(s) = &this.session {
                    if let Some(sym) = vnc::keysym(&e.keystroke.key) {
                        s.key(sym, true);
                    }
                    // The guest owns the keyboard while captured.
                    cx.stop_propagation();
                }
            }))
            .on_key_up(cx.listener(|this, e: &KeyUpEvent, _, _| {
                if !this.captured {
                    return;
                }
                if let (Some(s), Some(sym)) = (&this.session, vnc::keysym(&e.keystroke.key)) {
                    s.key(sym, false);
                }
            }))
            .on_modifiers_changed(cx.listener(|this, e: &ModifiersChangedEvent, window, cx| {
                let now = e.modifiers;
                if !this.captured {
                    this.modifiers = now;
                    return;
                }
                if now.control && now.alt {
                    this.release(window);
                    this.hover_suppressed = true;
                    this.modifiers = now;
                    cx.notify();
                    return;
                }
                let Some(s) = this.session.clone() else {
                    return;
                };
                let was = this.modifiers;
                let pairs = [
                    (was.shift, now.shift),
                    (was.control, now.control),
                    (was.alt, now.alt),
                    (was.platform, now.platform),
                ];
                for ((before, after), sym) in pairs.into_iter().zip(vnc::MODIFIERS) {
                    if before != after {
                        s.key(sym, after);
                    }
                }
                this.modifiers = now;
            }));

        let fullscreen = window.is_fullscreen();
        let show_toolbar =
            !fullscreen || self.toolbar_pinned || self.toolbar_revealed || self.toolbar_menu_open;
        v_flex()
            .relative()
            .size_full()
            .bg(p.canvas)
            .text_color(p.text)
            .when(!fullscreen, |root| root.child(self.top_bar(p, window, cx)))
            .child(surface)
            .when(fullscreen && show_toolbar, |root| {
                root.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .w_full()
                        .occlude()
                        .child(self.top_bar(p, window, cx)),
                )
            })
            .id("display-window")
            .capture_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if !view.captured && event.keystroke.key == "f11" && !event.is_held {
                    view.toolbar_revealed = false;
                    window.toggle_fullscreen();
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, window, cx| {
                // A fast move from the guest straight onto the toolbar skips the display's
                // own move events; release here so the toolbar takes the click.
                if view.captured
                    && view.buttons == 0
                    && !view.input.locked()
                    && view.absolute_pointer()
                    && !view.over_guest(event.position)
                {
                    view.release(window);
                    cx.notify();
                }
                if window.is_fullscreen() && !view.toolbar_pinned && !view.toolbar_menu_open {
                    let threshold = if view.toolbar_revealed {
                        TOOLBAR_HEIGHT + 8.
                    } else {
                        8.
                    };
                    let reveal = f32::from(event.position.y) <= threshold && !view.captured;
                    if reveal != view.toolbar_revealed {
                        view.toolbar_revealed = reveal;
                        cx.notify();
                    }
                }
            }))
    }
}

impl Workspace {
    /// Opens the selected guest's display in its own window, or brings an open one forward.
    pub(crate) fn open_display(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(machine) = self.selected_machine().cloned() else {
            return;
        };
        self.open_machine_display(machine, cx);
    }

    pub(crate) fn open_machine_display(&mut self, machine: Machine, cx: &mut Context<Self>) {
        if self.demo && std::env::var_os("HYDROXL_VNC_SOCKET").is_none() {
            self.notice = Some("The guest display is unavailable in preview.".into());
            cx.notify();
            return;
        }
        let connection = self.preferences.connection;
        self.display_windows.retain(|(_, _, view)| view.upgrade().is_some());
        if let Some((_, handle, view)) =
            self.display_windows.iter().find(|(id, _, _)| *id == machine.id)
        {
            let _ = handle.update(cx, |_, window, cx| {
                window.activate_window();
                if matches!(machine.state, State::Running | State::Paused) {
                    let _ = view.update(cx, |view, cx| view.ensure_session(window, cx));
                }
            });
            return;
        }
        if !self.opening_displays.insert(machine.id.clone()) {
            return;
        }
        let id = machine.id.clone();
        let workspace = cx.entity().downgrade();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1280.), px(844.)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some(machine.name.clone().into()),
                ..Default::default()
            }),
            app_id: Some("dev.hydroxl.Hydroxl".into()),
            ..Default::default()
        };
        let state = machine.state;
        let busy = self.busy();
        // Opened after this update: the new window renders immediately, outside the workspace.
        cx.defer(move |cx| {
            let opened = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    let mut view =
                        DisplayView::connect(connection, id, state, workspace.clone(), window, cx);
                    view.busy = busy;
                    view
                })
            });
            let _ = workspace.update(cx, |this, cx| {
                this.opening_displays.remove(&machine.id);
                match opened {
                    Ok((handle, view)) => {
                        this.display_windows.push((machine.id, handle, view.downgrade()));
                    }
                    Err(error) => {
                        this.error = Some(format!("Could not open the display window: {error}"))
                    }
                }
                cx.notify();
            });
        });
    }
}
