use super::{Frame, Peer};
use anyhow::{Context, Result, ensure};
use std::sync::{Arc, Mutex, mpsc};
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
        let control = control.clone();
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(command) = receive.recv() {
                match command {
                    Command::Offer(text) => {
                        if let Err(error) = self.offer(&control, text) {
                            eprintln!("Could not offer text to the QEMU clipboard: {error:#}");
                        }
                    }
                    Command::Read(result) => {
                        let _ = result.send(self.read(&control));
                    }
                    Command::Close => break,
                }
            }
        });
        Ok(send)
    }

    fn offer(&self, control: &Peer, text: String) -> Result<()> {
        ensure!(text.len() <= MAX_TEXT_BYTES, "Clipboard text is too large.");
        let serial = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.serial = state.serial.wrapping_add(1);
            state.revision = state.revision.wrapping_add(1);
            state.owner = Owner::Host(text);
            state.serial
        };
        control.call_method(
            None::<&str>,
            PATH,
            Some(INTERFACE),
            "Grab",
            &(SELECTION, serial, [MIME].as_slice()),
        )?;
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
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        ensure!(state.revision == revision, "The clipboard changed during the request.");
        Ok(std::str::from_utf8(bytes)?.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        if mimes.iter().any(|mime| mime == MIME) {
            let mut frame = self.frame.lock().unwrap_or_else(|error| error.into_inner());
            frame.guest_clipboard = frame.guest_clipboard.wrapping_add(1);
        }
    }

    fn release(&self, selection: u32) {
        if selection == SELECTION {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if matches!(state.owner, Owner::Guest) {
                state.owner = Owner::None;
                state.revision = state.revision.wrapping_add(1);
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
