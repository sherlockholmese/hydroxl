use super::DisplayView;
use crate::display::vnc;
use gpui_kit::{prelude::*, *};
use std::sync::Arc;

impl DisplayView {
    /// Guest framebuffer coordinates for a logical window position, letterboxing included.
    pub(super) fn guest_point(&self, position: Point<Pixels>) -> Option<(u16, u16)> {
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

    pub(super) fn send_pointer(&mut self, position: Point<Pixels>) {
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

    pub(super) fn move_relative_cursor(&mut self, dx: i32, dy: i32) {
        let (Some(pointer), Some(viewport)) = (self.pointer, self.viewport()) else {
            return;
        };
        let (x, y) = viewport.move_pointer(f32::from(pointer.x), f32::from(pointer.y), dx, dy);
        self.pointer = Some(point(px(x), px(y)));
    }

    /// Hands the guest cursor to the platform while input is captured with a free
    /// pointer. A locked pointer stays put, so that case keeps the in-window overlay.
    pub(super) fn sync_system_cursor(&mut self, window: &mut Window) {
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
    pub(super) fn cursor_overlay(&self) -> Option<AnyElement> {
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

    pub(super) fn mask(button: MouseButton) -> u8 {
        match button {
            MouseButton::Left => 1,
            MouseButton::Middle => 2,
            MouseButton::Right => 4,
            _ => 0,
        }
    }

    /// Hands keyboard and pointer back to the host, releasing anything held in the guest.
    pub(super) fn release(&mut self, window: &mut Window) {
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
    pub(super) fn capture(
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
        self.sync_clipboard(&session, true, window, cx);
        cx.notify();
    }

    /// Whether a window position is over the guest image (not the letterbox around it).
    pub(super) fn over_guest(&self, position: Point<Pixels>) -> bool {
        self.viewport().is_some_and(|viewport| viewport.bounds().contains(&position))
    }

    /// Whether moving over the guest image captures input. Needs an absolute pointer:
    /// a relative one is locked to the window and could never leave again.
    pub(super) fn hover_capture(&self) -> bool {
        !self.capture_on_click && self.absolute_pointer()
    }

    /// The guest follows the host pointer's position (a tablet), so the pointer can
    /// leave the guest image and release input in either capture mode.
    pub(super) fn absolute_pointer(&self) -> bool {
        self.session.as_ref().is_some_and(|session| {
            session.frame().lock().unwrap_or_else(|e| e.into_inner()).absolute != Some(false)
        })
    }

    pub(super) fn set_capture_mode(&mut self, on_click: bool, cx: &mut Context<Self>) {
        self.capture_on_click = on_click;
        self.hover_suppressed = false;
        let _ = self.workspace.update(cx, |workspace, _| {
            workspace.preferences.capture_on_click = on_click;
            workspace.save_preferences();
        });
        cx.notify();
    }
}
