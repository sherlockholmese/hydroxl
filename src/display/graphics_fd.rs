//! Local libvirt attachment for graphical transports, including socketless SPICE.
use anyhow::{Context, Result};
use libloading::Library;
use std::{
    ffi::{CStr, CString, c_char, c_void},
    os::fd::{FromRawFd, OwnedFd},
    sync::OnceLock,
};

type Ptr = *mut c_void;
struct Libvirt {
    _library: Library,
    connect: unsafe extern "C" fn(*const c_char) -> Ptr,
    close: unsafe extern "C" fn(Ptr) -> i32,
    domain: unsafe extern "C" fn(Ptr, *const c_char) -> Ptr,
    free: unsafe extern "C" fn(Ptr) -> i32,
    graphics: unsafe extern "C" fn(Ptr, u32, u32) -> i32,
    last_error_message: unsafe extern "C" fn() -> *const c_char,
}

impl Libvirt {
    fn error(&self, operation: &'static str) -> anyhow::Error {
        // The message belongs to the calling thread and is valid until the next libvirt call.
        let message = unsafe { (self.last_error_message)() };
        if message.is_null() {
            return anyhow::anyhow!("{operation}");
        }
        let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
        anyhow::anyhow!("{}", message.trim()).context(operation)
    }
}

fn libvirt() -> Result<&'static Libvirt> {
    static API: OnceLock<Result<Libvirt, String>> = OnceLock::new();
    API.get_or_init(|| {
        (|| -> Result<Libvirt> {
            let library = unsafe { Library::new("libvirt.so.0") }.context("Install libvirt.")?;
            macro_rules! load {
                ($name:literal) => {
                    *unsafe { library.get($name) }?
                };
            }
            Ok(Libvirt {
                connect: load!(b"virConnectOpen"),
                close: load!(b"virConnectClose"),
                domain: load!(b"virDomainLookupByUUIDString"),
                free: load!(b"virDomainFree"),
                graphics: load!(b"virDomainOpenGraphicsFD"),
                last_error_message: load!(b"virGetLastErrorMessage"),
                _library: library,
            })
        })()
        .map_err(|error| format!("{error:#}"))
    })
    .as_ref()
    .map_err(|error| anyhow::anyhow!("{error}"))
}

pub(super) struct GraphicsFd {
    libvirt: &'static Libvirt,
    connection: Ptr,
    domain: Ptr,
}

impl GraphicsFd {
    pub fn new(uri: &str, id: &str) -> Result<Self> {
        let libvirt = libvirt()?;
        let uri = CString::new(uri)?;
        let id = CString::new(id)?;
        // The libvirt connection and domain stay on the calling thread.
        let connection = unsafe { (libvirt.connect)(uri.as_ptr()) };
        if connection.is_null() {
            return Err(libvirt.error("Could not connect to libvirt"));
        }
        let domain = unsafe { (libvirt.domain)(connection, id.as_ptr()) };
        let attachment = Self {
            libvirt,
            connection,
            domain,
        };
        if domain.is_null() {
            return Err(libvirt.error("Could not find the virtual machine"));
        }
        Ok(attachment)
    }

    pub fn open(&self, index: u32) -> Result<OwnedFd> {
        // libvirt authorizes the local attachment and authenticates it for QEMU.
        let fd = unsafe { (self.libvirt.graphics)(self.domain, index, 1) };
        if fd < 0 {
            return Err(self.libvirt.error("Could not attach to the guest display"));
        }
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

impl Drop for GraphicsFd {
    fn drop(&mut self) {
        unsafe {
            if !self.domain.is_null() {
                (self.libvirt.free)(self.domain);
            }
            (self.libvirt.close)(self.connection);
        }
    }
}
