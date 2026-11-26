//! Shared frame data published by display transports and consumed by the viewer.
use super::{damage::Damage, vnc::ResizeScreen};

pub(super) const MAX_DESKTOP_SIZE: usize = 8192;

/// The guest framebuffer, BGRA, plus what changed since the UI last looked.
#[derive(Default)]
pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub(super) damage: Vec<Damage>,
    /// Incremented on every applied update; the UI uploads when it differs.
    pub generation: u64,
    pub closed: Option<String>,
    pub display_error: Option<String>,
    /// Guest cursor shape, drawn by the client at the host pointer. `None` hides it.
    pub cursor: Option<Cursor>,
    /// Incremented when the cursor shape changes.
    pub cursor_generation: u64,
    /// Whether QEMU routes pointer events absolutely (a tablet is active in the guest).
    pub absolute: Option<bool>,
    /// A single screen that can be addressed by a SetDesktopSize request.
    pub resize_screen: Option<ResizeScreen>,
    pub spice_resize: bool,
    pub dbus_resize: bool,
    pub spice_usb: bool,
    pub spice_clipboard: bool,
    pub dbus_clipboard: bool,
    /// QEMU's VNC clipboard extension is active (guest agent through qemu-vdagent).
    pub vnc_clipboard: bool,
    /// Latest text received from the guest.
    pub clipboard_text: Option<String>,
    /// Changes each time the guest claims or releases the clipboard.
    pub guest_clipboard: u64,
    pub guest_clipboard_has_text: bool,
    /// The agent reset its clipboard state and needs the current host offer again.
    pub clipboard_reset: u64,
    pub(crate) gpu_draw: Option<super::scanout::GpuDraw>,
    /// Wakes the UI the moment a GPU draw is offered, instead of at its next vsync.
    pub(crate) draw_waker: Option<std::task::Waker>,
    pub(crate) preview_requested: bool,
    pub(crate) thumbnail: Option<image::RgbaImage>,
}

impl Framebuffer {
    pub(crate) fn shares_clipboard(&self) -> bool {
        self.spice_clipboard || self.vnc_clipboard || self.dbus_clipboard
    }

    pub(crate) fn guest_clipboard_changed(&mut self, has_text: bool) {
        self.guest_clipboard = self.guest_clipboard.wrapping_add(1);
        self.guest_clipboard_has_text = has_text;
        self.clipboard_text = None;
    }

    pub(crate) fn received_clipboard(&mut self, text: String) {
        self.guest_clipboard_changed(true);
        self.clipboard_text = Some(text);
    }
}

/// A cursor bitmap in BGRA with alpha from the RFB mask, and its hotspot.
#[derive(Clone)]
pub struct Cursor {
    pub width: u32,
    pub height: u32,
    pub hot_x: u32,
    pub hot_y: u32,
    pub pixels: Vec<u8>,
}
