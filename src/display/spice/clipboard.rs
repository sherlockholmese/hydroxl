//! SPICE agent clipboard callbacks and pending read ownership.
use super::{CLIPBOARD, Client, MAX_CLIPBOARD_BYTES, UTF8_TEXT, api::Object};
use anyhow::Result;
use std::{ffi::c_int, sync::mpsc, time::Instant};

pub(super) struct ClipboardRequest {
    pub(super) revision: u64,
    pub(super) started: Instant,
    pub(super) result: mpsc::Sender<Result<String>>,
}

pub(super) unsafe extern "C" fn on_clipboard_request(
    channel: Object,
    selection: u32,
    kind: u32,
    data: Object,
) -> c_int {
    let client = unsafe { &*data.cast::<Client>() };
    if selection != CLIPBOARD || kind != UTF8_TEXT {
        return 0;
    }
    let offer = client.clipboard_offer.borrow();
    let Some(text) = offer.as_ref() else {
        return 0;
    };
    unsafe {
        (client.api.spice_main_channel_clipboard_selection_notify)(
            channel,
            CLIPBOARD,
            UTF8_TEXT,
            text.as_ptr(),
            text.len(),
        )
    };
    1
}

/// The guest copied something. Text is fetched on demand by the display view.
pub(super) unsafe extern "C" fn on_clipboard_grab(
    _: Object,
    selection: u32,
    types: *const u32,
    ntypes: u32,
    data: Object,
) -> c_int {
    let client = unsafe { &*data.cast::<Client>() };
    if selection != CLIPBOARD || !client.options.clipboard {
        return 0;
    }
    let has_text = !types.is_null()
        && unsafe { std::slice::from_raw_parts(types, ntypes as usize) }.contains(&UTF8_TEXT);
    client.clipboard_offer.take();
    client.frame().guest_clipboard_changed(has_text);
    1
}

pub(super) unsafe extern "C" fn on_clipboard_release(_: Object, selection: u32, data: Object) {
    let client = unsafe { &*data.cast::<Client>() };
    if selection == CLIPBOARD
        && client.options.clipboard
        && client.clipboard_offer.borrow().is_none()
    {
        client.frame().guest_clipboard_changed(false);
    }
}

pub(super) unsafe extern "C" fn on_clipboard_data(
    _: Object,
    selection: u32,
    kind: u32,
    bytes: *const u8,
    len: u32,
    data: Object,
) {
    if selection != CLIPBOARD {
        return;
    }
    let client = unsafe { &*data.cast::<Client>() };
    if let Some(request) = client.clipboard_request.take() {
        let text = if request.revision != client.frame().guest_clipboard {
            Err(anyhow::anyhow!("The clipboard changed during the request."))
        } else if kind != UTF8_TEXT
            || (bytes.is_null() && len != 0)
            || len as usize > MAX_CLIPBOARD_BYTES
        {
            Err(anyhow::anyhow!("The guest clipboard did not provide text."))
        } else if len == 0 {
            Ok(String::new())
        } else {
            let bytes = unsafe { std::slice::from_raw_parts(bytes, len as usize) };
            std::str::from_utf8(bytes).map(str::to_owned).map_err(Into::into)
        };
        if let Ok(text) = &text {
            client.frame().received_clipboard(text.clone());
        }
        let _ = request.result.send(text);
    }
}
