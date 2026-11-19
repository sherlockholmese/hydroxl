// Guest-input extensions, kept separate to make upstream rebases reviewable.
use wayland_protocols::wp::{
    keyboard_shortcuts_inhibit::zv1::client::{
        zwp_keyboard_shortcuts_inhibit_manager_v1, zwp_keyboard_shortcuts_inhibitor_v1,
    },
    pointer_constraints::zv1::client::{zwp_locked_pointer_v1, zwp_pointer_constraints_v1},
    relative_pointer::zv1::client::{zwp_relative_pointer_manager_v1, zwp_relative_pointer_v1},
};

#[derive(Default)]
struct WaylandCapture {
    hidden: bool,
    /// Shown instead of nothing while hidden, so the compositor moves the guest's cursor.
    image: Option<gpui::CursorImage>,
    locked: bool,
    lock: Option<zwp_locked_pointer_v1::ZwpLockedPointerV1>,
    relative: Option<zwp_relative_pointer_v1::ZwpRelativePointerV1>,
    inhibitor: Option<zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1>,
    delta: Point<f64>,
}
impl WaylandCapture {
    fn unlock(&mut self) {
        if let Some(relative) = self.relative.take() {
            relative.destroy();
        }
        if let Some(lock) = self.lock.take() {
            lock.destroy();
        }
        self.locked = false;
        self.delta = Point::default();
    }
    fn uninhibit(&mut self) {
        if let Some(inhibitor) = self.inhibitor.take() {
            inhibitor.destroy();
        }
    }
}
/// The surface that shows a guest cursor image as the pointer's cursor.
struct GuestCursorSurface {
    surface: wl_surface::WlSurface,
    buffer: Option<wl_buffer::WlBuffer>,
    /// What the buffer holds: image identity and scales, so it is redrawn only on change.
    drawn: Option<(usize, usize, u32, i32)>,
    hot: (i32, i32),
}

impl Drop for GuestCursorSurface {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            buffer.destroy();
        }
        self.surface.destroy();
    }
}

impl WaylandClientState {
    /// Applies a capture's hidden cursor: its guest image when one is set, else none.
    fn hide_captured_cursor(&mut self, serial: u32) {
        let Some(pointer) = self.wl_pointer.clone() else {
            return;
        };
        let focused = self.mouse_focused_window.clone();
        let image = focused
            .as_ref()
            .and_then(|window| self.captures.get(&window.surface_id()))
            .and_then(|capture| capture.image.clone());
        let shown = match (image, focused) {
            (Some(image), Some(window)) => {
                self.draw_guest_cursor(&image, window.primary_output_scale())
            }
            _ => None,
        };
        match shown {
            Some((surface, hot_x, hot_y)) => pointer.set_cursor(serial, Some(&surface), hot_x, hot_y),
            None => pointer.set_cursor(serial, None, 0, 0),
        }
    }

    fn draw_guest_cursor(
        &mut self,
        image: &gpui::CursorImage,
        scale: i32,
    ) -> Option<(wl_surface::WlSurface, i32, i32)> {
        use std::io::Write as _;
        use std::os::fd::{AsFd as _, FromRawFd as _, OwnedFd};
        let scale = scale.max(1);
        let key = (image.bgra.as_ptr() as usize, image.bgra.len(), image.scale.to_bits(), scale);
        let globals = &self.globals;
        let cursor = self.guest_cursor.get_or_insert_with(|| GuestCursorSurface {
            surface: globals.compositor.create_surface(&globals.qh, ()),
            buffer: None,
            drawn: None,
            hot: (0, 0),
        });
        if cursor.drawn != Some(key) {
            let scaled = crate::linux::cursor_image::scale_cursor(image, scale as f32)?;
            // A buffer scale needs dimensions divisible by it; pad with transparency.
            let pad = |value: u32| value.div_ceil(scale as u32) * scale as u32;
            let (width, height) = (pad(scaled.width), pad(scaled.height));
            let mut pixels = vec![0u8; (width * height * 4) as usize];
            for (row, source) in scaled.bgra.chunks_exact(scaled.width as usize * 4).enumerate() {
                let start = row * width as usize * 4;
                pixels[start..start + source.len()].copy_from_slice(source);
            }
            let fd = unsafe { libc::memfd_create(c"gpui-guest-cursor".as_ptr(), libc::MFD_CLOEXEC) };
            if fd < 0 {
                return None;
            }
            let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
            file.write_all(&pixels).ok()?;
            let pool = globals.shm.create_pool(file.as_fd(), pixels.len() as i32, &globals.qh, ());
            let buffer = pool.create_buffer(
                0,
                width as i32,
                height as i32,
                width as i32 * 4,
                wl_shm::Format::Argb8888,
                &globals.qh,
                (),
            );
            pool.destroy();
            cursor.surface.set_buffer_scale(scale);
            cursor.surface.attach(Some(&buffer), 0, 0);
            cursor.surface.damage(0, 0, i32::MAX, i32::MAX);
            cursor.surface.commit();
            if let Some(previous) = cursor.buffer.replace(buffer) {
                previous.destroy();
            }
            cursor.hot = (scaled.hot_x as i32 / scale, scaled.hot_y as i32 / scale);
            cursor.drawn = Some(key);
        }
        Some((cursor.surface.clone(), cursor.hot.0, cursor.hot.1))
    }

    fn restore_captured_cursor(&mut self, surface: &ObjectId) {
        if self.mouse_focused_window.as_ref().is_some_and(|window| window.surface_id() == *surface)
        {
            self.cursor_hidden_window = self.mouse_focused_window.clone();
            self.restore_cursor_after_hide();
        }
    }

    fn capture_hides_cursor(&self) -> bool {
        self.mouse_focused_window.as_ref().is_some_and(|window| {
            self.captures.get(&window.surface_id()).is_some_and(|capture| capture.hidden)
        })
    }
    fn release_all_captures(&mut self) {
        let ids: Vec<_> = self.captures.keys().cloned().collect();
        for id in ids {
            self.release_capture(&id);
        }
    }
    fn release_capture(&mut self, id: &ObjectId) {
        if let Some(mut capture) = self.captures.remove(id) {
            capture.unlock();
            capture.uninhibit();
            if capture.hidden {
                self.restore_captured_cursor(id);
            }
        }
    }
}
impl WaylandClientStatePtr {
    pub(super) fn release_guest_capture(&self, id: &ObjectId) {
        if let Some(client) = self.try_get_client() {
            client.borrow_mut().release_capture(id);
        }
    }
    pub(super) fn capture_cursor(
        &self,
        surface: &wl_surface::WlSurface,
        hidden: bool,
    ) -> anyhow::Result<()> {
        let client = self.get_client();
        let mut state = client.borrow_mut();
        if hidden {
            anyhow::ensure!(
                state.mouse_focused_window.as_ref().is_some_and(|w| w.surface_id() == surface.id()),
                "pointer is outside this window"
            );
            state.wl_pointer.as_ref().context("seat has no pointer")?;
            state.cursor_style.get_or_insert(CursorStyle::Arrow);
            let focused = state.mouse_focused_window.clone();
            state.cursor_hidden_window = focused;
            state.captures.entry(surface.id()).or_default().hidden = true;
            let serial = state.serial_tracker.get(SerialKind::MouseEnter).as_raw();
            state.hide_captured_cursor(serial);
        } else if let Some(capture) = state.captures.get_mut(&surface.id())
            && std::mem::take(&mut capture.hidden)
        {
            state.restore_captured_cursor(&surface.id());
        }
        Ok(())
    }
    pub(super) fn capture_cursor_image(
        &self,
        surface: &wl_surface::WlSurface,
        image: Option<gpui::CursorImage>,
    ) -> anyhow::Result<()> {
        let client = self.get_client();
        let mut state = client.borrow_mut();
        let capture = state.captures.entry(surface.id()).or_default();
        capture.image = image;
        let hidden = capture.hidden;
        let focused = state
            .mouse_focused_window
            .as_ref()
            .is_some_and(|window| window.surface_id() == surface.id());
        if hidden && focused {
            let serial = state.serial_tracker.get(SerialKind::MouseEnter).as_raw();
            state.hide_captured_cursor(serial);
        }
        Ok(())
    }
    pub(super) fn capture_pointer(
        &self,
        surface: &wl_surface::WlSurface,
        lock: bool,
    ) -> anyhow::Result<()> {
        let client = self.get_client();
        let mut state = client.borrow_mut();
        if !lock {
            if let Some(capture) = state.captures.get_mut(&surface.id()) {
                capture.unlock();
            }
            return Ok(());
        }
        if state.captures.get(&surface.id()).is_some_and(|c| c.lock.is_some()) {
            return Ok(());
        }
        anyhow::ensure!(
            state.mouse_focused_window.as_ref().is_some_and(|w| w.surface_id() == surface.id()),
            "pointer is outside this window"
        );
        let pointer = state.wl_pointer.as_ref().context("seat has no pointer")?;
        let constraints = state
            .globals
            .pointer_constraints
            .as_ref()
            .context("compositor does not support pointer lock")?;
        let manager = state
            .globals
            .relative_pointer_manager
            .as_ref()
            .context("compositor does not support relative motion")?;
        let relative = manager.get_relative_pointer(pointer, &state.globals.qh, surface.id());
        let locked = constraints.lock_pointer(
            surface,
            pointer,
            None,
            zwp_pointer_constraints_v1::Lifetime::Persistent,
            &state.globals.qh,
            surface.id(),
        );
        let capture = state.captures.entry(surface.id()).or_default();
        capture.lock = Some(locked);
        capture.relative = Some(relative);
        capture.delta = Point::default();
        Ok(())
    }
    pub(super) fn capture_locked(&self, surface: &wl_surface::WlSurface) -> bool {
        self.get_client().borrow().captures.get(&surface.id()).is_some_and(|c| c.locked)
    }
    pub(super) fn capture_motion(&self, surface: &wl_surface::WlSurface) -> Point<f64> {
        self.get_client()
            .borrow_mut()
            .captures
            .get_mut(&surface.id())
            .map(|c| std::mem::take(&mut c.delta))
            .unwrap_or_default()
    }
    pub(super) fn capture_shortcuts(
        &self,
        surface: &wl_surface::WlSurface,
        inhibit: bool,
    ) -> anyhow::Result<()> {
        let client = self.get_client();
        let mut state = client.borrow_mut();
        if !inhibit {
            if let Some(capture) = state.captures.get_mut(&surface.id()) {
                capture.uninhibit();
            }
            return Ok(());
        }
        if state.captures.get(&surface.id()).is_some_and(|c| c.inhibitor.is_some()) {
            return Ok(());
        }
        anyhow::ensure!(
            state.keyboard_focused_window.as_ref().is_some_and(|w| w.surface_id() == surface.id()),
            "window has no keyboard focus"
        );
        let manager = state
            .globals
            .shortcuts_manager
            .as_ref()
            .context("compositor does not support shortcut inhibition")?;
        let inhibitor = manager.inhibit_shortcuts(surface, &state.wl_seat, &state.globals.qh, ());
        state.captures.entry(surface.id()).or_default().inhibitor = Some(inhibitor);
        Ok(())
    }
}

wayland_client::delegate_noop!(WaylandClientStatePtr: ignore zwp_pointer_constraints_v1::ZwpPointerConstraintsV1);
wayland_client::delegate_noop!(WaylandClientStatePtr: ignore zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1);
wayland_client::delegate_noop!(WaylandClientStatePtr: ignore zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1);
wayland_client::delegate_noop!(WaylandClientStatePtr: ignore zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1);

impl Dispatch<zwp_locked_pointer_v1::ZwpLockedPointerV1, ObjectId> for WaylandClientStatePtr {
    fn event(
        this: &mut Self,
        proxy: &zwp_locked_pointer_v1::ZwpLockedPointerV1,
        event: zwp_locked_pointer_v1::Event,
        id: &ObjectId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let client = this.get_client();
        let mut state = client.borrow_mut();
        if let Some(capture) = state.captures.get_mut(id) {
            // Ignore events queued before an unlock/relock.
            if capture.lock.as_ref() != Some(proxy) {
                return;
            }
            capture.locked = matches!(event, zwp_locked_pointer_v1::Event::Locked);
            capture.delta = Point::default();
        }
    }
}
impl Dispatch<zwp_relative_pointer_v1::ZwpRelativePointerV1, ObjectId> for WaylandClientStatePtr {
    fn event(
        this: &mut Self,
        proxy: &zwp_relative_pointer_v1::ZwpRelativePointerV1,
        event: zwp_relative_pointer_v1::Event,
        id: &ObjectId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let zwp_relative_pointer_v1::Event::RelativeMotion {
            dx_unaccel,
            dy_unaccel,
            ..
        } = event
        else {
            return;
        };
        let client = this.get_client();
        let mut state = client.borrow_mut();
        if let Some(capture) = state.captures.get_mut(id) {
            if capture.locked && capture.relative.as_ref() == Some(proxy) {
                capture.delta.x += dx_unaccel;
                capture.delta.y += dy_unaccel;
            }
        }
    }
}
