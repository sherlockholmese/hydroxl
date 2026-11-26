//! QEMU Display1 over two local libvirt-authorized peer-to-peer D-Bus sockets.
mod clipboard;

use super::{
    damage::Damage,
    frame::{Cursor, Framebuffer, MAX_DESKTOP_SIZE},
    graphics_fd::GraphicsFd,
    keys::scancode,
    scanout,
    spice::Input,
    thumbnail,
};
use crate::backend::Connection;
use anyhow::{Context, Result};
use std::{
    collections::BTreeSet,
    os::{fd::AsFd, unix::net::UnixStream},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use zbus::{
    blocking::{Connection as Peer, connection::Builder},
    fdo,
    zvariant::{Fd, OwnedFd, OwnedValue},
};

const CONSOLE: &str = "org.qemu.Display1.Console";
const KEYBOARD: &str = "org.qemu.Display1.Keyboard";
const MOUSE: &str = "org.qemu.Display1.Mouse";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const LISTENER: &str = "/org/qemu/Display1/Listener";
const LISTENER_V2: &str = "org.qemu.Display1.Listener.Unix.ScanoutDMABUF2";
type Frame = Arc<Mutex<Framebuffer>>;

pub(crate) struct Session {
    pub frame: Frame,
    commands: mpsc::Sender<Option<Input>>,
    clipboard: Option<mpsc::Sender<clipboard::Command>>,
    usb: Option<(Connection, String, Arc<Mutex<()>>)>,
}

impl Session {
    pub fn connect(
        connection: Connection,
        id: &str,
        graphics: u32,
        gpu: bool,
        share_clipboard: bool,
        usb: bool,
    ) -> Result<Self> {
        let stream = UnixStream::from(GraphicsFd::new(connection.uri(), id)?.open(graphics)?);
        let frame = Frame::default();
        let clipboard = share_clipboard.then(|| clipboard::Clipboard::new(frame.clone()));
        let mut builder = peer(stream);
        if let Some(clipboard) = &clipboard {
            builder = builder.serve_at(clipboard::PATH, clipboard.clone())?;
        }
        let control = builder.build().context("Could not connect to the QEMU display.")?;
        let consoles: OwnedValue = control
            .call_method(
                None::<&str>,
                "/org/qemu/Display1/VM",
                Some(PROPERTIES),
                "Get",
                &("org.qemu.Display1.VM", "ConsoleIDs"),
            )?
            .body()
            .deserialize()?;
        let consoles = Vec::<u32>::try_from(consoles)?;
        let console = *consoles.first().context("QEMU has no display console.")?;
        let path = format!("/org/qemu/Display1/Console_{console}");
        let (listener, qemu) = UnixStream::pair()?;
        let display = Listener {
            frame: frame.clone(),
            scanout: Arc::new(Mutex::new(None)),
            gpu,
        };
        let listener_builder = peer(listener)
            .serve_at(LISTENER, display.clone())?
            .serve_at(LISTENER, ListenerV2(display))?;
        control
            .call_method(
                None::<&str>,
                path.as_str(),
                Some(CONSOLE),
                "RegisterListener",
                &(Fd::from(qemu.as_fd()),),
            )
            .context("QEMU rejected the display listener.")?;
        drop(qemu);
        let listener =
            listener_builder.build().context("Could not open the QEMU display listener.")?;
        let clipboard = clipboard.and_then(|clipboard| match clipboard.connect(&control) {
            Ok(commands) => Some(commands),
            Err(error) => {
                eprintln!("Could not register QEMU clipboard sharing: {error:#}");
                None
            }
        });
        let (commands, receive) = mpsc::channel();
        let state = frame.clone();
        std::thread::Builder::new()
            .name("dbus-display".into())
            .spawn(move || {
                InputSink {
                    control,
                    path,
                    keys: BTreeSet::new(),
                    buttons: 0,
                    previous: None,
                }
                .run(receive, &state);
                let _ = listener.close();
                let mut frame = state.lock().unwrap_or_else(|error| error.into_inner());
                frame.closed = Some("The QEMU display disconnected.".into());
                frame.generation = frame.generation.wrapping_add(1);
            })
            .context("Could not start the QEMU display client.")?;
        Ok(Self {
            frame,
            commands,
            clipboard,
            usb: usb.then(|| (connection, id.into(), Arc::default())),
        })
    }

    pub(super) fn send(&self, command: Input) {
        let _ = self.commands.send(Some(command));
    }

    pub(super) fn send_clipboard(&self, text: String) {
        if let Some(clipboard) = &self.clipboard {
            let _ = clipboard.send(clipboard::Command::Offer(text));
        }
    }

    pub(super) fn read_clipboard(&self) -> Result<mpsc::Receiver<Result<String>>> {
        let clipboard =
            self.clipboard.as_ref().context("QEMU clipboard sharing is unavailable.")?;
        let (send, receive) = mpsc::channel();
        clipboard.send(clipboard::Command::Read(send)).context("The QEMU display disconnected.")?;
        Ok(receive)
    }

    pub(super) fn can_toggle_usb(&self) -> bool {
        self.usb.is_some()
    }

    pub(super) fn toggle_usb(&self, bus: u8, address: u8) -> Result<mpsc::Receiver<Result<bool>>> {
        let (connection, id, lock) =
            self.usb.clone().context("The guest needs a USB controller.")?;
        let (send, receive) = mpsc::channel();
        std::thread::Builder::new()
            .name("display-usb".into())
            .spawn(move || {
                let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
                let _ = send.send(crate::operations::toggle_live_usb(
                    connection,
                    &id,
                    u16::from(bus),
                    u16::from(address),
                ));
            })
            .context("Could not start the USB device change.")?;
        Ok(receive)
    }

    pub fn close(&self) {
        if let Some(clipboard) = &self.clipboard {
            let _ = clipboard.send(clipboard::Command::Close);
        }
        let _ = self.commands.send(None);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

fn peer(stream: UnixStream) -> Builder<'static> {
    Builder::async_io_unix_stream(stream).p2p().method_timeout(Duration::from_secs(2))
}

struct InputSink {
    control: Peer,
    path: String,
    keys: BTreeSet<u32>,
    buttons: u8,
    previous: Option<(u16, u16)>,
}

impl InputSink {
    fn run(mut self, receive: mpsc::Receiver<Option<Input>>, frame: &Mutex<Framebuffer>) {
        let mut last_update = None;
        loop {
            if last_update
                .is_none_or(|last: std::time::Instant| last.elapsed() >= Duration::from_secs(1))
            {
                let absolute = (|| -> Result<bool> {
                    let reply = self.control.call_method(
                        None::<&str>,
                        self.path.as_str(),
                        Some(PROPERTIES),
                        "Get",
                        &(MOUSE, "IsAbsolute"),
                    )?;
                    Ok(bool::try_from(reply.body().deserialize::<OwnedValue>()?)?)
                })();
                let Ok(absolute) = absolute else {
                    break;
                };
                let mut frame = frame.lock().unwrap_or_else(|error| error.into_inner());
                if frame.absolute != Some(absolute) {
                    self.previous = None;
                    frame.absolute = Some(absolute);
                    frame.generation += 1;
                }
                frame.dbus_resize = true;
                last_update = Some(std::time::Instant::now());
            }
            match receive.recv_timeout(Duration::from_millis(200)) {
                Ok(Some(input)) => {
                    let absolute = frame.lock().unwrap_or_else(|error| error.into_inner()).absolute
                        == Some(true);
                    if self.input(input, absolute).is_err() {
                        break;
                    }
                }
                Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = self.input(Input::Release, false);
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        let _ = self.control.close();
    }

    fn input(&mut self, input: Input, absolute: bool) -> Result<()> {
        match input {
            Input::Key(symbol, down) => {
                if let Some(code) = scancode(symbol) {
                    // QEMU key numbers represent E0 in the high bit of the low byte.
                    let code = (code & 0x7f) | ((code >> 1) & 0x80);
                    self.send(
                        KEYBOARD,
                        if down {
                            "Press"
                        } else {
                            "Release"
                        },
                        &(code,),
                    )?;
                    if down {
                        self.keys.insert(code);
                    } else {
                        self.keys.remove(&code);
                    }
                }
            }
            Input::Pointer(x, y, buttons) => {
                let old = self.previous.replace((x, y));
                if absolute {
                    self.send(MOUSE, "SetAbsPosition", &(u32::from(x), u32::from(y)))?;
                } else if let Some((px, py)) = old {
                    self.send(
                        MOUSE,
                        "RelMotion",
                        &(i32::from(x) - i32::from(px), i32::from(y) - i32::from(py)),
                    )?;
                }
                self.buttons(buttons)?;
            }
            Input::Relative(dx, dy, buttons) => {
                if !absolute {
                    self.send(MOUSE, "RelMotion", &(dx, dy))?;
                }
                self.buttons(buttons)?;
            }
            Input::ResetPointer => self.previous = None,
            Input::Release => {
                self.previous = None;
                for code in std::mem::take(&mut self.keys) {
                    self.send(KEYBOARD, "Release", &(code,))?;
                }
                self.buttons(0)?;
            }
            Input::Resize(w, h) => self.send(
                CONSOLE,
                "SetUIInfo",
                &(0u16, 0u16, 0i32, 0i32, u32::from(w), u32::from(h)),
            )?,
            _ => {}
        }
        Ok(())
    }

    fn send<T: serde::Serialize + zbus::zvariant::DynamicType>(
        &self,
        interface: &str,
        method: &str,
        data: &T,
    ) -> Result<()> {
        let message = zbus::Message::method_call(self.path.as_str(), method)?
            .interface(interface)?
            .with_flags(zbus::message::Flags::NoReplyExpected)?
            .build(data)?;
        Ok(self.control.send(&message)?)
    }

    fn buttons(&mut self, buttons: u8) -> Result<()> {
        let changed = std::mem::replace(&mut self.buttons, buttons) ^ buttons;
        for (bit, button) in [(1, 0u32), (2, 1), (4, 2), (8, 3), (16, 4)] {
            if changed & bit != 0 {
                self.send(
                    MOUSE,
                    if buttons & bit != 0 {
                        "Press"
                    } else {
                        "Release"
                    },
                    &(button,),
                )?;
            }
        }
        Ok(())
    }
}

struct Buffer {
    _fds: Vec<OwnedFd>,
    surface: scanout::Scanout,
    crop: Damage,
    fresh: bool,
    gpu_failed: bool,
}

#[derive(Clone)]
struct Listener {
    frame: Frame,
    scanout: Arc<Mutex<Option<Buffer>>>,
    gpu: bool,
}

#[zbus::interface(name = "org.qemu.Display1.Listener", spawn = false)]
impl Listener {
    #[zbus(property)]
    fn interfaces(&self) -> Vec<&'static str> {
        vec![LISTENER_V2]
    }

    fn scanout(
        &self,
        width: u32,
        height: u32,
        stride: u32,
        pixman_format: u32,
        data: Vec<u8>,
    ) -> fdo::Result<()> {
        self.scanout.lock().unwrap_or_else(|error| error.into_inner()).take();
        self.software_scanout((0, 0, width, height), stride, pixman_format, &data, true)
    }

    // QEMU's Display1 D-Bus signature fixes the number and order of these arguments.
    #[allow(clippy::too_many_arguments)]
    fn update(
        &self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        stride: u32,
        pixman_format: u32,
        data: Vec<u8>,
    ) -> fdo::Result<()> {
        let [x, y, width, height] = [x, y, width, height].map(|value| value.max(0) as u32);
        self.software_scanout((x, y, width, height), stride, pixman_format, &data, false)
    }

    #[allow(clippy::too_many_arguments)]
    #[zbus(name = "ScanoutDMABUF")]
    async fn scanout_dmabuf(
        &self,
        dma_buf: OwnedFd,
        width: u32,
        height: u32,
        stride: u32,
        fourcc: u32,
        modifier: u64,
        y0_top: bool,
    ) -> fdo::Result<()> {
        let buffer = buffer(
            vec![dma_buf],
            [width, height],
            [0, 0, width, height],
            vec![0],
            vec![stride],
            fourcc,
            modifier,
            y0_top,
        )?;
        self.install_buffer(buffer).await
    }

    #[zbus(name = "UpdateDMABUF")]
    async fn update_dmabuf(&self, x: i32, y: i32, width: i32, height: i32) -> fdo::Result<()> {
        let [x, y, width, height] = [x, y, width, height].map(|value| value.max(0) as usize);
        self.draw_buffer(Some(Damage::new(x, y, width, height))).await
    }

    fn disable(&self) {
        self.scanout.lock().unwrap_or_else(|error| error.into_inner()).take();
    }

    fn mouse_set(&self, _x: i32, _y: i32, on: i32) {
        if on == 0 {
            let mut frame = self.frame.lock().unwrap_or_else(|error| error.into_inner());
            frame.cursor = None;
            frame.cursor_generation += 1;
            frame.generation += 1;
        }
    }

    fn cursor_define(
        &self,
        width: i32,
        height: i32,
        hot_x: i32,
        hot_y: i32,
        data: Vec<u8>,
    ) -> fdo::Result<()> {
        if !(1..=1024).contains(&width)
            || !(1..=1024).contains(&height)
            || data.len() != (width * height * 4) as usize
        {
            return Err(invalid());
        }
        let mut frame = self.frame.lock().unwrap_or_else(|error| error.into_inner());
        frame.cursor = Some(Cursor {
            pixels: data,
            width: width as u32,
            height: height as u32,
            hot_x: hot_x.clamp(0, width - 1) as u32,
            hot_y: hot_y.clamp(0, height - 1) as u32,
        });
        frame.cursor_generation += 1;
        frame.generation += 1;
        Ok(())
    }
}

impl Listener {
    async fn install_buffer(&self, buffer: Buffer) -> fdo::Result<()> {
        *self.scanout.lock().unwrap_or_else(|error| error.into_inner()) = Some(buffer);
        // Scanout is also a draw. A desktop at rest may never send UpdateDMABUF.
        self.draw_buffer(None).await
    }

    async fn draw_buffer(&self, damage: Option<Damage>) -> fdo::Result<()> {
        // Copy what the draw needs; the buffer stays installed while this call is
        // pending because the listener handles one message at a time.
        let (surface, crop, fresh, try_gpu) = {
            let mut scanout = self.scanout.lock().unwrap_or_else(|error| error.into_inner());
            let Some(buffer) = scanout.as_mut() else {
                return Ok(());
            };
            (
                buffer.surface,
                buffer.crop,
                std::mem::take(&mut buffer.fresh),
                self.gpu && !buffer.gpu_failed,
            )
        };
        let full = crop == Damage::new(0, 0, surface.width as usize, surface.height as usize);
        // Returning from this method acknowledges the draw. Await the UI's GPU copy
        // instead of blocking the connection's thread, so replies to our input calls
        // keep arriving while a frame is in flight.
        let mut gpu = false;
        if try_gpu && full {
            let damage = if fresh {
                None
            } else {
                damage
            };
            if let Some(outcome) = scanout::offer_scanout(&self.frame, surface, damage, None) {
                gpu = outcome.await;
            }
            if gpu {
                scanout::scanout_copied(&self.frame, surface);
            } else if let Some(buffer) =
                self.scanout.lock().unwrap_or_else(|error| error.into_inner()).as_mut()
            {
                buffer.gpu_failed = true;
            }
        }
        if fresh && std::env::var_os("HYDROXL_DEBUG_DISPLAY").is_some() {
            eprintln!(
                "D-Bus scanout {}x{}, buffer {}x{}, fourcc {:?}, modifier {:#x}, {}",
                crop.width,
                crop.height,
                surface.width,
                surface.height,
                String::from_utf8_lossy(&surface.format.to_le_bytes()),
                surface.modifier,
                if gpu {
                    "GPU copy"
                } else {
                    "EGL readback"
                },
            );
        }
        let want_preview = gpu
            && std::mem::take(
                &mut self.frame.lock().unwrap_or_else(|error| error.into_inner()).preview_requested,
            );
        if gpu && !want_preview {
            return Ok(());
        }
        let readback = (|| -> Result<Vec<u8>> {
            let renderer = scanout::renderer()?;
            let imported = renderer.import(surface)?;
            let mut pixels = Vec::new();
            scanout::wait_for_writes(surface.fds[0], 100);
            imported.read(crop, &mut pixels)?;
            Ok(pixels)
        })();
        let mut frame = self.frame.lock().unwrap_or_else(|error| error.into_inner());
        match readback {
            Ok(pixels) if gpu => {
                frame.thumbnail = thumbnail::sample(crop.width, crop.height, &pixels)
            }
            Ok(pixels) => {
                frame.width = crop.width;
                frame.height = crop.height;
                frame.pixels = pixels;
                frame.damage_all();
                frame.generation += 1;
            }
            Err(error) if !gpu => {
                frame.closed = Some(format!("Could not import the QEMU scanout. {error:#}"));
                frame.generation += 1;
                return Err(fdo::Error::Failed(error.to_string()));
            }
            Err(_) => {}
        }
        Ok(())
    }

    fn software_scanout(
        &self,
        rect: (u32, u32, u32, u32),
        stride: u32,
        format: u32,
        data: &[u8],
        fresh: bool,
    ) -> fdo::Result<()> {
        let (x, y, width, height) = rect;
        if !dimensions(width, height)
            || !matches!(format, 0x2002_8888 | 0x2002_0888)
            || stride < width * 4
            || u64::from(stride) * u64::from(height - 1) + u64::from(width * 4) > data.len() as u64
        {
            return Err(invalid());
        }
        let mut frame = self.frame.lock().unwrap_or_else(|error| error.into_inner());
        if fresh {
            frame.width = width;
            frame.height = height;
            frame.pixels.resize(width as usize * height as usize * 4, 0);
        }
        if frame.pixels.is_empty()
            || x > frame.width
            || y > frame.height
            || width > frame.width - x
            || height > frame.height - y
        {
            return Err(invalid());
        }
        let frame_width = frame.width as usize;
        let bytes = width as usize * 4;
        for row in 0..height as usize {
            let source = row * stride as usize;
            let target = ((y as usize + row) * frame_width + x as usize) * 4;
            let pixels = &mut frame.pixels[target..target + bytes];
            pixels.copy_from_slice(&data[source..source + bytes]);
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
        }
        if fresh {
            frame.damage_all();
        } else {
            frame.mark_damage(Damage::new(x as usize, y as usize, width as usize, height as usize));
        }
        frame.generation += 1;
        Ok(())
    }
}

struct ListenerV2(Listener);

#[zbus::interface(name = "org.qemu.Display1.Listener.Unix.ScanoutDMABUF2", spawn = false)]
impl ListenerV2 {
    #[allow(clippy::too_many_arguments)]
    #[zbus(name = "ScanoutDMABUF2")]
    async fn scanout_dmabuf2(
        &self,
        fds: Vec<OwnedFd>,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        offsets: Vec<u32>,
        strides: Vec<u32>,
        planes: u32,
        fourcc: u32,
        backing_width: u32,
        backing_height: u32,
        modifier: u64,
        y0_top: bool,
    ) -> fdo::Result<()> {
        if fds.len() != planes as usize {
            return Err(invalid());
        }
        let buffer = buffer(
            fds,
            [backing_width, backing_height],
            [x, y, width, height],
            offsets,
            strides,
            fourcc,
            modifier,
            y0_top,
        )?;
        self.0.install_buffer(buffer).await
    }
}

#[allow(clippy::too_many_arguments)]
fn buffer(
    fds: Vec<OwnedFd>,
    [w, h]: [u32; 2],
    [x, y, width, height]: [u32; 4],
    offsets: Vec<u32>,
    strides: Vec<u32>,
    fourcc: u32,
    modifier: u64,
    y0_top: bool,
) -> fdo::Result<Buffer> {
    use std::os::fd::AsRawFd;
    let planes = fds.len();
    if !dimensions(w, h)
        || !dimensions(width, height)
        || x >= w
        || y >= h
        || width > w - x
        || height > h - y
        || !(1..=4).contains(&planes)
        || offsets.len() < planes
        || strides.len() < planes
        || strides[..planes].contains(&0)
    {
        return Err(invalid());
    }
    let mut surface = scanout::Scanout {
        fds: [-1; 4],
        width: w,
        height: h,
        offsets: [0; 4],
        strides: [0; 4],
        planes: planes as u32,
        format: fourcc,
        modifier,
        top_down: i32::from(y0_top),
    };
    for (plane, fd) in fds.iter().enumerate() {
        surface.fds[plane] = fd.as_raw_fd();
        surface.offsets[plane] = offsets[plane];
        surface.strides[plane] = strides[plane];
    }
    Ok(Buffer {
        _fds: fds,
        surface,
        crop: Damage::new(x as usize, y as usize, width as usize, height as usize),
        fresh: true,
        gpu_failed: false,
    })
}

fn dimensions(width: u32, height: u32) -> bool {
    let max = MAX_DESKTOP_SIZE as u32;
    (1..=max).contains(&width) && (1..=max).contains(&height)
}

fn invalid() -> fdo::Error {
    fdo::Error::InvalidArgs("Invalid QEMU scanout geometry or pixel format".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::object_server::Interface;

    #[test]
    #[ignore = "requires a running local D-Bus VM and a GPU"]
    fn live_dbus_desktop_delivers_visible_pixels() -> anyhow::Result<()> {
        let name = std::env::var("HYDROXL_DBUS_TEST_VM")
            .context("Set HYDROXL_DBUS_TEST_VM to a running session VM.")?;
        let connection = Connection::Session;
        let id = crate::backend::virsh(connection, &["domuuid", &name])?;
        let id = id.trim();
        let crate::backend::DisplayTarget::Dbus {
            graphics,
            ..
        } = crate::backend::display_target(connection, id, false)?
        else {
            anyhow::bail!("This VM does not use peer-to-peer D-Bus graphics.");
        };
        let session = Session::connect(connection, id, graphics, false, false, false)?;
        let started = std::time::Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            {
                let frame = session.frame.lock().unwrap();
                if let Some(error) = &frame.closed {
                    anyhow::bail!("{error}");
                }
                if frame.pixels.as_chunks::<4>().0.iter().any(|pixel| pixel[..3] != [0; 3]) {
                    eprintln!(
                        "Received visible pixels from a {}×{} D-Bus frame.",
                        frame.width, frame.height
                    );
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        anyhow::bail!("The connected D-Bus display supplied no visible pixels.");
    }

    #[test]
    fn software_listener_validates_and_copies_partial_frames() -> fdo::Result<()> {
        let frame = Frame::default();
        let listener = Listener {
            frame: frame.clone(),
            scanout: Arc::new(Mutex::new(None)),
            gpu: false,
        };
        listener.scanout(2, 2, 8, 0x2002_0888, vec![0; 16])?;
        listener.update(1, 1, 1, 1, 8, 0x2002_0888, vec![30, 20, 10, 0])?;
        assert_eq!(&frame.lock().unwrap().pixels[12..16], &[30, 20, 10, 255]);
        assert!(listener.update(2, 0, 1, 1, 4, 0x2002_0888, vec![0; 4]).is_err());
        Ok(())
    }

    #[test]
    fn listener_exports_qemus_display_signatures() -> anyhow::Result<()> {
        let mut metadata = String::new();
        let listener = Listener {
            frame: Frame::default(),
            scanout: Arc::new(Mutex::new(None)),
            gpu: false,
        };
        listener.introspect_to_writer(&mut metadata, 0);
        ListenerV2(listener).introspect_to_writer(&mut metadata, 0);
        for method in ["ScanoutDMABUF", "ScanoutDMABUF2", "UpdateDMABUF"] {
            assert!(metadata.contains(&format!("method name=\"{method}\"")), "{metadata}");
        }
        assert!(metadata.contains("type=\"h\""));
        assert!(metadata.contains("type=\"ah\""));
        let interfaces = format!("<node>{metadata}</node>");
        let doc = roxmltree::Document::parse(&interfaces)?;
        let update = doc
            .descendants()
            .find(|node| node.has_tag_name("method") && node.attribute("name") == Some("Update"))
            .context("Missing QEMU software update method")?;
        let args: Vec<_> = update.children().filter_map(|node| node.attribute("type")).collect();
        assert_eq!(args, ["i", "i", "i", "i", "u", "u", "ay"]);
        Ok(())
    }
}
