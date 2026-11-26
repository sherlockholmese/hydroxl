//! The SPICE GLib client runs on one thread. GPUI only sees owned frame data and commands.
mod api;
mod clipboard;
mod gpu;
mod software;

use clipboard::{
    ClipboardRequest, on_clipboard_data, on_clipboard_grab, on_clipboard_release,
    on_clipboard_request,
};
use gpu::{PendingDraw, ReadbackProfile, ScanoutLayout, on_gl_draw, on_scanout_change};
use software::{SoftwareDisplay, on_create, on_damage, on_display_mark, on_primary_destroy};

use super::frame::{Cursor, Framebuffer};
use super::graphics_fd::GraphicsFd;
use super::{keys, scanout};
use crate::backend::{Connection, SpiceOptions};
use anyhow::{Context, Result, ensure};
use api::{Api, GlibError, Object};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeSet, HashMap},
    ffi::{CStr, c_int, c_void},
    os::fd::{AsRawFd, IntoRawFd},
    ptr::null_mut,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const MAIN: i32 = 1;
const DISPLAY: i32 = 2;
const INPUTS: i32 = 3;
const CURSOR: i32 = 4;
const USBREDIR: i32 = 9;
const CHANNEL_OPENED: i32 = 10;
const CHANNEL_CLOSED: i32 = 12;
const CHANNEL_ERROR_CONNECT: i32 = 20;
const CLIENT_MOUSE_MODE: i32 = 2;
const MONITORS_CONFIG: u32 = 1;
const CLIPBOARD_BY_DEMAND: u32 = 5;
const CLIPBOARD: u32 = 0;
const UTF8_TEXT: u32 = 1;
const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;
const CLIPBOARD_TIMEOUT: Duration = Duration::from_secs(4);
const G_SIGNAL_MATCH_DATA: u32 = 1 << 4;

type Frame = Arc<Mutex<Framebuffer>>;

enum Message {
    Open {
        id: u64,
        connection: Connection,
        machine: String,
        options: SpiceOptions,
        gpu: bool,
        frame: Frame,
        result: mpsc::SyncSender<Result<()>>,
    },
    Input(u64, Input),
    Frame(u64),
    Close(u64),
}

struct Worker {
    thread: std::thread::JoinHandle<()>,
    sender: mpsc::Sender<Message>,
    api: &'static Api,
}

impl Worker {
    fn send(&self, message: Message) -> Result<()> {
        self.sender.send(message).context("The SPICE client is unavailable.")?;
        // A ready source wakes GLib even if it is queued just before the worker
        // starts a blocking iteration. A bare context wakeup can lose that race.
        unsafe { (self.api.g_idle_add_full)(0, wake_worker, null_mut(), None) };
        Ok(())
    }
}

unsafe extern "C" fn wake_worker(_: Object) -> c_int {
    0
}

pub(super) enum Input {
    Key(u32, bool),
    Pointer(u16, u16, u8),
    Relative(i32, i32, u8),
    ResetPointer,
    Release,
    Resize(u16, u16),
    SendClipboard(String),
    ReadClipboard(mpsc::Sender<Result<String>>),
    Usb {
        bus: u8,
        address: u8,
        result: mpsc::Sender<Result<bool>>,
    },
}

pub(crate) struct Session {
    id: u64,
    pub frame: Frame,
    worker: Arc<Worker>,
}

impl Session {
    pub(super) fn connect(
        connection: Connection,
        machine: String,
        options: SpiceOptions,
        gpu: bool,
    ) -> Result<Self> {
        static WORKER: Mutex<Option<Arc<Worker>>> = Mutex::new(None);
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let api = api::api()?;
        let mut shared = WORKER.lock().unwrap_or_else(|error| error.into_inner());
        let worker =
            if let Some(worker) = shared.as_ref().filter(|worker| !worker.thread.is_finished()) {
                worker.clone()
            } else {
                let (tx, rx) = mpsc::channel();
                let thread = std::thread::Builder::new()
                    .name("spice-display".into())
                    .spawn(move || work(api, rx))
                    .context("Could not start the SPICE display client.")?;
                let worker = Arc::new(Worker {
                    thread,
                    sender: tx,
                    api,
                });
                *shared = Some(worker.clone());
                worker
            };
        drop(shared);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let frame = Frame::default();
        let (result, ready) = mpsc::sync_channel(1);
        worker
            .send(Message::Open {
                id,
                connection,
                machine,
                options,
                gpu,
                frame: frame.clone(),
                result,
            })
            .context("The SPICE client is unavailable.")?;
        let session = Self {
            id,
            frame,
            worker,
        };
        match ready.recv_timeout(Duration::from_secs(10)) {
            Ok(result) => result?,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                anyhow::bail!("The SPICE display did not become ready in time.")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("The SPICE display client stopped before connecting.")
            }
        }
        Ok(session)
    }

    pub(super) fn send(&self, input: Input) {
        self.send_active(Message::Input(self.id, input));
    }

    pub(super) fn request_frame(&self) {
        self.send_active(Message::Frame(self.id));
    }

    fn send_active(&self, message: Message) {
        if self.worker.send(message).is_err() {
            let mut frame = self.frame.lock().unwrap_or_else(|error| error.into_inner());
            frame.closed.get_or_insert_with(|| {
                "The SPICE display client stopped. Reconnect to the display.".into()
            });
        }
    }

    pub(super) fn close(&self) {
        let _ = self.worker.send(Message::Close(self.id));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

fn work(api: &'static Api, rx: mpsc::Receiver<Message>) {
    let mut clients = HashMap::new();
    loop {
        let mut messages = 0;
        for message in rx.try_iter().take(32) {
            messages += 1;
            match message {
                Message::Open {
                    id,
                    connection,
                    machine,
                    options,
                    gpu,
                    frame,
                    result,
                } => match Client::open(
                    api,
                    connection.uri(),
                    &machine,
                    options,
                    gpu,
                    frame,
                    result.clone(),
                ) {
                    Ok(client) => {
                        clients.insert(id, client);
                    }
                    Err(error) => {
                        let _ = result.send(Err(error));
                    }
                },
                Message::Input(id, command) => {
                    if let Some(client) = clients.get(&id) {
                        client.input(command);
                    }
                }
                Message::Frame(id) => {
                    if let Some(client) = clients.get(&id) {
                        client.software.borrow_mut().requested = true;
                    }
                }
                Message::Close(id) => {
                    clients.remove(&id);
                }
            }
        }
        // Drain ready work without an artificial sleep between batches. Bound time,
        // not draw count, so input and snapshots still progress on a busy channel.
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(4) {
            if unsafe { (api.g_main_context_iteration)(null_mut(), 0) } == 0 {
                break;
            }
        }
        for client in clients.values() {
            client.advance_gpu_draw();
            // Canvas pointers never leave the decoding thread. GPUI receives only
            // owned pixels, sampled after coalescing the available invalidations.
            unsafe { client.software.borrow_mut().publish(&mut client.frame()) };
        }
        if messages < 32 {
            // Ready sources dispatch immediately; otherwise wait for I/O, timers,
            // or the source posted by Worker::send, not a polling timeout.
            unsafe { (api.g_main_context_iteration)(null_mut(), 1) };
        }
    }
}

struct Client {
    api: &'static Api,
    attachment: GraphicsFd,
    options: SpiceOptions,
    session: Object,
    main: Cell<Object>,
    display: Cell<Object>,
    inputs: Cell<Object>,
    channels: RefCell<Vec<Object>>,
    ready: RefCell<Option<mpsc::SyncSender<Result<()>>>>,
    main_open: Cell<bool>,
    display_open: Cell<bool>,
    frame: Frame,
    software: RefCell<SoftwareDisplay>,
    pressed: RefCell<BTreeSet<u32>>,
    buttons: Cell<u8>,
    last_pointer: Cell<Option<(u16, u16)>>,
    clipboard_offer: RefCell<Option<String>>,
    clipboard_request: RefCell<Option<ClipboardRequest>>,
    gpu: Cell<bool>,
    /// A GPU draw QEMU is waiting on. SPICE sends no other draw until it is acknowledged.
    pending: RefCell<Option<PendingDraw>>,
    scanout: RefCell<Option<(scanout::Scanout, scanout::Imported)>>,
    readback_pixels: RefCell<Vec<u8>>,
    profile: RefCell<Option<ReadbackProfile>>,
    /// Last scanout layout described in the debug log.
    logged_scanout: Cell<Option<ScanoutLayout>>,
    logged_scanout_visible: Cell<Option<bool>>,
}

impl Client {
    fn open(
        api: &'static Api,
        uri: &str,
        id: &str,
        options: SpiceOptions,
        gpu: bool,
        frame: Frame,
        ready: mpsc::SyncSender<Result<()>>,
    ) -> Result<Box<Self>> {
        let mut client = Box::new(Self {
            api,
            attachment: GraphicsFd::new(uri, id)?,
            options,
            session: null_mut(),
            main: Cell::default(),
            display: Cell::default(),
            inputs: Cell::default(),
            channels: RefCell::default(),
            ready: RefCell::new(Some(ready)),
            main_open: Cell::default(),
            display_open: Cell::default(),
            frame,
            software: RefCell::default(),
            pressed: RefCell::default(),
            buttons: Cell::default(),
            last_pointer: Cell::default(),
            clipboard_offer: RefCell::default(),
            clipboard_request: RefCell::default(),
            gpu: Cell::new(gpu),
            pending: RefCell::default(),
            scanout: RefCell::default(),
            readback_pixels: RefCell::default(),
            profile: RefCell::new(
                std::env::var_os("HYDROXL_PROFILE_SPICE").is_some().then(ReadbackProfile::default),
            ),
            logged_scanout: Cell::new(None),
            logged_scanout_visible: Cell::new(None),
        });
        // All handles are acquired and released on this thread. Drop also handles
        // partial initialization, and unregisters callbacks before freeing client data.
        unsafe {
            client.session = (api.spice_session_new)();
            ensure!(!client.session.is_null(), "Could not create a SPICE session.");
            (api.g_object_set)(
                client.session,
                c"gl-scanout".as_ptr(),
                i32::from(options.gl),
                std::ptr::null::<c_void>(),
            );
            client.signal(client.session, c"channel-new", on_channel as *const c_void);
            client.signal(client.session, c"channel-destroy", on_channel_destroy as *const c_void);
            (api.spice_audio_get)(client.session, null_mut());
            if options.usb {
                (api.spice_usb_device_manager_get)(client.session, null_mut());
            } else {
                (api.g_object_set)(
                    client.session,
                    c"enable-usbredir".as_ptr(),
                    0i32,
                    std::ptr::null::<c_void>(),
                );
            }
            ensure!(
                (api.spice_session_open_fd)(client.session, -1) != 0,
                "Could not open the SPICE session."
            );
        }
        Ok(client)
    }

    fn data(&self) -> Object {
        std::ptr::from_ref(self).cast_mut().cast()
    }

    unsafe fn signal(&self, object: Object, name: &std::ffi::CStr, handler: *const c_void) {
        unsafe {
            (self.api.g_signal_connect_data)(object, name.as_ptr(), handler, self.data(), None, 0)
        };
    }

    fn property(&self, object: Object, name: &std::ffi::CStr) -> i32 {
        let mut value = 0i32;
        // The named properties used here are all gint or gboolean.
        unsafe {
            (self.api.g_object_get)(
                object,
                name.as_ptr(),
                &mut value as *mut i32,
                std::ptr::null::<c_void>(),
            );
        }
        value
    }

    fn frame(&self) -> std::sync::MutexGuard<'_, Framebuffer> {
        self.frame.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn disconnect(&self, message: &str) {
        let mut frame = self.frame();
        if frame.closed.is_none() {
            frame.closed = Some(message.to_owned());
            frame.generation += 1;
            if let Some(ready) = self.ready.take() {
                let _ = ready.send(Err(anyhow::anyhow!("{message}")));
            }
        }
    }

    fn input(&self, command: Input) {
        let command = match command {
            Input::Resize(width, height) => {
                let main = self.main.get();
                if !main.is_null() && self.frame().spice_resize {
                    unsafe {
                        (self.api.spice_main_channel_update_display_enabled)(main, 0, 1, 0);
                        (self.api.spice_main_channel_update_display)(
                            main,
                            0,
                            0,
                            0,
                            width.into(),
                            height.into(),
                            1,
                        );
                    }
                }
                return;
            }
            Input::Usb {
                bus,
                address,
                result,
            } => {
                if let Err(error) = self.toggle_usb(bus, address, result.clone()) {
                    let _ = result.send(Err(error));
                }
                return;
            }
            Input::SendClipboard(text) => {
                let main = self.main.get();
                if !main.is_null()
                    && text.len() <= MAX_CLIPBOARD_BYTES
                    && self.frame().spice_clipboard
                {
                    self.clipboard_offer.replace(Some(text));
                    self.frame().guest_clipboard_changed(false);
                    unsafe {
                        (self.api.spice_main_channel_clipboard_selection_grab)(
                            main, CLIPBOARD, &UTF8_TEXT, 1,
                        );
                    }
                }
                return;
            }
            Input::ReadClipboard(result) => {
                let main = self.main.get();
                let (available, revision) = {
                    let frame = self.frame();
                    (frame.spice_clipboard, frame.guest_clipboard)
                };
                if main.is_null() || !available || self.clipboard_offer.borrow().is_some() {
                    let _ =
                        result.send(Err(anyhow::anyhow!("The guest clipboard is unavailable.")));
                } else if self
                    .clipboard_request
                    .borrow()
                    .as_ref()
                    .is_some_and(|request| request.started.elapsed() < CLIPBOARD_TIMEOUT)
                {
                    let _ = result.send(Err(anyhow::anyhow!("A clipboard request is pending.")));
                } else {
                    self.clipboard_request.replace(Some(ClipboardRequest {
                        revision,
                        started: Instant::now(),
                        result,
                    }));
                    unsafe {
                        (self.api.spice_main_channel_clipboard_selection_request)(
                            main, CLIPBOARD, UTF8_TEXT,
                        );
                    }
                }
                return;
            }
            other => other,
        };
        let inputs = self.inputs.get();
        if inputs.is_null() {
            return;
        }
        match command {
            Input::Key(symbol, down) => {
                if let Some(code) = keys::scancode(symbol) {
                    if down {
                        self.pressed.borrow_mut().insert(code);
                    } else {
                        self.pressed.borrow_mut().remove(&code);
                    }
                    self.send_key(inputs, code, down);
                }
            }
            Input::Pointer(x, y, buttons) => {
                let previous = self.last_pointer.replace(Some((x, y)));
                let relative = self.frame().absolute == Some(false);
                if relative {
                    if let Some((px, py)) = previous {
                        unsafe {
                            (self.api.spice_inputs_channel_motion)(
                                inputs,
                                i32::from(x) - i32::from(px),
                                i32::from(y) - i32::from(py),
                                Self::button_state(buttons),
                            );
                        }
                    }
                } else {
                    unsafe {
                        (self.api.spice_inputs_channel_position)(
                            inputs,
                            x.into(),
                            y.into(),
                            0,
                            Self::button_state(buttons),
                        );
                    }
                }
                self.send_buttons(inputs, buttons);
            }
            Input::Relative(dx, dy, buttons) => {
                if self.frame().absolute == Some(false) {
                    unsafe {
                        (self.api.spice_inputs_channel_motion)(
                            inputs,
                            dx,
                            dy,
                            Self::button_state(buttons),
                        );
                    }
                }
                self.send_buttons(inputs, buttons);
            }
            Input::Release => {
                self.last_pointer.set(None);
                for code in self.pressed.take() {
                    self.send_key(inputs, code, false);
                }
                self.send_buttons(inputs, 0);
            }
            Input::ResetPointer => self.last_pointer.set(None),
            Input::Resize(..)
            | Input::Usb {
                ..
            }
            | Input::SendClipboard(_)
            | Input::ReadClipboard(_) => unreachable!(),
        }
    }

    fn send_key(&self, channel: Object, code: u32, down: bool) {
        unsafe {
            if down {
                (self.api.spice_inputs_channel_key_press)(channel, code);
            } else {
                (self.api.spice_inputs_channel_key_release)(channel, code);
            }
        }
    }

    fn button_state(bits: u8) -> i32 {
        // RFB uses left/middle/right, SPICE uses left/middle/right but treats wheel as buttons.
        i32::from(bits & 7)
    }

    fn send_buttons(&self, channel: Object, buttons: u8) {
        let changed = self.buttons.replace(buttons) ^ buttons;
        for (bit, button) in [(1, 1), (2, 2), (4, 3), (8, 4), (16, 5)] {
            if changed & bit != 0 {
                let send = if buttons & bit != 0 {
                    self.api.spice_inputs_channel_button_press
                } else {
                    self.api.spice_inputs_channel_button_release
                };
                unsafe { send(channel, button, Self::button_state(buttons)) };
            }
        }
    }

    fn toggle_usb(&self, bus: u8, address: u8, result: mpsc::Sender<Result<bool>>) -> Result<()> {
        // The manager and boxed device references belong to this GLib thread.
        unsafe {
            let mut error = null_mut();
            let manager = (self.api.spice_usb_device_manager_get)(self.session, &mut error);
            if manager.is_null() {
                return Err(glib_error(self.api, error, "USB redirection is unavailable."));
            }
            let devices = (self.api.spice_usb_device_manager_get_devices)(manager);
            ensure!(!devices.is_null(), "USB redirection is unavailable.");
            let count = (*devices).len as usize;
            let candidates = if count == 0 {
                &[][..]
            } else {
                std::slice::from_raw_parts((*devices).data, count)
            };
            let device = candidates.iter().copied().find(|&device| {
                let native = (self.api.spice_usb_device_get_libusb_device)(device);
                !native.is_null()
                    && (self.api.libusb_get_bus_number)(native) == bus
                    && (self.api.libusb_get_device_address)(native) == address
            });
            if let Some(device) = device {
                let attach =
                    (self.api.spice_usb_device_manager_is_device_connected)(manager, device) == 0;
                let send = if attach {
                    self.api.spice_usb_device_manager_connect_device_async
                } else {
                    self.api.spice_usb_device_manager_disconnect_device_async
                };
                let request = Box::new(UsbRequest {
                    api: self.api,
                    attach,
                    result,
                });
                send(manager, device, null_mut(), usb_finished, Box::into_raw(request).cast());
                (self.api.g_ptr_array_unref)(devices);
                Ok(())
            } else {
                (self.api.g_ptr_array_unref)(devices);
                anyhow::bail!("The USB device is no longer available for SPICE redirection.");
            }
        }
    }
}

struct UsbRequest {
    api: &'static Api,
    attach: bool,
    result: mpsc::Sender<Result<bool>>,
}

unsafe fn glib_error(api: &Api, error: *mut GlibError, fallback: &str) -> anyhow::Error {
    if error.is_null() {
        return anyhow::anyhow!("{fallback}");
    }
    let message = unsafe { CStr::from_ptr((*error).message) }.to_string_lossy().into_owned();
    unsafe { (api.g_error_free)(error) };
    anyhow::anyhow!("{message}")
}

unsafe extern "C" fn usb_finished(manager: Object, result: Object, data: Object) {
    let request = unsafe { Box::from_raw(data.cast::<UsbRequest>()) };
    let finish = if request.attach {
        request.api.spice_usb_device_manager_connect_device_finish
    } else {
        request.api.spice_usb_device_manager_disconnect_device_finish
    };
    let mut error = null_mut();
    let result = if unsafe { finish(manager, result, &mut error) } != 0 {
        Ok(request.attach)
    } else {
        Err(unsafe { glib_error(request.api, error, "Could not redirect the USB device.") })
    };
    let _ = request.result.send(result);
}

impl Drop for Client {
    fn drop(&mut self) {
        unsafe {
            if !self.session.is_null() {
                for object in std::iter::once(self.session).chain(self.channels.take()) {
                    (self.api.g_signal_handlers_disconnect_matched)(
                        object,
                        G_SIGNAL_MATCH_DATA,
                        0,
                        0,
                        null_mut(),
                        null_mut(),
                        self.data(),
                    );
                    if object != self.session {
                        (self.api.g_object_unref)(object);
                    }
                }
                (self.api.spice_session_disconnect)(self.session);
                (self.api.g_object_unref)(self.session);
            }
        }
    }
}

// Callback data is the boxed Client. The worker owns it until all handlers are disconnected.
unsafe extern "C" fn on_channel(_: Object, channel: Object, data: Object) {
    let client = unsafe { &*data.cast::<Client>() };
    let kind = client.property(channel, c"channel-type");
    unsafe {
        (client.api.g_object_ref)(channel);
        client.channels.borrow_mut().push(channel);
        client.signal(channel, c"open-fd", on_open_fd as *const c_void);
        match kind {
            MAIN => {
                client.main.set(channel);
                client.signal(
                    channel,
                    c"channel-event",
                    on_required_channel_event as *const c_void,
                );
                for name in [c"main-mouse-update", c"main-agent-update"] {
                    client.signal(channel, name, on_main_update as *const c_void);
                }
                client.signal(
                    channel,
                    c"main-clipboard-selection",
                    on_clipboard_data as *const c_void,
                );
                client.signal(
                    channel,
                    c"main-clipboard-selection-request",
                    on_clipboard_request as *const c_void,
                );
                client.signal(
                    channel,
                    c"main-clipboard-selection-grab",
                    on_clipboard_grab as *const c_void,
                );
                client.signal(
                    channel,
                    c"main-clipboard-selection-release",
                    on_clipboard_release as *const c_void,
                );
            }
            DISPLAY if client.property(channel, c"channel-id") == 0 => {
                if std::env::var_os("HYDROXL_DEBUG_DISPLAY").is_some() {
                    eprintln!("display channel, gl {}", client.options.gl);
                }
                client.display.set(channel);
                client.signal(
                    channel,
                    c"channel-event",
                    on_required_channel_event as *const c_void,
                );
                client.signal(channel, c"display-primary-create", on_create as *const c_void);
                client.signal(
                    channel,
                    c"display-primary-destroy",
                    on_primary_destroy as *const c_void,
                );
                client.signal(channel, c"display-invalidate", on_damage as *const c_void);
                client.signal(channel, c"display-mark", on_display_mark as *const c_void);
                if client.options.gl {
                    client.signal(
                        channel,
                        c"notify::gl-scanout",
                        on_scanout_change as *const c_void,
                    );
                    client.signal(channel, c"gl-draw", on_gl_draw as *const c_void);
                }
                if (client.api.spice_channel_connect)(channel) == 0 {
                    client.disconnect("Could not start the SPICE display channel.");
                }
            }
            INPUTS => {
                client.inputs.set(channel);
                (client.api.spice_channel_connect)(channel);
            }
            CURSOR => {
                client.signal(channel, c"cursor-set", on_cursor as *const c_void);
                client.signal(channel, c"cursor-hide", on_cursor_hide as *const c_void);
                client.signal(channel, c"cursor-reset", on_cursor_hide as *const c_void);
                (client.api.spice_channel_connect)(channel);
            }
            USBREDIR => {
                let mut frame = client.frame();
                frame.spice_usb = true;
                frame.generation += 1;
            }
            _ => {}
        }
    }
}

unsafe extern "C" fn on_channel_destroy(_: Object, channel: Object, data: Object) {
    let client = unsafe { &*data.cast::<Client>() };
    if client.display.get() == channel {
        client.display_open.set(false);
        client.software.borrow_mut().destroy();
    }
    if client.main.get() == channel {
        client.main_open.set(false);
    }
    if client.pending.borrow().as_ref().is_some_and(|pending| pending.channel == channel) {
        client.pending.take();
        scanout::withdraw_scanout(&client.frame);
    }
    for current in [&client.main, &client.display, &client.inputs] {
        if current.get() == channel {
            current.set(null_mut());
        }
    }
    client.channels.borrow_mut().retain(|stored| *stored != channel);
    unsafe {
        (client.api.g_signal_handlers_disconnect_matched)(
            channel,
            G_SIGNAL_MATCH_DATA,
            0,
            0,
            null_mut(),
            null_mut(),
            data,
        );
        (client.api.g_object_unref)(channel);
    }
}

unsafe extern "C" fn on_open_fd(channel: Object, tls: c_int, data: Object) {
    let client = unsafe { &*data.cast::<Client>() };
    let report = |message: &str| {
        if client.main.get() == channel || client.display.get() == channel {
            client.disconnect(message);
        } else {
            eprintln!("Could not open an auxiliary SPICE channel. {message}");
        }
    };
    if tls != 0 {
        report("The local SPICE connection requested TLS.");
        return;
    }
    let fd = match client.attachment.open(client.options.graphics) {
        Ok(fd) => fd,
        Err(error) => {
            report(&format!("{error:#}"));
            return;
        }
    };
    if unsafe { (client.api.spice_channel_open_fd)(channel, fd.as_raw_fd()) } != 0 {
        let _ = fd.into_raw_fd();
    } else {
        report("Could not open a channel on the guest's SPICE display.");
    }
}

unsafe extern "C" fn on_required_channel_event(channel: Object, event: c_int, data: Object) {
    let client = unsafe { &*data.cast::<Client>() };
    if event == CHANNEL_OPENED {
        if client.main.get() == channel {
            client.main_open.set(true);
            unsafe { on_main_update(channel, data) };
        } else if client.display.get() == channel {
            client.display_open.set(true);
        }
        if client.main_open.get()
            && client.display_open.get()
            && let Some(ready) = client.ready.take()
        {
            let _ = ready.send(Ok(()));
        }
    } else if event == CHANNEL_CLOSED || event >= CHANNEL_ERROR_CONNECT {
        let name = if client.main.get() == channel {
            "main"
        } else {
            "display"
        };
        let mut message = format!("The SPICE {name} channel disconnected.");
        let error = unsafe { (client.api.spice_channel_get_error)(channel) };
        if !error.is_null() && !unsafe { (*error).message }.is_null() {
            let detail = unsafe { CStr::from_ptr((*error).message) }.to_string_lossy();
            message.push(' ');
            message.push_str(&detail);
        }
        client.disconnect(&message);
    }
}

unsafe extern "C" fn on_main_update(channel: Object, data: Object) {
    let client = unsafe { &*data.cast::<Client>() };
    let absolute = client.property(channel, c"mouse-mode") == CLIENT_MOUSE_MODE;
    let can_resize = unsafe {
        (client.api.spice_main_channel_agent_test_capability)(channel, MONITORS_CONFIG) != 0
    };
    let clipboard = client.options.clipboard
        && client.property(channel, c"agent-connected") != 0
        && unsafe {
            (client.api.spice_main_channel_agent_test_capability)(channel, CLIPBOARD_BY_DEMAND) != 0
        };
    let mut frame = client.frame();
    if frame.absolute != Some(absolute) {
        client.last_pointer.set(None);
    }
    frame.absolute = Some(absolute);
    frame.spice_resize = can_resize;
    if frame.spice_clipboard && !clipboard {
        frame.guest_clipboard_changed(false);
        client.clipboard_offer.take();
        client.clipboard_request.take();
    }
    if clipboard && !frame.spice_clipboard {
        frame.clipboard_reset = frame.clipboard_reset.wrapping_add(1);
    }
    frame.spice_clipboard = clipboard;
    frame.generation += 1;
}

unsafe extern "C" fn on_cursor(
    _: Object,
    w: i32,
    h: i32,
    hot_x: i32,
    hot_y: i32,
    pixels: *const u8,
    data: Object,
) {
    if pixels.is_null() || !(1..=1024).contains(&w) || !(1..=1024).contains(&h) {
        return;
    }
    let client = unsafe { &*data.cast::<Client>() };
    let mut rgba = unsafe { std::slice::from_raw_parts(pixels, (w * h * 4) as usize) }.to_vec();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let mut frame = client.frame();
    frame.cursor = Some(Cursor {
        width: w as u32,
        height: h as u32,
        hot_x: hot_x.max(0) as u32,
        hot_y: hot_y.max(0) as u32,
        pixels: rgba,
    });
    frame.cursor_generation += 1;
    frame.generation += 1;
}

unsafe extern "C" fn on_cursor_hide(_: Object, data: Object) {
    let client = unsafe { &*data.cast::<Client>() };
    let mut frame = client.frame();
    frame.cursor = None;
    frame.cursor_generation += 1;
    frame.generation += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Attaches to a running domain's SPICE display (HYDROXL_SPICE_DOMAIN) and reports
    /// what arrives: GL scanouts are read back through EGL, so no window is needed.
    #[test]
    #[ignore]
    fn live_spice_frames() {
        let id = std::env::var("HYDROXL_SPICE_DOMAIN").expect("HYDROXL_SPICE_DOMAIN");
        let connection = crate::backend::Connection::Session;
        let crate::backend::DisplayTarget::Spice(options) =
            crate::backend::display_target(connection, &id, true).expect("display target")
        else {
            panic!("not a SPICE display");
        };
        eprintln!("SPICE options: gl {}", options.gl);
        let gpu = std::env::var_os("HYDROXL_SPICE_GPU").is_some();
        let session =
            crate::display::session::Session::spice(connection, id, options, gpu).expect("connect");
        let resize = std::env::var("HYDROXL_SPICE_RESIZE").ok().and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some((w.parse::<u16>().ok()?, h.parse::<u16>().ok()?))
        });
        let start = Instant::now();
        let mut last = 0;
        let mut resized = false;
        while start.elapsed() < Duration::from_secs(40) {
            session.request_frame();
            std::thread::sleep(Duration::from_millis(500));
            let frame = session.frame().lock().unwrap();
            if let Some(error) = &frame.closed {
                panic!("closed: {error}");
            }
            if let Some(size) = resize.filter(|_| !resized && frame.spice_resize) {
                resized = true;
                eprintln!("requesting {}x{}", size.0, size.1);
                drop(frame);
                let _ = session.resize_desktop(size, crate::display::session::Resize::Spice);
                continue;
            }
            if frame.generation != last && !frame.pixels.is_empty() {
                last = frame.generation;
                let lit = frame
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|p| p[0] | p[1] | p[2] != 0)
                    .count();
                eprintln!(
                    "frame {}x{} generation {} lit {}/{}",
                    frame.width,
                    frame.height,
                    frame.generation,
                    lit,
                    frame.pixels.len() / 4
                );
            }
        }
        session.close();
    }

    #[test]
    #[ignore = "requires the SPICE runtime and libvirt's test driver"]
    fn loads_runtime_and_handles_an_unavailable_graphics_socket() -> Result<()> {
        let api = api::api()?;
        let frame = Frame::default();
        let (ready, result) = mpsc::sync_channel(1);
        let client = Client::open(
            api,
            "test:///default",
            "6695eb01-f6a4-8304-79aa-97f2502e193f",
            SpiceOptions {
                graphics: 0,
                clipboard: false,
                usb: false,
                gl: false,
            },
            false,
            frame.clone(),
            ready,
        )?;
        assert!(!client.main.get().is_null());
        for _ in 0..20 {
            unsafe { (api.g_main_context_iteration)(null_mut(), 0) };
            if frame.lock().unwrap().closed.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(frame.lock().unwrap().closed.is_some());
        assert!(result.try_recv()?.is_err());
        drop(client);
        unsafe { (api.g_main_context_iteration)(null_mut(), 0) };
        Ok(())
    }
}
