use super::vnc::{self, Framebuffer};
use super::{
    dbus,
    spice::{self, Input},
};
use crate::backend::{Connection, SpiceOptions};
use anyhow::Result;
use std::sync::{Arc, Mutex, mpsc};

pub(crate) enum Session {
    Vnc(Arc<vnc::Session>),
    Spice(spice::Session),
    Dbus(dbus::Session),
}

#[derive(Clone, Copy)]
pub(crate) enum Resize {
    Vnc(vnc::ResizeScreen),
    Spice,
    Dbus,
}

impl Session {
    pub(crate) fn vnc(endpoint: &vnc::Endpoint) -> Result<Arc<Self>> {
        Ok(Arc::new(Self::Vnc(vnc::Session::connect(endpoint)?)))
    }

    pub(crate) fn spice(
        connection: Connection,
        id: String,
        options: SpiceOptions,
        gpu: bool,
    ) -> Result<Arc<Self>> {
        Ok(Arc::new(Self::Spice(spice::Session::connect(connection, id, options, gpu)?)))
    }

    pub(crate) fn dbus(
        connection: Connection,
        id: &str,
        index: u32,
        gpu: bool,
        clipboard: bool,
        usb: bool,
    ) -> Result<Arc<Self>> {
        Ok(Arc::new(Self::Dbus(dbus::Session::connect(
            connection, id, index, gpu, clipboard, usb,
        )?)))
    }

    pub(crate) fn frame(&self) -> &Mutex<Framebuffer> {
        match self {
            Self::Vnc(session) => &session.frame,
            Self::Spice(session) => &session.frame,
            Self::Dbus(session) => &session.frame,
        }
    }

    /// Software SPICE snapshots are copied on demand by the decoding thread.
    pub(crate) fn request_frame(&self) {
        if let Self::Spice(session) = self {
            session.request_frame();
        }
    }

    pub(crate) fn close(&self) {
        match self {
            Self::Vnc(session) => session.close(),
            Self::Spice(session) => session.close(),
            Self::Dbus(session) => session.close(),
        }
    }

    pub(crate) fn resize_desktop(&self, size: (u16, u16), resize: Resize) -> Result<()> {
        match (self, resize) {
            (Self::Vnc(session), Resize::Vnc(screen)) => session.resize_desktop(size, screen)?,
            (Self::Spice(session), Resize::Spice) => session.send(Input::Resize(size.0, size.1)),
            (Self::Dbus(session), Resize::Dbus) => session.send(Input::Resize(size.0, size.1)),
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn key(&self, symbol: u32, down: bool) {
        match self {
            Self::Vnc(session) => session.key(symbol, down),
            Self::Spice(session) => session.send(Input::Key(symbol, down)),
            Self::Dbus(session) => session.send(Input::Key(symbol, down)),
        }
    }

    pub(crate) fn combo(&self, keys: &[u32]) {
        for key in keys {
            self.key(*key, true);
        }
        for key in keys.iter().rev() {
            self.key(*key, false);
        }
    }

    pub(crate) fn pointer(&self, x: u16, y: u16, buttons: u8) {
        match self {
            Self::Vnc(session) => session.pointer(x, y, buttons),
            Self::Spice(session) => session.send(Input::Pointer(x, y, buttons)),
            Self::Dbus(session) => session.send(Input::Pointer(x, y, buttons)),
        }
    }

    pub(crate) fn relative_pointer(&self, dx: i32, dy: i32, buttons: u8) {
        match self {
            Self::Vnc(session) => session.relative_pointer(dx, dy, buttons),
            Self::Spice(session) => session.send(Input::Relative(dx, dy, buttons)),
            Self::Dbus(session) => session.send(Input::Relative(dx, dy, buttons)),
        }
    }

    pub(crate) fn reset_pointer(&self) {
        match self {
            Self::Vnc(session) => session.reset_pointer(),
            Self::Spice(session) => session.send(Input::ResetPointer),
            Self::Dbus(session) => session.send(Input::ResetPointer),
        }
    }

    pub(crate) fn release_input(&self) {
        match self {
            Self::Vnc(session) => session.release_input(),
            Self::Spice(session) => session.send(Input::Release),
            Self::Dbus(session) => session.send(Input::Release),
        }
    }

    pub(crate) fn can_toggle_usb(&self) -> bool {
        match self {
            Self::Spice(session) => {
                session.frame.lock().unwrap_or_else(|e| e.into_inner()).spice_usb
            }
            Self::Dbus(session) => session.can_toggle_usb(),
            Self::Vnc(_) => false,
        }
    }

    pub(crate) fn toggle_usb(&self, bus: u8, address: u8) -> Result<mpsc::Receiver<Result<bool>>> {
        match self {
            Self::Spice(session) => {
                let (result, rx) = mpsc::channel();
                session.send(Input::Usb {
                    bus,
                    address,
                    result,
                });
                Ok(rx)
            }
            Self::Dbus(session) => session.toggle_usb(bus, address),
            Self::Vnc(_) => anyhow::bail!("USB redirection is unavailable for this display."),
        }
    }

    pub(crate) fn send_clipboard(&self, text: String) {
        match self {
            Self::Spice(session) => session.send(Input::SendClipboard(text)),
            Self::Vnc(session) => session.offer_clipboard(text),
            Self::Dbus(session) => session.send_clipboard(text),
        }
    }

    /// Asks a VNC guest for its clipboard; the text arrives in the frame's `clipboard_text`.
    pub(crate) fn request_vnc_clipboard(&self) -> Result<()> {
        match self {
            Self::Vnc(session) => session.request_clipboard(),
            _ => anyhow::bail!("Clipboard requests over VNC only."),
        }
    }

    pub(crate) fn read_clipboard(&self) -> Result<mpsc::Receiver<Result<String>>> {
        match self {
            Self::Spice(session) => {
                let (result, rx) = mpsc::channel();
                session.send(Input::ReadClipboard(result));
                Ok(rx)
            }
            Self::Dbus(session) => session.read_clipboard(),
            Self::Vnc(_) => anyhow::bail!("Use a VNC clipboard request."),
        }
    }
}
