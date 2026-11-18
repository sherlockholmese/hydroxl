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
impl WaylandClientState {
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
            let pointer = state.wl_pointer.as_ref().context("seat has no pointer")?;
            pointer.set_cursor(
                state.serial_tracker.get(SerialKind::MouseEnter).as_raw(),
                None,
                0,
                0,
            );
            state.cursor_style.get_or_insert(CursorStyle::Arrow);
            let focused = state.mouse_focused_window.clone();
            state.cursor_hidden_window = focused;
            state.captures.entry(surface.id()).or_default().hidden = true;
        } else if let Some(capture) = state.captures.get_mut(&surface.id())
            && std::mem::take(&mut capture.hidden)
        {
            state.restore_captured_cursor(&surface.id());
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
