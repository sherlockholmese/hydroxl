use super::{ClipboardRead, DisplayView};
use crate::display::session::Session;
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

const CLIPBOARD_POLL_INTERVAL: Duration = Duration::from_millis(350);
const CLIPBOARD_TIMEOUT: Duration = Duration::from_secs(4);
const DBUS_CLIPBOARD_TIMEOUT: Duration = Duration::from_secs(8);
impl DisplayView {
    pub(super) fn offer_host_clipboard(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        {
            let frame = session.frame().lock().unwrap_or_else(|e| e.into_inner());
            if !frame.shares_clipboard() {
                self.clipboard_text = None;
                return;
            }
            if frame.closed.is_some()
                || (frame.guest_clipboard_has_text
                    && (frame.guest_clipboard != self.guest_clipboard_seen
                        || (frame.vnc_clipboard
                            && frame.clipboard_text.is_none()
                            && self
                                .last_clipboard_poll
                                .is_some_and(|last| last.elapsed() < CLIPBOARD_POLL_INTERVAL))))
                || self.clipboard_read.is_some()
                || self.copy_requested.is_some()
            {
                return;
            }
        }
        self.last_clipboard_poll = Some(Instant::now());
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if self.clipboard_text.as_ref() != Some(&text)
            || (force && self.clipboard_from_host && matches!(session.as_ref(), Session::Vnc(_)))
        {
            self.clipboard_text = Some(text.clone());
            self.clipboard_from_host = true;
            session.send_clipboard(text);
        }
    }

    pub(super) fn sync_clipboard(
        &mut self,
        session: &Session,
        force_host_offer: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let can_write = window.is_window_active() || cx.compositor_name() != "Wayland";
        let (revision, shared, has_text, has_cached_text, text) = {
            let frame = session.frame().lock().unwrap_or_else(|e| e.into_inner());
            if frame.closed.is_some() {
                return;
            }
            if frame.clipboard_reset != self.clipboard_reset_seen {
                self.clipboard_reset_seen = frame.clipboard_reset;
                if self.clipboard_from_host {
                    self.clipboard_text = None;
                    self.last_clipboard_poll = None;
                }
            }
            let revision = frame.guest_clipboard;
            let has_cached_text = frame.clipboard_text.is_some();
            let text = if can_write
                && (revision != self.guest_clipboard_seen || self.copy_requested.is_some())
            {
                frame.clipboard_text.clone()
            } else {
                None
            };
            (
                revision,
                frame.shares_clipboard(),
                frame.guest_clipboard_has_text,
                has_cached_text,
                text,
            )
        };

        if let Some(text) = text {
            self.guest_clipboard_seen = revision;
            self.clipboard_read = None;
            self.clipboard_text = Some(text.clone());
            self.clipboard_from_host = false;
            self.last_clipboard_poll = Some(Instant::now());
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            if self.copy_requested.take().is_some() {
                window.push_notification("Guest clipboard copied", cx);
            }
        }

        if let Some(read) = &self.clipboard_read {
            let error = match read.receiver.try_recv() {
                Ok(result) => Some(result.err()),
                Err(mpsc::TryRecvError::Empty) if read.started.elapsed() < CLIPBOARD_TIMEOUT => {
                    None
                }
                Err(_) => Some(Some(anyhow::anyhow!("The guest clipboard did not respond."))),
            };
            if let Some(error) = error {
                if read.revision == revision
                    && let Some(error) = error
                    && !matches!(session, Session::Dbus(_))
                    && self.copy_requested.take().is_some()
                {
                    window.push_notification(
                        crate::ui::primitives::plain_error(&format!("{error:#}")),
                        cx,
                    );
                }
                self.clipboard_read = None;
            }
        }

        if revision != self.guest_clipboard_seen
            && !has_cached_text
            && (!has_text || shared)
            && self.clipboard_read.is_none()
        {
            self.guest_clipboard_seen = revision;
            if has_text {
                self.last_clipboard_poll = Some(Instant::now());
                if !matches!(session, Session::Vnc(_)) {
                    self.request_guest_clipboard(session, revision, window, cx);
                }
            }
        }
        // Older agents can leave a D-Bus request pending until QEMU's five-second timeout.
        let copy_timeout = if matches!(session, Session::Dbus(_)) {
            DBUS_CLIPBOARD_TIMEOUT
        } else {
            CLIPBOARD_TIMEOUT
        };
        if self.copy_requested.is_some_and(|started| started.elapsed() >= copy_timeout) {
            self.copy_requested = None;
            window.push_notification("The guest clipboard did not provide text.", cx);
        }
        if window.is_window_active()
            && (force_host_offer
                || self
                    .last_clipboard_poll
                    .is_none_or(|last| last.elapsed() >= CLIPBOARD_POLL_INTERVAL))
        {
            self.offer_host_clipboard(force_host_offer, cx);
        }
    }

    fn request_guest_clipboard(
        &mut self,
        session: &Session,
        revision: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = if matches!(session, Session::Vnc(_)) {
            session.request_vnc_clipboard().map(|()| None)
        } else {
            session.read_clipboard().map(Some)
        };
        match result {
            Ok(Some(receiver)) => {
                self.clipboard_read = Some(ClipboardRead {
                    revision,
                    started: Instant::now(),
                    receiver,
                });
            }
            Ok(None) => {}
            Err(error) => {
                if self.copy_requested.take().is_some() {
                    window.push_notification(
                        crate::ui::primitives::plain_error(&format!("{error:#}")),
                        cx,
                    );
                }
            }
        }
    }

    pub(super) fn copy_guest_clipboard_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let (shared, revision, cached) = {
            let frame = session.frame().lock().unwrap_or_else(|e| e.into_inner());
            (frame.shares_clipboard(), frame.guest_clipboard, frame.clipboard_text.is_some())
        };
        if !shared && !cached {
            window.push_notification(
                "Clipboard sharing needs the guest agent (spice-vdagent) in the guest.",
                cx,
            );
            return;
        }
        self.copy_requested = Some(Instant::now());
        if cached {
            self.sync_clipboard(&session, false, window, cx);
        } else if self.clipboard_read.is_none() {
            self.guest_clipboard_seen = revision;
            self.request_guest_clipboard(&session, revision, window, cx);
        }
    }
}
