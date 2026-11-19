//! The SPICE GLib client runs on one thread. GPUI only sees owned frame data and commands.
mod api;

use super::damage::Damage;
use super::graphics_fd::GraphicsFd;
use super::vnc::{Cursor, Framebuffer, MAX_DESKTOP_SIZE};
use super::{keys, scanout};
use crate::backend::{Connection, SpiceOptions};
use anyhow::{Context, Result, ensure};
use api::{Api, GlibError, Object, Primary};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeSet, HashMap},
    ffi::{CStr, c_int, c_void},
    os::fd::{AsRawFd, IntoRawFd},
    ptr::null_mut,
    sync::{
        Arc, Mutex, OnceLock,
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
const MAIN_CLOSED: i32 = 12;
const CHANNEL_ERROR_CONNECT: i32 = 20;
const CLIENT_MOUSE_MODE: i32 = 2;
const MONITORS_CONFIG: u32 = 1;
const CLIPBOARD_BY_DEMAND: u32 = 5;
const CLIPBOARD: u32 = 0;
const UTF8_TEXT: u32 = 1;
const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;
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
    worker: &'static Worker,
}

impl Session {
    pub(super) fn connect(
        connection: Connection,
        machine: String,
        options: SpiceOptions,
        gpu: bool,
    ) -> Result<Self> {
        static WORKER: OnceLock<Worker> = OnceLock::new();
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let api = api::api()?;
        let worker = WORKER.get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || work(api, rx));
            Worker {
                sender: tx,
                api,
            }
        });
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
        ready
            .recv_timeout(Duration::from_secs(10))
            .context("The SPICE connection timed out.")??;
        Ok(session)
    }

    pub(super) fn send(&self, input: Input) {
        let _ = self.worker.send(Message::Input(self.id, input));
    }

    pub(super) fn request_frame(&self) {
        let _ = self.worker.send(Message::Frame(self.id));
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
                } => match Client::open(api, connection.uri(), &machine, options, gpu, frame) {
                    Ok(client) => {
                        clients.insert(id, client);
                        let _ = result.send(Ok(()));
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

#[derive(Default)]
struct SoftwareDisplay {
    // Valid only on the SPICE thread, from primary-create until primary-destroy.
    primary: Option<Primary>,
    damage: Vec<Damage>,
    marked: bool,
    requested: bool,
}

impl SoftwareDisplay {
    fn create(&mut self, primary: Primary) {
        let (width, height) = (primary.width, primary.height);
        self.marked = primary.marked != 0;
        self.primary = Some(primary);
        self.damage.clear();
        self.invalidate(0, 0, width, height);
    }

    fn destroy(&mut self) {
        self.primary = None;
        self.damage.clear();
        self.marked = false;
    }

    fn invalidate(&mut self, x: i32, y: i32, w: i32, h: i32) {
        let Some(primary) = &self.primary else {
            return;
        };
        if !(1..=MAX_DESKTOP_SIZE as i32).contains(&primary.width)
            || !(1..=MAX_DESKTOP_SIZE as i32).contains(&primary.height)
        {
            return;
        }
        let x = x.clamp(0, primary.width) as u32;
        let y = y.clamp(0, primary.height) as u32;
        let w = (w.max(0) as u32).min(primary.width as u32 - x);
        let h = (h.max(0) as u32).min(primary.height as u32 - y);
        if w == 0 || h == 0 {
            return;
        }
        let mut rect = Damage {
            x,
            y,
            width: w,
            height: h,
        };
        // Merge only when the union costs no more pixels than separate copies.
        // Adjacent strips collapse; distant damage does not copy the gap.
        let mut index = 0;
        while index < self.damage.len() {
            let previous = self.damage[index];
            let left = previous.x.min(rect.x);
            let top = previous.y.min(rect.y);
            let right = (previous.x + previous.width).max(rect.x + rect.width);
            let bottom = (previous.y + previous.height).max(rect.y + rect.height);
            if u64::from(right - left) * u64::from(bottom - top)
                <= u64::from(previous.width) * u64::from(previous.height)
                    + u64::from(rect.width) * u64::from(rect.height)
            {
                self.damage.swap_remove(index);
                rect = Damage {
                    x: left,
                    y: top,
                    width: right - left,
                    height: bottom - top,
                };
                index = 0;
            } else {
                index += 1;
            }
        }
        // Bound fragmented updates just as the shared framebuffer does.
        if self.damage.len() >= 128 {
            self.damage.clear();
            rect = Damage::new(0, 0, primary.width as usize, primary.height as usize);
        }
        self.damage.push(rect);
    }

    /// The primary must still be alive and may only be read on its owner thread.
    unsafe fn publish(&mut self, frame: &mut Framebuffer) {
        if !self.requested || !self.marked {
            return;
        }
        self.requested = false;
        let Some(primary) = &self.primary else {
            return;
        };
        let mut updated = false;
        for rect in self.damage.drain(..) {
            updated |= unsafe {
                update_frame(
                    frame,
                    primary,
                    rect.x as i32,
                    rect.y as i32,
                    rect.width as i32,
                    rect.height as i32,
                )
            };
        }
        if updated {
            frame.generation += 1;
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
    frame: Frame,
    software: RefCell<SoftwareDisplay>,
    pressed: RefCell<BTreeSet<u32>>,
    buttons: Cell<u8>,
    last_pointer: Cell<Option<(u16, u16)>>,
    clipboard_offer: RefCell<String>,
    clipboard_request: RefCell<Option<mpsc::Sender<Result<String>>>>,
    gpu: Cell<bool>,
    /// A GPU draw QEMU is waiting on. SPICE sends no other draw until it is acknowledged.
    pending: RefCell<Option<PendingDraw>>,
    scanout: RefCell<Option<(scanout::Scanout, scanout::Imported)>>,
    readback_pixels: RefCell<Vec<u8>>,
    profile: RefCell<Option<ReadbackProfile>>,
    /// Last scanout layout described in the debug log.
    logged_scanout: Cell<Option<(u32, u32, u32, u64, u32)>>,
    logged_scanout_visible: Cell<Option<bool>>,
}

#[derive(Default)]
struct ReadbackProfile {
    milliseconds: Vec<f32>,
    pixels: u64,
}

impl ReadbackProfile {
    fn record(&mut self, elapsed: Duration, rect: Damage) {
        self.milliseconds.push(elapsed.as_secs_f32() * 1000.);
        self.pixels += u64::from(rect.width) * u64::from(rect.height);
        if self.milliseconds.len() == 120 {
            let mean = self.milliseconds.iter().sum::<f32>() / 120.;
            self.milliseconds.sort_unstable_by(f32::total_cmp);
            eprintln!(
                "SPICE GL readback: mean {mean:.2} ms, p95 {:.2} ms, {:.2} MiB/frame (120 frames)",
                self.milliseconds[113],
                self.pixels as f64 * 4. / (120. * 1024. * 1024.)
            );
            self.milliseconds.clear();
            self.pixels = 0;
        }
    }
}

impl Client {
    fn open(
        api: &'static Api,
        uri: &str,
        id: &str,
        options: SpiceOptions,
        gpu: bool,
        frame: Frame,
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
        frame.closed = Some(message.to_owned());
        frame.generation += 1;
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
                    self.clipboard_offer.replace(text);
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
                if main.is_null() || !self.frame().spice_clipboard {
                    let _ =
                        result.send(Err(anyhow::anyhow!("The guest clipboard is unavailable.")));
                } else {
                    self.clipboard_request.replace(Some(result));
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
                client.signal(channel, c"channel-event", on_main_event as *const c_void);
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
            }
            DISPLAY if client.property(channel, c"channel-id") == 0 => {
                if std::env::var_os("HYDROXL_DEBUG_DISPLAY").is_some() {
                    eprintln!("display channel, gl {}", client.options.gl);
                }
                client.display.set(channel);
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
                (client.api.spice_channel_connect)(channel);
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

unsafe extern "C" fn on_scanout_change(_: Object, _: Object, data: Object) {
    let client = unsafe { &*data.cast::<Client>() };
    client.scanout.take();
}

unsafe extern "C" fn on_gl_draw(
    channel: Object,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    data: Object,
) {
    let client = unsafe { &*data.cast::<Client>() };
    // GL draws own their publication/acknowledgment path; don't later overwrite
    // their pixels with a pending software snapshot.
    client.software.borrow_mut().damage.clear();
    let debug = std::env::var_os("HYDROXL_DEBUG_DISPLAY").is_some();
    if debug {
        eprintln!("gl-draw {x},{y} {width}x{height}");
    }
    let Some(scanout) = (unsafe { client.api.scanout(channel) }) else {
        if debug {
            eprintln!("gl-draw without a scanout");
        }
        unsafe { (client.api.spice_display_channel_gl_draw_done)(channel) };
        return;
    };
    let described =
        (scanout.width, scanout.height, scanout.format, scanout.modifier, scanout.planes);
    let first = debug && client.logged_scanout.get() != Some(described);
    if first {
        client.logged_scanout.set(Some(described));
        eprintln!(
            "SPICE GL scanout: {}x{} format {:?} modifier {:#x} planes {} stride {} offset {} top-down {} direct import {}",
            scanout.width,
            scanout.height,
            String::from_utf8_lossy(&scanout.format.to_le_bytes()),
            scanout.modifier,
            scanout.planes,
            scanout.strides[0],
            scanout.offsets[0],
            scanout.rows_are_top_down(),
            client.gpu.get(),
        );
    }
    let rect = [x, y, width, height];
    if client.gpu.get() {
        // Like UTM: return to GLib at once so input, cursor and audio keep flowing.
        // The worker loop acknowledges once the producer's fence signals and the
        // UI's GPU copy completes.
        *client.pending.borrow_mut() = Some(PendingDraw {
            channel,
            scanout,
            rect,
            stage: Stage::Fence,
            since: Instant::now(),
        });
        unsafe {
            (client.api.g_unix_fd_add_full)(
                0,
                scanout.fds[0],
                1, // G_IO_IN: the DMA-BUF's write fences have signaled.
                wake_on_fd,
                null_mut(),
                None,
            );
        }
        client.wake_after(DRAW_TIMEOUT);
        client.advance_gpu_draw();
        return;
    }
    client.read_back(channel, scanout, rect, false, first);
}

/// How long a GPU draw may wait on the producer's fence or the UI before it falls back.
const DRAW_TIMEOUT: Duration = Duration::from_millis(100);

struct PendingDraw {
    channel: Object,
    scanout: scanout::Scanout,
    rect: [u32; 4],
    stage: Stage,
    since: Instant,
}

enum Stage {
    /// The guest's GPU is still writing the buffer.
    Fence,
    /// Offered to the UI's next frame; the outcome reports the copy.
    Copy(scanout::DrawOutcome),
}

unsafe extern "C" fn wake_on_fd(_: c_int, _: u32, _: Object) -> c_int {
    0
}

impl Client {
    fn wake_after(&self, delay: Duration) {
        unsafe {
            (self.api.g_timeout_add_full)(
                0,
                delay.as_millis() as u32 + 1,
                wake_worker,
                null_mut(),
                None,
            );
        }
    }

    /// Moves a pending GPU draw forward without blocking the SPICE thread.
    fn advance_gpu_draw(&self) {
        let mut slot = self.pending.borrow_mut();
        let Some(pending) = slot.as_mut() else {
            return;
        };
        let expired = pending.since.elapsed() >= DRAW_TIMEOUT;
        let outcome = match &pending.stage {
            Stage::Fence => {
                if !scanout::writes_finished(pending.scanout.fds[0]) && !expired {
                    return;
                }
                let api = self.api;
                let wake: Box<dyn Fn() + Send> = Box::new(move || unsafe {
                    (api.g_idle_add_full)(0, wake_worker, null_mut(), None);
                });
                let [x, y, width, height] = pending.rect.map(|value| value as usize);
                let damage = Damage::new(x, y, width, height);
                match scanout::offer_scanout(&self.frame, pending.scanout, Some(damage), Some(wake))
                {
                    Some(reply) => {
                        pending.stage = Stage::Copy(reply);
                        pending.since = Instant::now();
                        drop(slot);
                        self.wake_after(DRAW_TIMEOUT);
                        return;
                    }
                    // This layout cannot be copied on the GPU; stay on readback.
                    None => Some(false),
                }
            }
            Stage::Copy(outcome) => match outcome.try_take() {
                Some(copied) => Some(copied),
                None if expired => {
                    // The UI is not drawing (hidden or minimized). Withdraw the offer and
                    // read this frame back; once the UI owns the copy, wait for its reply.
                    if scanout::withdraw_scanout(&self.frame) {
                        None
                    } else {
                        return;
                    }
                }
                None => return,
            },
        };
        let Some(pending) = slot.take() else {
            return;
        };
        drop(slot);
        match outcome {
            Some(true) => {
                scanout::scanout_copied(&self.frame, pending.scanout);
                if std::mem::take(&mut self.frame().preview_requested) {
                    self.read_back(pending.channel, pending.scanout, pending.rect, true, false);
                } else {
                    unsafe { (self.api.spice_display_channel_gl_draw_done)(pending.channel) };
                }
            }
            Some(false) => {
                if std::env::var_os("HYDROXL_DEBUG_DISPLAY").is_some() {
                    eprintln!("SPICE GL: direct import declined; using EGL readback.");
                }
                self.gpu.set(false);
                self.read_back(pending.channel, pending.scanout, pending.rect, false, false);
            }
            None => self.read_back(pending.channel, pending.scanout, pending.rect, false, false),
        }
    }

    /// Reads the scanout through EGL, publishes it (or only a thumbnail when the GPU
    /// already copied it), then acknowledges the draw.
    fn read_back(
        &self,
        channel: Object,
        scanout: scanout::Scanout,
        [x, y, width, height]: [u32; 4],
        gpu_copied: bool,
        first: bool,
    ) {
        let debug = first || std::env::var_os("HYDROXL_DEBUG_DISPLAY").is_some();
        let result = (|| -> Result<()> {
            let renderer = scanout::renderer()?;
            let mut cache = self.scanout.borrow_mut();
            let new_scanout = cache.as_ref().is_none_or(|(previous, _)| *previous != scanout);
            if new_scanout {
                *cache = Some((scanout, renderer.import(scanout)?));
            }
            let Some((_, imported)) = cache.as_ref() else {
                return Ok(());
            };
            let (w, h) = (imported.width, imported.height);
            let resized = {
                let frame = self.frame();
                frame.pixels.is_empty() || (frame.width as usize, frame.height as usize) != (w, h)
            };
            let (x, y) = (x as usize, y as usize);
            let (width, height) = (width as usize, height as usize);
            let fits = x < w && y < h && width > 0 && height > 0;
            let rect = if gpu_copied || new_scanout || resized || !fits {
                Damage::new(0, 0, w, h)
            } else {
                Damage::new(x, y, width.min(w - x), height.min(h - y))
            };
            let mut pixels = self.readback_pixels.borrow_mut();
            let mut profile = self.profile.borrow_mut();
            let started = profile.as_ref().map(|_| Instant::now());
            // QEMU can announce the buffer before the producer finishes drawing.
            scanout::wait_for_writes(scanout.fds[0], 100);
            imported.read(rect, &mut pixels)?;

            let full_frame =
                rect.x == 0 && rect.y == 0 && rect.width as usize == w && rect.height as usize == h;
            if debug && full_frame {
                let lit = pixels.chunks_exact(4).filter(|p| p[0] | p[1] | p[2] != 0).count();
                if self.logged_scanout_visible.replace(Some(lit > 0)) != Some(lit > 0) || first {
                    eprintln!(
                        "SPICE GL readback: {} of {} pixels non-black",
                        lit,
                        pixels.len() / 4
                    );
                }
            }
            if let Some((profile, started)) = profile.as_mut().zip(started) {
                profile.record(started.elapsed(), rect);
            }

            let mut frame = self.frame();
            if gpu_copied {
                frame.thumbnail = super::thumbnail::sample(w as u32, h as u32, &pixels);
                return Ok(());
            }
            if resized {
                frame.width = w as u32;
                frame.height = h as u32;
                frame.discard_damage();
            }
            if full_frame {
                std::mem::swap(&mut frame.pixels, &mut *pixels);
            } else {
                let stride = rect.width as usize * 4;
                for (offset, row) in pixels.chunks_exact(stride).enumerate() {
                    let start = ((rect.y as usize + offset) * w + rect.x as usize) * 4;
                    frame.pixels[start..start + stride].copy_from_slice(row);
                }
            }
            frame.mark_damage(rect);
            frame.generation += 1;
            Ok(())
        })();
        // ReadPixels completes the copy before SPICE lets QEMU reuse the producer buffer.
        unsafe { (self.api.spice_display_channel_gl_draw_done)(channel) };
        if let Err(error) = result
            && !gpu_copied
        {
            self.disconnect(&format!("Could not draw the SPICE GPU buffer. {error:#}"));
        }
    }
}

unsafe extern "C" fn on_channel_destroy(_: Object, channel: Object, data: Object) {
    let client = unsafe { &*data.cast::<Client>() };
    if client.display.get() == channel {
        client.software.borrow_mut().destroy();
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
    if tls != 0 {
        client.disconnect("The local SPICE connection requested TLS.");
        return;
    }
    let fd = match client.attachment.open(client.options.graphics) {
        Ok(fd) => fd,
        Err(error) => {
            client.disconnect(&error.to_string());
            return;
        }
    };
    if unsafe { (client.api.spice_channel_open_fd)(channel, fd.as_raw_fd()) } != 0 {
        let _ = fd.into_raw_fd();
    } else {
        client.disconnect("Could not open a channel on the guest's SPICE display.");
    }
}

unsafe extern "C" fn on_main_event(_: Object, event: c_int, data: Object) {
    let client = unsafe { &*data.cast::<Client>() };
    if event == MAIN_CLOSED || event >= CHANNEL_ERROR_CONNECT {
        client.disconnect("The guest's SPICE display disconnected.");
    }
}

unsafe extern "C" fn on_main_update(channel: Object, data: Object) {
    let client = unsafe { &*data.cast::<Client>() };
    let absolute = client.property(channel, c"mouse-mode") == CLIENT_MOUSE_MODE;
    let can_resize = unsafe {
        (client.api.spice_main_channel_agent_test_capability)(channel, MONITORS_CONFIG) != 0
    };
    let clipboard = unsafe {
        (client.api.spice_main_channel_agent_test_capability)(channel, CLIPBOARD_BY_DEMAND) != 0
    };
    let mut frame = client.frame();
    if frame.absolute != Some(absolute) {
        client.last_pointer.set(None);
    }
    frame.absolute = Some(absolute);
    frame.spice_resize = can_resize;
    frame.spice_clipboard = clipboard && client.options.clipboard;
    frame.generation += 1;
}

unsafe extern "C" fn on_clipboard_request(
    channel: Object,
    selection: u32,
    kind: u32,
    data: Object,
) -> c_int {
    let client = unsafe { &*data.cast::<Client>() };
    if selection != CLIPBOARD || kind != UTF8_TEXT {
        return 0;
    }
    let text = client.clipboard_offer.borrow();
    if text.is_empty() {
        return 0;
    }
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
unsafe extern "C" fn on_clipboard_grab(
    _: Object,
    selection: u32,
    types: *const u32,
    ntypes: u32,
    data: Object,
) -> c_int {
    if selection != CLIPBOARD || types.is_null() {
        return 1;
    }
    let client = unsafe { &*data.cast::<Client>() };
    let types = unsafe { std::slice::from_raw_parts(types, ntypes as usize) };
    if types.contains(&UTF8_TEXT) {
        let mut frame = client.frame();
        frame.guest_clipboard = frame.guest_clipboard.wrapping_add(1);
    }
    1
}

unsafe extern "C" fn on_clipboard_data(
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
    if let Some(result) = client.clipboard_request.take() {
        let text = if kind != UTF8_TEXT || bytes.is_null() || len as usize > MAX_CLIPBOARD_BYTES {
            Err(anyhow::anyhow!("The guest clipboard did not provide text."))
        } else {
            let bytes = unsafe { std::slice::from_raw_parts(bytes, len as usize) };
            std::str::from_utf8(bytes).map(str::to_owned).map_err(Into::into)
        };
        let _ = result.send(text);
    }
}

unsafe extern "C" fn on_create(
    channel: Object,
    format: c_int,
    width: c_int,
    height: c_int,
    stride: c_int,
    shmid: c_int,
    pixels: Object,
    data: Object,
) {
    let client = unsafe { &*data.cast::<Client>() };
    let mut primary = Primary {
        format,
        width,
        height,
        stride,
        shmid,
        data: pixels.cast(),
        marked: 0,
    };
    // Read the channel's actual readiness: it can stay marked across a resize
    // without another display-mark signal, unlike the initial connection.
    if unsafe { (client.api.spice_display_channel_get_primary)(channel, 0, &mut primary) } == 0 {
        return;
    }
    client.software.borrow_mut().create(primary);
}

unsafe extern "C" fn on_primary_destroy(_: Object, data: Object) {
    let client = unsafe { &*data.cast::<Client>() };
    client.software.borrow_mut().destroy();
}

unsafe extern "C" fn on_damage(
    _: Object,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    data: Object,
) {
    let client = unsafe { &*data.cast::<Client>() };
    if std::env::var_os("HYDROXL_DEBUG_DISPLAY").is_some() {
        eprintln!("invalidate {x},{y} {width}x{height}");
    }
    client.software.borrow_mut().invalidate(x, y, width, height);
}

unsafe extern "C" fn on_display_mark(_: Object, marked: c_int, data: Object) {
    let client = unsafe { &*data.cast::<Client>() };
    client.software.borrow_mut().marked = marked != 0;
}

// The primary data must be valid for its dimensions and stride for this call.
unsafe fn update_frame(
    frame: &mut Framebuffer,
    primary: &Primary,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> bool {
    let (width, height) = (primary.width as usize, primary.height as usize);
    let bytes_per_pixel = match primary.format {
        16 | 80 => 2,
        32 | 96 => 4,
        _ => return false,
    };
    if width == 0
        || height == 0
        || width > MAX_DESKTOP_SIZE
        || height > MAX_DESKTOP_SIZE
        || primary.data.is_null()
        || (primary.stride.unsigned_abs() as usize) < width * bytes_per_pixel
    {
        return false;
    }
    let x = x.clamp(0, primary.width) as usize;
    let y = y.clamp(0, primary.height) as usize;
    let w = (w.max(0) as usize).min(width - x);
    let h = (h.max(0) as usize).min(height - y);
    let (x, y, w, h) = if frame.pixels.is_empty()
        || (frame.width, frame.height) != (width as u32, height as u32)
    {
        frame.width = width as u32;
        frame.height = height as u32;
        frame.pixels.resize(width * height * 4, 0);
        frame.discard_damage();
        (0, 0, width, height)
    } else {
        (x, y, w, h)
    };
    for row in y..y + h {
        // libspice owns the primary until primary-destroy. The decoding thread
        // reads it here without dispatching callbacks or yielding to GLib.
        let offset = row as isize * primary.stride as isize + (x * bytes_per_pixel) as isize;
        let source =
            unsafe { std::slice::from_raw_parts(primary.data.offset(offset), w * bytes_per_pixel) };
        let start = (row * width + x) * 4;
        let dest = &mut frame.pixels[start..start + w * 4];
        if bytes_per_pixel == 4 {
            dest.copy_from_slice(source);
            for pixel in dest.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
        } else {
            let green_bits = if primary.format == 80 {
                6
            } else {
                5
            };
            let green_mask = (1 << green_bits) - 1;
            for (source, pixel) in source.chunks_exact(2).zip(dest.chunks_exact_mut(4)) {
                let value = u16::from_le_bytes([source[0], source[1]]);
                pixel.copy_from_slice(&[
                    ((value & 31) * 255 / 31) as u8,
                    (((value >> 5) & green_mask) * 255 / green_mask) as u8,
                    (((value >> (5 + green_bits)) & 31) * 255 / 31) as u8,
                    255,
                ]);
            }
        }
    }
    frame.mark_damage(Damage::new(x, y, w, h));
    true
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
    for pixel in rgba.chunks_exact_mut(4) {
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
                let lit = frame.pixels.chunks_exact(4).filter(|p| p[0] | p[1] | p[2] != 0).count();
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
    fn software_snapshots_coalesce_and_follow_primary_lifetime() {
        let (width, height) = (3840usize, 3usize);
        let mut canvas = [3, 5, 7, 0].repeat(width * height);
        let mut software = SoftwareDisplay::default();
        software.create(Primary {
            format: 32,
            width: width as i32,
            height: height as i32,
            stride: width as i32 * 4,
            shmid: -1,
            data: canvas.as_ptr(),
            marked: 0,
        });
        let mut frame = Framebuffer::default();
        software.requested = true;
        unsafe { software.publish(&mut frame) };
        assert!(frame.pixels.is_empty(), "primary-create is not display readiness");
        software.marked = true;
        unsafe { software.publish(&mut frame) };
        let original = [3, 5, 7, 255].repeat(width * height);
        assert_eq!(frame.pixels, original);
        frame.discard_damage();
        let generation = frame.generation;

        // Model a 4K-wide refresh as 120 strips. No intermediate canvas state
        // should become visible before the consumer requests another snapshot.
        for strip in 0..120 {
            for x in strip * 32..(strip + 1) * 32 {
                let start = (width + x) * 4;
                canvas[start..start + 4].copy_from_slice(&[strip as u8, 11, 19, 0]);
            }
            software.invalidate(strip as i32 * 32, 1, 32, 1);
            unsafe { software.publish(&mut frame) };
        }
        assert_eq!(frame.pixels, original);
        assert_eq!(frame.generation, generation);
        // Read the live canvas at publication, not bytes captured when damaged.
        canvas[width * 4..width * 4 + 4].copy_from_slice(&[251, 13, 17, 0]);
        software.requested = true;
        unsafe { software.publish(&mut frame) };
        let mut expected = original;
        for x in 0..width {
            let pixel = if x == 0 {
                [251, 13, 17, 255]
            } else {
                [(x / 32) as u8, 11, 19, 255]
            };
            expected[(width + x) * 4..(width + x + 1) * 4].copy_from_slice(&pixel);
        }
        assert_eq!(frame.pixels, expected);
        assert_eq!(frame.generation, generation + 1);
        let patches = frame.take_damage();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].0, Damage::new(0, 1, width, 1));
        assert_eq!(patches[0].1, expected[width * 4..width * 8]);

        // Distant changes must not turn into a full-frame copy. The unannounced
        // pixel between them makes a bounding-box implementation visibly wrong.
        canvas[0..4].copy_from_slice(&[67, 71, 73, 0]);
        canvas[4..8].copy_from_slice(&[99, 101, 103, 0]);
        let last = (width * height - 1) * 4;
        canvas[last..last + 4].copy_from_slice(&[79, 83, 89, 0]);
        software.invalidate(0, 0, 1, 1);
        software.invalidate(width as i32 - 1, height as i32 - 1, 1, 1);
        software.requested = true;
        unsafe { software.publish(&mut frame) };
        expected[0..4].copy_from_slice(&[67, 71, 73, 255]);
        expected[last..last + 4].copy_from_slice(&[79, 83, 89, 255]);
        assert_eq!(frame.pixels, expected);
        assert_eq!(frame.generation, generation + 2);
        let mut patches = frame.take_damage();
        patches.sort_by_key(|(rect, _)| (rect.y, rect.x));
        assert_eq!(patches.len(), 2);
        assert_eq!(patches[0], (Damage::new(0, 0, 1, 1), vec![67, 71, 73, 255]));
        assert_eq!(patches[1], (Damage::new(width - 1, height - 1, 1, 1), vec![79, 83, 89, 255]));

        software.destroy();
        drop(canvas); // Publishing after primary-destroy must not read this address.
        software.requested = true;
        unsafe { software.publish(&mut frame) };
        assert_eq!(frame.pixels, expected);
        assert_eq!(frame.generation, generation + 2);
        assert!(frame.take_damage().is_empty());

        let resized = [41, 43, 47, 0, 53, 59, 61, 0];
        // The channel can retain its mark across a resolution change without
        // emitting another display-mark signal.
        software.create(Primary {
            format: 32,
            width: 1,
            height: 2,
            stride: 4,
            shmid: -1,
            data: resized.as_ptr(),
            marked: 1,
        });
        unsafe { software.publish(&mut frame) };
        assert_eq!((frame.width, frame.height), (1, 2));
        assert_eq!(frame.pixels, [41, 43, 47, 255, 53, 59, 61, 255]);
    }

    #[test]
    fn accepts_padded_and_bottom_up_spice_frames() {
        let rgb565 = [0x00, 0xf8, 0, 0, 0x1f, 0, 0, 0];
        let primary = Primary {
            format: 80,
            width: 1,
            height: 2,
            stride: -4,
            shmid: -1,
            data: rgb565[4..].as_ptr(),
            marked: 1,
        };
        let mut frame = Framebuffer::default();
        unsafe { update_frame(&mut frame, &primary, 0, 0, 1, 2) };
        assert_eq!(frame.pixels, [255, 0, 0, 255, 0, 0, 255, 255]);
    }

    #[test]
    #[ignore = "requires the SPICE runtime and libvirt's test driver"]
    fn loads_runtime_and_handles_an_unavailable_graphics_socket() -> Result<()> {
        let api = api::api()?;
        let frame = Frame::default();
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
        drop(client);
        unsafe { (api.g_main_context_iteration)(null_mut(), 0) };
        Ok(())
    }
}
