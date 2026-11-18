//! Optional platform input capture, without changing the shared event enums.
use super::*;

impl Window {
    /// Overrides the application's automatic cursor hiding policy for this window.
    /// Explicit capture through `set_cursor_hidden` is independent of this policy.
    pub fn set_cursor_hide_mode_override(&mut self, mode: Option<CursorHideMode>) {
        self.cursor_hide_mode_override = mode;
    }

    /// Hides the host cursor until explicitly restored, focus loss, or window destruction.
    pub fn set_cursor_hidden(&mut self, hidden: bool) -> Result<()> {
        self.platform_window.set_cursor_hidden(hidden)
    }

    /// Requests pointer lock; use `is_pointer_locked` to check compositor activation.
    pub fn lock_pointer(&mut self, lock: bool) -> Result<()> {
        self.platform_window.lock_pointer(lock)
    }

    /// Reports actual server activation, rather than whether a request was accepted.
    pub fn is_pointer_locked(&self) -> bool {
        self.platform_window.is_pointer_locked()
    }

    /// Drains unaccelerated motion accumulated since the last call. Device units are
    /// independent of logical coordinates and display scale; do not scale them again.
    /// Poll once per frame, including frames with no framebuffer damage.
    pub fn take_raw_mouse_motion(&mut self) -> Point<f64> {
        self.platform_window.take_raw_mouse_motion()
    }

    /// Inhibits host shortcuts where supported; compositor policy remains authoritative.
    pub fn inhibit_system_shortcuts(&mut self, inhibit: bool) -> Result<()> {
        self.platform_window.inhibit_system_shortcuts(inhibit)
    }
}
