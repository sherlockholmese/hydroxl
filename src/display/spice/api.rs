//! Runtime bindings for the public SPICE Client GLib and libvirt C APIs.
use crate::display::scanout::{LegacyScanout, Scanout};
use anyhow::{Context, Result};
use libloading::Library;
use std::ffi::{c_char, c_int, c_ulong, c_void};
use std::sync::OnceLock;

pub(super) type Object = *mut c_void;

#[repr(C)]
pub(super) struct Primary {
    pub format: c_int,
    pub width: c_int,
    pub height: c_int,
    pub stride: c_int,
    pub shmid: c_int,
    pub data: *const u8,
    pub marked: c_int,
}

#[repr(C)]
pub(super) struct PtrArray {
    pub data: *const Object,
    pub len: u32,
}

#[repr(C)]
pub(super) struct GlibError {
    pub domain: u32,
    pub code: c_int,
    pub message: *const c_char,
}

macro_rules! bindings {
    ($($library:ident, $soname:literal, $help:literal => { $($field:ident: $fn:ty),* $(,)? }),* $(,)?) => {
        #[allow(non_snake_case)]
        pub(super) struct Api {
            $($(pub $field: $fn,)*)*
            _libraries: [Library; 1],
        }
        impl Api {
            fn load() -> Result<Self> {
                // The handles stay loaded for the lifetime of registered GObject types.
                $(let $library = unsafe { Library::new($soname) }.context($help)?;)*
                Ok(Self {
                    $($($field: *unsafe { $library.get::<$fn>(stringify!($field).as_bytes()) }
                        .with_context(|| concat!("Missing SPICE viewer API ", stringify!($field)))?,)*)*
                    _libraries: [$($library),*],
                })
            }
        }
    }
}

bindings! {
    spice, "libspice-client-glib-2.0.so.8", "Install SPICE Client GLib to open this display." => {
        g_main_context_iteration: unsafe extern "C" fn(Object, c_int) -> c_int,
        g_idle_add_full: unsafe extern "C" fn(
            c_int, unsafe extern "C" fn(Object) -> c_int, Object, Option<unsafe extern "C" fn(Object)>
        ) -> u32,
        g_timeout_add_full: unsafe extern "C" fn(
            c_int, u32, unsafe extern "C" fn(Object) -> c_int, Object, Option<unsafe extern "C" fn(Object)>
        ) -> u32,
        g_unix_fd_add_full: unsafe extern "C" fn(
            c_int, c_int, u32, unsafe extern "C" fn(c_int, u32, Object) -> c_int, Object,
            Option<unsafe extern "C" fn(Object)>
        ) -> u32,
        g_signal_connect_data: unsafe extern "C" fn(
            Object, *const c_char, *const c_void, Object, Option<unsafe extern "C" fn(Object, Object)>, u32
        ) -> c_ulong,
        g_signal_handlers_disconnect_matched: unsafe extern "C" fn(
            Object, u32, u32, u32, Object, Object, Object
        ) -> u32,
        g_object_get: unsafe extern "C" fn(Object, *const c_char, ...),
        g_object_set: unsafe extern "C" fn(Object, *const c_char, ...),
        g_object_ref: unsafe extern "C" fn(Object) -> Object,
        g_object_unref: unsafe extern "C" fn(Object),
        g_ptr_array_unref: unsafe extern "C" fn(*mut PtrArray),
        g_error_free: unsafe extern "C" fn(*mut GlibError),
        spice_session_new: unsafe extern "C" fn() -> Object,
        spice_session_open_fd: unsafe extern "C" fn(Object, c_int) -> c_int,
        spice_session_disconnect: unsafe extern "C" fn(Object),
        spice_channel_connect: unsafe extern "C" fn(Object) -> c_int,
        spice_channel_open_fd: unsafe extern "C" fn(Object, c_int) -> c_int,
        spice_channel_get_error: unsafe extern "C" fn(Object) -> *const GlibError,
        spice_display_channel_get_primary: unsafe extern "C" fn(Object, u32, *mut Primary) -> c_int,
        spice_display_channel_get_gl_scanout: unsafe extern "C" fn(Object) -> *const LegacyScanout,
        spice_display_channel_gl_draw_done: unsafe extern "C" fn(Object),
        spice_audio_get: unsafe extern "C" fn(Object, Object) -> Object,
        spice_usb_device_manager_get: unsafe extern "C" fn(Object, *mut *mut GlibError) -> Object,
        spice_usb_device_manager_get_devices: unsafe extern "C" fn(Object) -> *mut PtrArray,
        spice_usb_device_manager_is_device_connected: unsafe extern "C" fn(Object, Object) -> c_int,
        spice_usb_device_get_libusb_device: unsafe extern "C" fn(Object) -> Object,
        spice_usb_device_manager_connect_device_async: unsafe extern "C" fn(
            Object, Object, Object, unsafe extern "C" fn(Object, Object, Object), Object
        ),
        spice_usb_device_manager_disconnect_device_async: unsafe extern "C" fn(
            Object, Object, Object, unsafe extern "C" fn(Object, Object, Object), Object
        ),
        spice_usb_device_manager_connect_device_finish: unsafe extern "C" fn(Object, Object, *mut *mut GlibError) -> c_int,
        spice_usb_device_manager_disconnect_device_finish: unsafe extern "C" fn(Object, Object, *mut *mut GlibError) -> c_int,
        libusb_get_bus_number: unsafe extern "C" fn(Object) -> u8,
        libusb_get_device_address: unsafe extern "C" fn(Object) -> u8,
        spice_main_channel_agent_test_capability: unsafe extern "C" fn(Object, u32) -> c_int,
        spice_main_channel_update_display_enabled: unsafe extern "C" fn(Object, c_int, c_int, c_int),
        spice_main_channel_update_display: unsafe extern "C" fn(Object, c_int, c_int, c_int, c_int, c_int, c_int),
        spice_main_channel_clipboard_selection_grab: unsafe extern "C" fn(Object, u32, *const u32, c_int),
        spice_main_channel_clipboard_selection_request: unsafe extern "C" fn(Object, u32, u32),
        spice_main_channel_clipboard_selection_notify: unsafe extern "C" fn(Object, u32, u32, *const u8, usize),
        spice_inputs_channel_key_press: unsafe extern "C" fn(Object, u32),
        spice_inputs_channel_key_release: unsafe extern "C" fn(Object, u32),
        spice_inputs_channel_position: unsafe extern "C" fn(Object, c_int, c_int, c_int, c_int),
        spice_inputs_channel_motion: unsafe extern "C" fn(Object, c_int, c_int, c_int),
        spice_inputs_channel_button_press: unsafe extern "C" fn(Object, c_int, c_int),
        spice_inputs_channel_button_release: unsafe extern "C" fn(Object, c_int, c_int),
    },

}

pub(super) fn api() -> Result<&'static Api> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| Api::load().map_err(|error| format!("{error:#}")))
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))
}

impl Api {
    pub(super) unsafe fn scanout(&self, channel: Object) -> Option<Scanout> {
        type GetScanout = unsafe extern "C" fn(Object) -> *const Scanout;
        static MODERN: OnceLock<Option<GetScanout>> = OnceLock::new();
        let modern = MODERN.get_or_init(|| unsafe {
            self._libraries[0]
                .get::<GetScanout>(b"spice_display_channel_get_gl_scanout2")
                .ok()
                .map(|symbol| *symbol)
        });
        if let Some(read) = modern {
            unsafe { read(channel).as_ref() }.copied()
        } else {
            unsafe { (self.spice_display_channel_get_gl_scanout)(channel).as_ref() }
                .map(Scanout::from)
        }
    }
}
