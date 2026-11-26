//! VNC extended clipboard negotiation and guest/host ownership.
use super::Session;
use anyhow::{Result, bail};
use std::io::Read;

#[derive(Default)]
pub(super) struct Clipboard {
    /// Host text offered to the guest, sent when the server asks for it.
    offer: Option<String>,
    requested: bool,
}

impl Session {
    /// Offers host clipboard text to the guest. QEMU asks for the data when the guest
    /// pastes, so large text is only transferred when it is used.
    pub fn offer_clipboard(&self, text: String) {
        let mut clipboard = self.clipboard.lock().unwrap_or_else(|e| e.into_inner());
        let mut frame = self.frame.lock().unwrap_or_else(|e| e.into_inner());
        if !frame.vnc_clipboard || text.len() > MAX_CLIPBOARD_TEXT {
            return;
        }
        frame.guest_clipboard_changed(false);
        drop(frame);
        clipboard.offer = Some(text);
        clipboard.requested = false;
        let _ = self.send_clipboard_message(CLIP_NOTIFY | CLIP_TEXT, &[]);
    }

    /// Asks the guest for its clipboard text; it arrives as `clipboard_text`.
    pub fn request_clipboard(&self) -> Result<()> {
        let mut clipboard = self.clipboard.lock().unwrap_or_else(|e| e.into_inner());
        if !self.frame.lock().unwrap_or_else(|e| e.into_inner()).vnc_clipboard {
            bail!("Clipboard sharing needs the guest agent.");
        }
        if clipboard.offer.is_some() {
            bail!("The guest has no clipboard text of its own.");
        }
        self.send_clipboard_message(CLIP_REQUEST | CLIP_TEXT, &[])?;
        clipboard.requested = true;
        Ok(())
    }

    /// ClientCutText in the extended form: a negative length, then flags and data.
    fn send_clipboard_message(&self, flags: u32, data: &[u8]) -> Result<()> {
        let mut msg = vec![6, 0, 0, 0];
        msg.extend_from_slice(&(-((4 + data.len()) as i32)).to_be_bytes());
        msg.extend_from_slice(&flags.to_be_bytes());
        msg.extend_from_slice(data);
        self.send(&msg)
    }

    pub(super) fn received_clipboard(&self, text: String, extended: bool) {
        let mut clipboard = self.clipboard.lock().unwrap_or_else(|e| e.into_inner());
        if extended && !clipboard.requested {
            return;
        }
        clipboard.offer = None;
        clipboard.requested = false;
        let mut frame = self.frame.lock().unwrap_or_else(|e| e.into_inner());
        frame.received_clipboard(text);
    }

    /// QEMU's extended clipboard (`vnc-clipboard.c`): capabilities, notify, request, provide.
    pub(super) fn extended_clipboard(&self, payload: &[u8]) -> Result<()> {
        if payload.len() < 4 {
            return Ok(());
        }
        let flags = u32::from_be_bytes(payload[..4].try_into().unwrap_or_default());
        let data = &payload[4..];
        if flags & CLIP_CAPS != 0 {
            if flags & CLIP_TEXT == 0 || flags & CLIP_PROVIDE == 0 {
                return Ok(());
            }
            // Request data explicitly after a notification.
            let actions = CLIP_REQUEST | CLIP_PEEK | CLIP_NOTIFY | CLIP_PROVIDE;
            self.send_clipboard_message(CLIP_CAPS | CLIP_TEXT | actions, &0u32.to_be_bytes())?;
            self.frame.lock().unwrap_or_else(|e| e.into_inner()).vnc_clipboard = true;
            return Ok(());
        }
        if flags & CLIP_NOTIFY != 0 {
            let mut clipboard = self.clipboard.lock().unwrap_or_else(|e| e.into_inner());
            let has_text = flags & CLIP_TEXT != 0;
            clipboard.offer = None;
            clipboard.requested = has_text;
            self.frame.lock().unwrap_or_else(|e| e.into_inner()).guest_clipboard_changed(has_text);
            if has_text {
                self.send_clipboard_message(CLIP_REQUEST | CLIP_TEXT, &[])?;
            }
            return Ok(());
        }
        if flags & CLIP_PEEK != 0 {
            let has_text = self.clipboard.lock().unwrap_or_else(|e| e.into_inner()).offer.is_some();
            self.send_clipboard_message(
                CLIP_NOTIFY
                    | if has_text {
                        CLIP_TEXT
                    } else {
                        0
                    },
                &[],
            )?;
        }
        if flags & CLIP_TEXT == 0 {
            return Ok(());
        }
        if flags & CLIP_PROVIDE != 0
            && let Some(text) = inflate_text(data)
        {
            self.received_clipboard(text, true);
        }
        if flags & CLIP_REQUEST != 0 {
            let clipboard = self.clipboard.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(text) = &clipboard.offer {
                self.send_clipboard_message(CLIP_PROVIDE | CLIP_TEXT, &deflate_text(text)?)?;
            }
        }
        Ok(())
    }
}

// QEMU's extended clipboard pseudo-encoding and its message flags.
pub(super) const CLIPBOARD_EXT: i32 = 0xc0a1_e5ce_u32 as i32;
const CLIP_TEXT: u32 = 1;
const CLIP_CAPS: u32 = 1 << 24;
const CLIP_REQUEST: u32 = 1 << 25;
const CLIP_PEEK: u32 = 1 << 26;
const CLIP_NOTIFY: u32 = 1 << 27;
const CLIP_PROVIDE: u32 = 1 << 28;
// QEMU caps the inflated buffer at 1 MiB, including the length and terminator.
const MAX_CLIPBOARD_TEXT: usize = (1 << 20) - 5;
pub(super) const MAX_CLIPBOARD_MESSAGE: usize = 4 << 20;

/// QEMU forwards the exact payload bytes to its guest agent.
fn deflate_text(text: &str) -> Result<Vec<u8>> {
    use std::io::Write as _;
    let mut raw = Vec::with_capacity(text.len() + 4);
    raw.extend_from_slice(&(text.len() as u32).to_be_bytes());
    raw.extend_from_slice(text.as_bytes());
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&raw)?;
    Ok(encoder.finish()?)
}

fn inflate_text(data: &[u8]) -> Option<String> {
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(data)
        .take(MAX_CLIPBOARD_TEXT as u64 + 6)
        .read_to_end(&mut raw)
        .ok()?;
    let size = u32::from_be_bytes(raw.get(..4)?.try_into().ok()?) as usize;
    let text = raw.get(4..)?;
    if size != text.len() {
        return None;
    }
    let text = text.strip_suffix(&[0]).unwrap_or(text);
    if text.len() > MAX_CLIPBOARD_TEXT {
        return None;
    }
    Some(std::str::from_utf8(text).ok()?.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::vnc::tests::local_session;

    #[test]
    fn clipboard_text_round_trips_through_the_provide_encoding() {
        for text in ["", "hello", "naïve — ünïcode\nline two"] {
            assert_eq!(inflate_text(&deflate_text(text).unwrap()).unwrap(), text);
        }
        let text = "héllo";
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(deflate_text(text).unwrap().as_slice())
            .read_to_end(&mut raw)
            .unwrap();
        assert_eq!(&raw[4..], text.as_bytes());
        assert!(
            inflate_text(&deflate_text(&"a".repeat(MAX_CLIPBOARD_TEXT + 1)).unwrap()).is_none()
        );
    }

    #[test]
    fn vnc_clipboard_ignores_guest_data_after_a_new_host_offer() -> Result<()> {
        let (session, _, peer) = local_session(4, 3);
        session.extended_clipboard(&(CLIP_CAPS | CLIP_TEXT | CLIP_PROVIDE).to_be_bytes())?;
        session.extended_clipboard(&(CLIP_NOTIFY | CLIP_TEXT).to_be_bytes())?;
        assert!(peer.lock().unwrap().ends_with(&(CLIP_REQUEST | CLIP_TEXT).to_be_bytes()));
        let mut guest = (CLIP_PROVIDE | CLIP_TEXT).to_be_bytes().to_vec();
        guest.extend(deflate_text("guest text")?);
        session.offer_clipboard("host text".into());
        session.extended_clipboard(&guest)?;
        assert!(session.frame.lock().unwrap().clipboard_text.is_none());

        session.extended_clipboard(&(CLIP_NOTIFY | CLIP_TEXT).to_be_bytes())?;
        session.extended_clipboard(&guest)?;
        assert_eq!(session.frame.lock().unwrap().clipboard_text.as_deref(), Some("guest text"));
        session.extended_clipboard(&CLIP_NOTIFY.to_be_bytes())?;
        assert!(session.frame.lock().unwrap().clipboard_text.is_none());
        Ok(())
    }
}
