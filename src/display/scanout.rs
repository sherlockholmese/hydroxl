//! EGL readback for SPICE's local GL scanouts. All EGL resources stay on the SPICE worker.
use super::{
    damage::Damage,
    vnc::{Framebuffer, MAX_DESKTOP_SIZE},
};
use anyhow::{Context, Result, ensure};
use gpui_kit::{Bounds, DevicePixels, DmaBufFrame, point, size};
use libloading::Library;
use std::future::Future;
use std::os::fd::BorrowedFd;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll, Waker};
use std::{
    cell::OnceCell,
    ffi::{CStr, c_char, c_void},
    ptr::{null, null_mut},
    rc::Rc,
};

type Handle = *mut c_void;
type Proc = unsafe extern "C" fn(*const c_char) -> *const c_void;
const EGL_NONE: i32 = 0x3038;
const DMA_BUF_TARGET: u32 = 0x3270;
const TEXTURE_2D: u32 = 0x0de1;
const FRAMEBUFFER: u32 = 0x8d40;
const COLOR_ATTACHMENT: u32 = 0x8ce0;
const RGBA: u32 = 0x1908;
const UNSIGNED_BYTE: u32 = 0x1401;
const INVALID_MODIFIER: u64 = (1 << 56) - 1;
const PLANE_ATTRS: [[i32; 5]; 4] = [
    [0x3272, 0x3273, 0x3274, 0x3443, 0x3444],
    [0x3275, 0x3276, 0x3277, 0x3445, 0x3446],
    [0x3278, 0x3279, 0x327a, 0x3447, 0x3448],
    [0x3440, 0x3441, 0x3442, 0x3449, 0x344a],
];

#[repr(C)]
pub(super) struct LegacyScanout {
    pub fd: i32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
    pub top_down: i32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub(super) struct Scanout {
    pub fds: [i32; 4],
    pub width: u32,
    pub height: u32,
    pub offsets: [u32; 4],
    pub strides: [u32; 4],
    pub planes: u32,
    pub format: u32,
    pub top_down: i32,
    pub modifier: u64,
}

impl Scanout {
    pub(super) fn rows_are_top_down(self) -> bool {
        // GPU image rows run opposite to the producer's Y origin convention.
        self.top_down == 0
    }
}

impl From<&LegacyScanout> for Scanout {
    fn from(old: &LegacyScanout) -> Self {
        Self {
            fds: [old.fd, -1, -1, -1],
            width: old.width,
            height: old.height,
            offsets: [0; 4],
            strides: [old.stride, 0, 0, 0],
            planes: 1,
            format: old.format,
            top_down: old.top_down,
            modifier: INVALID_MODIFIER,
        }
    }
}

macro_rules! egl_api {
    ($($name:ident: $ty:ty),* $(,)?) => {
        #[allow(non_snake_case)]
        struct Egl {
            _library: Library,
            $($name: $ty,)*
        }
        impl Egl {
            fn load() -> Result<Self> {
                let library = unsafe { Library::new("libEGL.so.1") }?;
                let get = *unsafe { library.get::<Proc>(b"eglGetProcAddress") }?;
                Ok(Self {
                    $($name: {
                        let address = unsafe { get(concat!(stringify!($name), "\0").as_ptr().cast()) };
                        ensure!(!address.is_null(), "Missing EGL API {}", stringify!($name));
                        // EGLGetProcAddress returns the ABI of the named Khronos API.
                        unsafe { std::mem::transmute::<*const c_void, $ty>(address) }
                    },)*
                    _library: library,
                })
            }
        }
    }
}

egl_api! {
    eglGetPlatformDisplay: unsafe extern "C" fn(u32, Handle, *const isize) -> Handle,
    eglInitialize: unsafe extern "C" fn(Handle, *mut i32, *mut i32) -> u32,
    eglTerminate: unsafe extern "C" fn(Handle) -> u32,
    eglQueryString: unsafe extern "C" fn(Handle, i32) -> *const c_char,
    eglBindAPI: unsafe extern "C" fn(u32) -> u32,
    eglChooseConfig: unsafe extern "C" fn(Handle, *const i32, *mut Handle, i32, *mut i32) -> u32,
    eglCreateContext: unsafe extern "C" fn(Handle, Handle, Handle, *const i32) -> Handle,
    eglDestroyContext: unsafe extern "C" fn(Handle, Handle) -> u32,
    eglCreatePbufferSurface: unsafe extern "C" fn(Handle, Handle, *const i32) -> Handle,
    eglDestroySurface: unsafe extern "C" fn(Handle, Handle) -> u32,
    eglMakeCurrent: unsafe extern "C" fn(Handle, Handle, Handle, Handle) -> u32,
    eglCreateImageKHR: unsafe extern "C" fn(Handle, Handle, u32, Handle, *const i32) -> Handle,
    eglDestroyImageKHR: unsafe extern "C" fn(Handle, Handle) -> u32,
    glGenTextures: unsafe extern "C" fn(i32, *mut u32),
    glBindTexture: unsafe extern "C" fn(u32, u32),
    glDeleteTextures: unsafe extern "C" fn(i32, *const u32),
    glEGLImageTargetTexture2DOES: unsafe extern "C" fn(u32, Handle),
    glGenFramebuffers: unsafe extern "C" fn(i32, *mut u32),
    glBindFramebuffer: unsafe extern "C" fn(u32, u32),
    glFramebufferTexture2D: unsafe extern "C" fn(u32, u32, u32, u32, i32),
    glCheckFramebufferStatus: unsafe extern "C" fn(u32) -> u32,
    glDeleteFramebuffers: unsafe extern "C" fn(i32, *const u32),
    glReadPixels: unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, Handle),
    glGetError: unsafe extern "C" fn() -> u32,
}

pub(super) struct Renderer {
    egl: Egl,
    display: Handle,
    surface: Handle,
    context: Handle,
    modifiers: bool,
}

thread_local! {
    static RENDERER: OnceCell<Result<Rc<Renderer>, String>> = const { OnceCell::new() };
}

pub(super) fn renderer() -> Result<Rc<Renderer>> {
    RENDERER.with(|slot| {
        slot.get_or_init(|| Renderer::new(true).map(Rc::new).map_err(|error| error.to_string()))
            .clone()
            .map_err(anyhow::Error::msg)
    })
}

impl Renderer {
    fn new(require_dma_buf: bool) -> Result<Self> {
        let egl = Egl::load().context("EGL is unavailable.")?;
        // A surfaceless display prevents a second window system connection.
        let display = unsafe { (egl.eglGetPlatformDisplay)(0x31dd, null_mut(), null()) };
        ensure!(!display.is_null(), "Could not choose a surfaceless EGL display.");
        ensure!(
            unsafe { (egl.eglInitialize)(display, null_mut(), null_mut()) } != 0,
            "Could not initialize the EGL display."
        );
        let mut renderer = Self {
            egl,
            display,
            surface: null_mut(),
            context: null_mut(),
            modifiers: false,
        };
        let egl = &renderer.egl;
        let extensions = unsafe { (egl.eglQueryString)(display, 0x3055) };
        ensure!(!extensions.is_null(), "EGL did not report its extensions.");
        let extensions = unsafe { CStr::from_ptr(extensions) }.to_string_lossy();
        let supports =
            |extension: &str| extensions.split_ascii_whitespace().any(|e| e == extension);
        ensure!(
            !require_dma_buf || supports("EGL_EXT_image_dma_buf_import"),
            "EGL DMA-BUF import is unavailable."
        );
        renderer.modifiers = supports("EGL_EXT_image_dma_buf_import_modifiers");
        let mut config = null_mut();
        let mut count = 0;
        // EGL_PBUFFER_BIT and EGL_OPENGL_ES2_BIT.
        let attrs = [0x3033, 1, 0x3040, 4, EGL_NONE];
        unsafe {
            ensure!((egl.eglBindAPI)(0x30a0) != 0, "Could not select OpenGL ES.");
            ensure!(
                (egl.eglChooseConfig)(display, attrs.as_ptr(), &mut config, 1, &mut count) != 0
                    && count > 0,
                "No compatible EGL configuration was found."
            );
            renderer.surface = (egl.eglCreatePbufferSurface)(
                display,
                config,
                [0x3057, 1, 0x3056, 1, EGL_NONE].as_ptr(),
            );
            renderer.context =
                (egl.eglCreateContext)(display, config, null_mut(), [0x3098, 2, EGL_NONE].as_ptr());
            ensure!(
                !renderer.surface.is_null()
                    && !renderer.context.is_null()
                    && (egl.eglMakeCurrent)(
                        display,
                        renderer.surface,
                        renderer.surface,
                        renderer.context
                    ) != 0,
                "Could not start the SPICE GL renderer."
            );
        }
        Ok(renderer)
    }

    pub(super) fn import(self: &Rc<Self>, scanout: Scanout) -> Result<Imported> {
        let attrs = attributes(scanout, self.modifiers)?;
        let mut imported = Imported {
            renderer: self.clone(),
            image: null_mut(),
            texture: 0,
            framebuffer: 0,
            width: scanout.width as usize,
            height: scanout.height as usize,
            top_down: scanout.rows_are_top_down(),
        };
        let egl = &self.egl;
        unsafe {
            imported.image = (egl.eglCreateImageKHR)(
                self.display,
                null_mut(),
                DMA_BUF_TARGET,
                null_mut(),
                attrs.as_ptr(),
            );
            ensure!(!imported.image.is_null(), "EGL could not import the SPICE GPU buffer.");
            (egl.glGenTextures)(1, &mut imported.texture);
            (egl.glBindTexture)(TEXTURE_2D, imported.texture);
            (egl.glEGLImageTargetTexture2DOES)(TEXTURE_2D, imported.image);
            (egl.glGenFramebuffers)(1, &mut imported.framebuffer);
            (egl.glBindFramebuffer)(FRAMEBUFFER, imported.framebuffer);
            (egl.glFramebufferTexture2D)(
                FRAMEBUFFER,
                COLOR_ATTACHMENT,
                TEXTURE_2D,
                imported.texture,
                0,
            );
            ensure!(
                (egl.glGetError)() == 0 && (egl.glCheckFramebufferStatus)(FRAMEBUFFER) == 0x8cd5,
                "The SPICE GPU buffer cannot be read by this EGL renderer."
            );
        }
        Ok(imported)
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            (self.egl.eglMakeCurrent)(self.display, null_mut(), null_mut(), null_mut());
            if !self.context.is_null() {
                (self.egl.eglDestroyContext)(self.display, self.context);
            }
            if !self.surface.is_null() {
                (self.egl.eglDestroySurface)(self.display, self.surface);
            }
            (self.egl.eglTerminate)(self.display);
        }
    }
}

pub(super) struct Imported {
    renderer: Rc<Renderer>,
    image: Handle,
    texture: u32,
    framebuffer: u32,
    pub width: usize,
    pub height: usize,
    top_down: bool,
}

impl Imported {
    pub(super) fn read(&self, rect: Damage, pixels: &mut Vec<u8>) -> Result<()> {
        let (width, height) = (rect.width as usize, rect.height as usize);
        let (x, y) = (rect.x as usize, rect.y as usize);
        ensure!(
            x + width <= self.width && y + height <= self.height,
            "Invalid SPICE readback bounds."
        );
        pixels.resize(width * height * 4, 0);
        let gl_y = if self.top_down {
            y
        } else {
            self.height - y - height
        };
        let egl = &self.renderer.egl;
        unsafe {
            (egl.glBindFramebuffer)(FRAMEBUFFER, self.framebuffer);
            (egl.glReadPixels)(
                x as i32,
                gl_y as i32,
                width as i32,
                height as i32,
                RGBA,
                UNSIGNED_BYTE,
                pixels.as_mut_ptr().cast(),
            );
            ensure!((egl.glGetError)() == 0, "Could not read the SPICE GPU buffer.");
        }
        for rgba in pixels.chunks_exact_mut(4) {
            rgba.swap(0, 2);
            rgba[3] = 255;
        }
        if !self.top_down {
            let stride = width * 4;
            for row in 0..height / 2 {
                let end = (height - 1 - row) * stride;
                let (upper, lower) = pixels.split_at_mut(end);
                upper[row * stride..(row + 1) * stride].swap_with_slice(&mut lower[..stride]);
            }
        }
        Ok(())
    }
}

impl Drop for Imported {
    fn drop(&mut self) {
        let egl = &self.renderer.egl;
        unsafe {
            (egl.glDeleteFramebuffers)(1, &self.framebuffer);
            (egl.glDeleteTextures)(1, &self.texture);
            if !self.image.is_null() {
                (egl.eglDestroyImageKHR)(self.renderer.display, self.image);
            }
        }
    }
}

fn attributes(scanout: Scanout, modifiers: bool) -> Result<Vec<i32>> {
    let max = MAX_DESKTOP_SIZE as u32;
    ensure!(
        (1..=max).contains(&scanout.width) && (1..=max).contains(&scanout.height),
        "Invalid SPICE scanout size."
    );
    ensure!((1..=4).contains(&scanout.planes), "Invalid SPICE scanout plane count.");
    let valid_modifier = scanout.modifier != INVALID_MODIFIER && scanout.modifier != u64::MAX;
    ensure!(
        !valid_modifier || scanout.modifier == 0 || modifiers,
        "EGL does not support this DMA-BUF modifier."
    );
    let mut attrs = vec![
        0x3057,
        scanout.width as i32,
        0x3056,
        scanout.height as i32,
        0x3271,
        scanout.format as i32,
    ];
    for (index, [fd, offset, pitch, modifier_lo, modifier_hi]) in
        PLANE_ATTRS.into_iter().take(scanout.planes as usize).enumerate()
    {
        ensure!(
            scanout.fds[index] >= 0 && scanout.strides[index] != 0,
            "Invalid SPICE scanout plane."
        );
        attrs.extend([
            fd,
            scanout.fds[index],
            offset,
            i32::try_from(scanout.offsets[index])?,
            pitch,
            i32::try_from(scanout.strides[index])?,
        ]);
        if valid_modifier && modifiers {
            attrs.extend([
                modifier_lo,
                scanout.modifier as i32,
                modifier_hi,
                (scanout.modifier >> 32) as i32,
            ]);
        }
    }
    attrs.push(EGL_NONE);
    Ok(attrs)
}

pub(crate) struct GpuDraw {
    pub frame: DmaBufFrame,
    pub reply: DrawReply,
}

#[derive(Default)]
struct ReplyState {
    copied: Option<bool>,
    waker: Option<Waker>,
}

/// The producer's side of a GPU draw, answered once the UI's copy completes. Dropping
/// it unanswered reports a failed copy, so the producer is never left waiting.
pub(crate) struct DrawReply {
    state: Arc<Mutex<ReplyState>>,
    /// Wakes the producer's thread so it acknowledges without polling.
    wake: Option<Box<dyn Fn() + Send>>,
    sent: bool,
}

impl DrawReply {
    /// Reports whether the frame was copied; the producer then releases the buffer.
    pub(crate) fn finish(mut self, copied: bool) {
        self.send(copied);
    }

    fn send(&mut self, copied: bool) {
        if std::mem::replace(&mut self.sent, true) {
            return;
        }
        let waker = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.copied = Some(copied);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        if let Some(wake) = &self.wake {
            wake();
        }
    }
}

impl Drop for DrawReply {
    fn drop(&mut self) {
        self.send(false);
    }
}

/// The producer's view of a draw it offered: polled by the SPICE loop, awaited by D-Bus.
pub(super) struct DrawOutcome(Arc<Mutex<ReplyState>>);

impl DrawOutcome {
    pub(super) fn try_take(&self) -> Option<bool> {
        self.0.lock().unwrap_or_else(|error| error.into_inner()).copied
    }
}

impl Future for DrawOutcome {
    type Output = bool;

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<bool> {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        match state.copied {
            Some(copied) => Poll::Ready(copied),
            None => {
                state.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

/// The scanout as a frame the UI can copy on the GPU, when it has a usable layout.
fn gpu_frame(scanout: Scanout, damage: Option<Damage>) -> Option<DmaBufFrame> {
    if scanout.planes != 1
        || scanout.fds[0] < 0
        || scanout.modifier == (1 << 56) - 1
        || scanout.modifier == u64::MAX
        || !(1..=MAX_DESKTOP_SIZE as u32).contains(&scanout.width)
        || !(1..=MAX_DESKTOP_SIZE as u32).contains(&scanout.height)
    {
        return None;
    }
    // Our own reference, held while the producer awaits the draw acknowledgment.
    let fd = unsafe { BorrowedFd::borrow_raw(scanout.fds[0]) }.try_clone_to_owned().ok()?;
    Some(DmaBufFrame {
        fd,
        extent: size(DevicePixels(scanout.width as i32), DevicePixels(scanout.height as i32)),
        fourcc: scanout.format,
        modifier: scanout.modifier,
        offset: scanout.offsets[0],
        stride: scanout.strides[0],
        top_down: scanout.rows_are_top_down(),
        damage: damage.map(|rect| {
            Bounds::new(
                point(DevicePixels(rect.x as i32), DevicePixels(rect.y as i32)),
                size(DevicePixels(rect.width as i32), DevicePixels(rect.height as i32)),
            )
        }),
        on_complete: None,
    })
}

/// Hands the scanout to the UI's next frame without waiting for it. `None` means the
/// layout cannot be copied on the GPU. Only `damage` needs copying when the UI already
/// shows the previous frame.
pub(super) fn offer_scanout(
    shared: &Mutex<Framebuffer>,
    scanout: Scanout,
    damage: Option<Damage>,
    wake: Option<Box<dyn Fn() + Send>>,
) -> Option<DrawOutcome> {
    let frame = gpu_frame(scanout, damage)?;
    let state = Arc::new(Mutex::new(ReplyState::default()));
    let waker = {
        let mut shared = shared.lock().unwrap_or_else(|error| error.into_inner());
        shared.gpu_draw = Some(GpuDraw {
            frame,
            reply: DrawReply {
                state: state.clone(),
                wake,
                sent: false,
            },
        });
        shared.draw_waker.take()
    };
    if let Some(waker) = waker {
        waker.wake();
    }
    Some(DrawOutcome(state))
}

/// Resolves once a GPU draw is waiting for the UI. Like UTM, the UI copies a frame as
/// soon as it arrives and presents at its own pace, so the guest is never held until
/// the next vsync before it may render again.
pub(crate) struct DrawOffered<'a>(pub &'a Mutex<Framebuffer>);

impl Future for DrawOffered<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<()> {
        let mut frame = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if frame.gpu_draw.is_some() {
            Poll::Ready(())
        } else {
            frame.draw_waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

/// Withdraws an offer the UI has not taken. False once the UI owns the copy.
pub(super) fn withdraw_scanout(shared: &Mutex<Framebuffer>) -> bool {
    let draw = shared.lock().unwrap_or_else(|error| error.into_inner()).gpu_draw.take();
    draw.is_some()
}

/// Records a GPU-copied scanout as the current frame.
pub(super) fn scanout_copied(shared: &Mutex<Framebuffer>, scanout: Scanout) {
    let mut frame = shared.lock().unwrap_or_else(|error| error.into_inner());
    frame.width = scanout.width;
    frame.height = scanout.height;
    frame.display_error = None;
    frame.pixels.clear();
    frame.discard_damage();
    frame.generation += 1;
}

/// Whether the producer's pending GPU writes to the buffer have finished.
pub(super) fn writes_finished(fd: i32) -> bool {
    if fd < 0 {
        return true;
    }
    let mut descriptor = PollFd {
        fd,
        events: 1, // POLLIN
        revents: 0,
    };
    unsafe { poll(&mut descriptor, 1, 0) == 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_modifier_and_rejects_unusable_scanouts() {
        let mut scanout = Scanout::from(&LegacyScanout {
            fd: 7,
            width: 1280,
            height: 720,
            stride: 5120,
            format: u32::from_le_bytes(*b"XR24"),
            top_down: 1,
        });
        assert_eq!(attributes(scanout, false).unwrap().last(), Some(&EGL_NONE));
        scanout.modifier = 0x01000000_00000004;
        assert!(attributes(scanout, false).is_err());
        let attrs = attributes(scanout, true).unwrap();
        assert!(attrs.windows(4).any(|p| p == [0x3443, 4, 0x3444, 0x01000000]));
        scanout.planes = 5;
        assert!(attributes(scanout, true).is_err());
    }

    #[test]
    #[ignore = "requires the EGL runtime"]
    fn software_egl_readback_produces_bgra_pixels() -> Result<()> {
        let renderer = Rc::new(Renderer::new(false)?);
        let get = *unsafe { renderer.egl._library.get::<Proc>(b"eglGetProcAddress") }?;
        let function = unsafe { get(c"glClearColor".as_ptr()) };
        ensure!(!function.is_null(), "Could not load glClearColor.");
        let clear_color = unsafe {
            std::mem::transmute::<*const c_void, unsafe extern "C" fn(f32, f32, f32, f32)>(function)
        };
        let function = unsafe { get(c"glClear".as_ptr()) };
        ensure!(!function.is_null(), "Could not load glClear.");
        let clear =
            unsafe { std::mem::transmute::<*const c_void, unsafe extern "C" fn(u32)>(function) };
        unsafe {
            clear_color(1., 0., 0., 1.);
            clear(0x4000);
        }
        let surface = Imported {
            renderer,
            image: null_mut(),
            framebuffer: 0,
            texture: 0,
            width: 1,
            height: 1,
            top_down: true,
        };
        let mut pixels = Vec::new();
        surface.read(Damage::new(0, 0, 1, 1), &mut pixels)?;
        assert_eq!(pixels, [0, 0, 255, 255]);
        Ok(())
    }
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

unsafe extern "C" {
    fn poll(fds: *mut PollFd, count: u64, timeout: i32) -> i32;
}

/// Waits until the producer's GPU writes to a DMA-BUF have finished. Linux signals
/// readability on the buffer once its implicit write fences complete, so a read that
/// follows sees the finished frame rather than whatever was there before.
pub(super) fn wait_for_writes(fd: i32, timeout_ms: i32) {
    if fd < 0 {
        return;
    }
    let mut descriptor = PollFd {
        fd,
        events: 1, // POLLIN
        revents: 0,
    };
    unsafe { poll(&mut descriptor, 1, timeout_ms) };
}
