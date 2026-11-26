use super::{Frame, Peer};
use anyhow::{Context, Result, ensure};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use zbus::fdo;

pub(super) const PATH: &str = "/org/qemu/Display1/Clipboard";
const INTERFACE: &str = "org.qemu.Display1.Clipboard";
const SELECTION: u32 = 0;
const MIME: &str = "text/plain;charset=utf-8";
const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;

pub(super) enum Command {
    Offer(String),
    Read(mpsc::Sender<Result<String>>),
    Close,
}

#[derive(Default)]
enum Owner {
    #[default]
    None,
    Host(String),
    Guest,
}

#[derive(Default)]
struct State {
    serial: u32,
    revision: u64,
    owner: Owner,
}

#[derive(Clone)]
pub(super) struct Clipboard {
    frame: Frame,
    state: Arc<Mutex<State>>,
}

impl Clipboard {
    pub(super) fn new(frame: Frame) -> Self {
        Self {
            frame,
            state: Arc::default(),
        }
    }

    pub(super) fn connect(self, control: &Peer) -> Result<mpsc::Sender<Command>> {
        control.call_method(None::<&str>, PATH, Some(INTERFACE), "Register", &())?;
        self.frame.lock().unwrap_or_else(|error| error.into_inner()).dbus_clipboard = true;
        let frame = self.frame.clone();
        let worker_control = control.clone();
        let (send, receive) = mpsc::channel();
        let worker = std::thread::Builder::new().name("dbus-clipboard".into()).spawn(move || {
            let control = worker_control;
            loop {
                match receive.recv_timeout(Duration::from_secs(1)) {
                    Ok(Command::Offer(text)) => {
                        if let Err(error) = self.offer(&control, text) {
                            eprintln!("Could not offer text to the QEMU clipboard: {error:#}");
                        }
                    }
                    Ok(Command::Read(result)) => {
                        let _ = result.send(self.read(&control));
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // QEMU omits Grab notifications for guest agents without grab serials.
                        let _ = self.read(&control);
                    }
                    Ok(Command::Close) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            let _ = control.call_method(None::<&str>, PATH, Some(INTERFACE), "Unregister", &());
        });
        if let Err(error) = worker {
            frame.lock().unwrap_or_else(|error| error.into_inner()).dbus_clipboard = false;
            let _ = control.call_method(None::<&str>, PATH, Some(INTERFACE), "Unregister", &());
            return Err(error).context("Could not start the QEMU clipboard client.");
        }
        Ok(send)
    }

    fn offer(&self, control: &Peer, text: String) -> Result<()> {
        ensure!(text.len() <= MAX_TEXT_BYTES, "Clipboard text is too large.");
        let (serial, revision) = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.serial = state.serial.wrapping_add(1);
            state.revision = state.revision.wrapping_add(1);
            state.owner = Owner::Host(text);
            self.frame
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .guest_clipboard_changed(false);
            (state.serial, state.revision)
        };
        let result = control.call_method(
            None::<&str>,
            PATH,
            Some(INTERFACE),
            "Grab",
            &(SELECTION, serial, [MIME].as_slice()),
        );
        if let Err(error) = result {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.revision == revision {
                state.owner = Owner::None;
            }
            return Err(error.into());
        }
        Ok(())
    }

    fn read(&self, control: &Peer) -> Result<String> {
        let revision = self.state.lock().unwrap_or_else(|error| error.into_inner()).revision;
        let reply = control
            .call_method(
                None::<&str>,
                PATH,
                Some(INTERFACE),
                "Request",
                &(SELECTION, [MIME].as_slice()),
            )
            .context("Could not read the guest clipboard.")?;
        let body = reply.body();
        let (mime, bytes): (&str, &[u8]) = body.deserialize()?;
        ensure!(mime == MIME && bytes.len() <= MAX_TEXT_BYTES, "Invalid clipboard text.");
        let text = std::str::from_utf8(bytes)?.to_owned();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        ensure!(state.revision == revision, "The clipboard changed during the request.");
        state.owner = Owner::Guest;
        let mut frame = self.frame.lock().unwrap_or_else(|error| error.into_inner());
        if frame.clipboard_text.as_ref() != Some(&text) {
            frame.received_clipboard(text.clone());
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::{blocking::connection::Builder, connection::socket::Channel};

    struct GuestClipboard;

    #[zbus::interface(name = "org.qemu.Display1.Clipboard")]
    impl GuestClipboard {
        #[zbus(out_args("mime", "data"))]
        fn request(&self, selection: u32, mimes: Vec<String>) -> (&'static str, Vec<u8>) {
            assert_eq!(selection, SELECTION);
            assert_eq!(mimes, [MIME]);
            (MIME, "D-Bus café\n".as_bytes().to_vec())
        }
    }

    #[test]
    fn accepts_qemus_utf8_reply_and_caches_unannounced_text() -> Result<()> {
        let (client, server) = Channel::pair();
        let guid = zbus::Guid::generate();
        let _server = Builder::authenticated_socket(server, guid.clone())?
            .p2p()
            .serve_at(PATH, GuestClipboard)?
            .build()?;
        let client = Builder::authenticated_socket(client, guid)?
            .p2p()
            .method_timeout(Duration::from_secs(2))
            .build()?;
        let frame = Frame::default();
        let clipboard = Clipboard::new(frame.clone());
        assert_eq!(clipboard.read(&client)?, "D-Bus café\n");
        assert_eq!(clipboard.read(&client)?, "D-Bus café\n");
        let frame = frame.lock().unwrap();
        assert_eq!(frame.clipboard_text.as_deref(), Some("D-Bus café\n"));
        assert_eq!(frame.guest_clipboard, 1);
        Ok(())
    }

    #[test]
    fn accepts_newer_guest_grabs_and_preserves_host_ownership_on_a_tie() -> Result<()> {
        let frame = Frame::default();
        let clipboard = Clipboard::new(frame.clone());
        {
            let mut state = clipboard.state.lock().unwrap();
            state.serial = 2;
            state.owner = Owner::Host("héllo".into());
        }
        clipboard.grab(SELECTION, 2, vec![MIME.into()]);
        assert_eq!(frame.lock().unwrap().guest_clipboard, 0);
        let (mime, data) = clipboard.request(SELECTION, vec![MIME.into()])?;
        assert_eq!((mime, data.as_slice()), (MIME, "héllo".as_bytes()));
        clipboard.grab(SELECTION, 3, vec![MIME.into()]);
        assert_eq!(frame.lock().unwrap().guest_clipboard, 1);
        assert!(clipboard.request(SELECTION, vec![MIME.into()]).is_err());
        Ok(())
    }
}

#[zbus::interface(name = "org.qemu.Display1.Clipboard", spawn = false)]
impl Clipboard {
    #[zbus(property)]
    fn interfaces(&self) -> Vec<&'static str> {
        Vec::new()
    }

    fn register(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.serial = 0;
        let mut frame = self.frame.lock().unwrap_or_else(|error| error.into_inner());
        frame.clipboard_reset = frame.clipboard_reset.wrapping_add(1);
    }

    fn unregister(&self) {
        self.release(SELECTION);
    }

    fn grab(&self, selection: u32, serial: u32, mimes: Vec<String>) {
        if selection != SELECTION {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if serial < state.serial
            || (serial == state.serial && matches!(state.owner, Owner::Host(_)))
        {
            return;
        }
        state.serial = serial;
        state.revision = state.revision.wrapping_add(1);
        state.owner = Owner::Guest;
        let has_text = mimes.iter().any(|mime| mime == MIME);
        self.frame
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .guest_clipboard_changed(has_text);
    }

    fn release(&self, selection: u32) {
        if selection == SELECTION {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if matches!(state.owner, Owner::Guest) {
                state.owner = Owner::None;
                state.revision = state.revision.wrapping_add(1);
                self.frame
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .guest_clipboard_changed(false);
            }
        }
    }

    #[zbus(out_args("reply_mime", "data"))]
    fn request(&self, selection: u32, mimes: Vec<String>) -> fdo::Result<(&'static str, Vec<u8>)> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if selection == SELECTION
            && mimes.iter().any(|mime| mime == MIME)
            && let Owner::Host(text) = &state.owner
        {
            return Ok((MIME, text.as_bytes().to_vec()));
        }
        Err(fdo::Error::Failed("No matching host clipboard text.".into()))
    }
}
