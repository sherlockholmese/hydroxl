//! Minimal RFB 3.8 client for QEMU's local VNC server.
//!
//! The reader thread applies framebuffer updates (Raw, CopyRect, DesktopSize) to a shared
//! BGRA buffer and records the damaged area; the UI uploads at most one frame per tick.
//! Input is written from the UI thread through a cloned socket.
mod clipboard;

use super::{
    damage::Damage,
    frame::{Cursor, Framebuffer, MAX_DESKTOP_SIZE},
};
use anyhow::{Context, Result, bail, ensure};
use clipboard::{CLIPBOARD_EXT, Clipboard, MAX_CLIPBOARD_MESSAGE};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const EXTENDED_DESKTOP_SIZE: i32 = -308;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const WRITE_TIMEOUT: Duration = Duration::from_millis(150);

/// Where QEMU serves the display, as reported by `virsh domdisplay --type vnc`.
#[derive(Clone, Debug)]
pub enum Endpoint {
    Unix(std::path::PathBuf),
    Tcp(String),
}
impl Endpoint {
    pub fn parse(uri: &str) -> Option<Self> {
        let uri = uri.trim();
        if let Some(path) = uri.strip_prefix("vnc+unix://") {
            return (!path.is_empty()).then(|| Self::Unix(path.into()));
        }
        let rest = uri.strip_prefix("vnc://")?;
        let (host, display) = rest.rsplit_once(':')?;
        if host.is_empty() {
            return None;
        }
        let display: u16 = display.trim_end_matches('/').parse().ok()?;
        // domdisplay reports either a display number or a TCP port.
        let port = if display < 5900 {
            5900 + display
        } else {
            display
        };
        Some(Self::Tcp(format!("{host}:{port}")))
    }
}

trait Stream: Read + Write + Send {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()>;
    fn duplicate(&self) -> std::io::Result<Box<dyn Stream>>;
    fn shutdown(&self);
}
impl Stream for UnixStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        UnixStream::set_read_timeout(self, timeout)
    }
    fn duplicate(&self) -> std::io::Result<Box<dyn Stream>> {
        Ok(Box::new(self.try_clone()?))
    }
    fn shutdown(&self) {
        let _ = UnixStream::shutdown(self, std::net::Shutdown::Both);
    }
}
impl Stream for TcpStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        TcpStream::set_read_timeout(self, timeout)
    }
    fn duplicate(&self) -> std::io::Result<Box<dyn Stream>> {
        let clone = self.try_clone()?;
        clone.set_nodelay(true)?;
        Ok(Box::new(clone))
    }
    fn shutdown(&self) {
        let _ = TcpStream::shutdown(self, std::net::Shutdown::Both);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResizeScreen {
    id: u32,
    flags: u32,
}

pub struct Session {
    pub frame: Arc<Mutex<Framebuffer>>,
    writer: Mutex<Box<dyn Stream>>,
    /// Last absolute position sent, for relative-mode deltas.
    last: Mutex<Option<(u16, u16)>>,
    pressed_keys: Mutex<std::collections::BTreeSet<u32>>,
    clipboard: Mutex<Clipboard>,
    share_clipboard: bool,
}

impl Session {
    pub fn connect(endpoint: &Endpoint, share_clipboard: bool) -> Result<Arc<Self>> {
        let mut stream: Box<dyn Stream> = match endpoint {
            Endpoint::Unix(path) => {
                let socket =
                    UnixStream::connect(path).context("Could not reach the guest's VNC socket.")?;
                socket.set_write_timeout(Some(WRITE_TIMEOUT))?;
                Box::new(socket)
            }
            Endpoint::Tcp(addr) => {
                let addresses =
                    addr.to_socket_addrs().context("Could not resolve the guest's VNC address.")?;
                let deadline = std::time::Instant::now() + CONNECT_TIMEOUT;
                let mut last_error = None;
                let mut connection = None;
                for address in addresses {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match TcpStream::connect_timeout(&address, remaining) {
                        Ok(socket) => {
                            connection = Some(socket);
                            break;
                        }
                        Err(error) => last_error = Some(error),
                    }
                }
                let socket = connection.ok_or_else(|| match last_error {
                    Some(error) => {
                        anyhow::Error::new(error).context("Could not reach the guest's VNC server.")
                    }
                    None => anyhow::anyhow!("The VNC address did not resolve to a usable server."),
                })?;
                socket.set_nodelay(true)?;
                socket.set_write_timeout(Some(WRITE_TIMEOUT))?;
                Box::new(socket)
            }
        };
        stream.set_read_timeout(Some(CONNECT_TIMEOUT))?;
        let (width, height) = handshake(&mut stream, share_clipboard).map_err(|error| {
            if let Some(io) = error.downcast_ref::<std::io::Error>() {
                return match io.kind() {
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                        error.context("The VNC server stopped responding during connection.")
                    }
                    _ => error.context("The VNC connection ended before the display was ready."),
                };
            }
            error
        })?;
        stream.set_read_timeout(None)?;
        let writer = stream.duplicate()?;
        let pixels = zeroed_pixels(width as usize * height as usize * 4)?;
        let session = Arc::new(Self {
            frame: Arc::new(Mutex::new(Framebuffer {
                width,
                height,
                pixels,
                damage: vec![Damage {
                    x: 0,
                    y: 0,
                    width,
                    height,
                }],
                generation: 0,
                ..Default::default()
            })),
            writer: Mutex::new(writer),
            last: Mutex::new(None),
            pressed_keys: Mutex::default(),
            clipboard: Mutex::default(),
            share_clipboard,
        });
        session.request(false)?;
        let reader = session.clone();
        std::thread::Builder::new()
            .name("vnc-reader".into())
            .spawn(move || {
                let result = reader.read_loop(stream);
                let mut frame = reader.frame.lock().unwrap_or_else(|e| e.into_inner());
                frame.closed.get_or_insert_with(|| match result {
                    Ok(()) => "The guest display closed.".into(),
                    Err(error) if error.is::<std::io::Error>() => {
                        eprintln!("VNC display disconnected: {error:#}");
                        "The VNC connection was interrupted.".into()
                    }
                    Err(error) => error.to_string(),
                });
                frame.generation = frame.generation.wrapping_add(1);
            })
            .context("Could not start reading the VNC display.")?;
        Ok(session)
    }

    fn send(&self, bytes: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(error) = writer.write_all(bytes) {
            writer.shutdown();
            drop(writer);
            let mut frame = self.frame.lock().unwrap_or_else(|err| err.into_inner());
            frame.closed =
                Some("Could not send input to VNC. The display connection was interrupted.".into());
            return Err(error).context("Could not send data to the guest's VNC server.");
        }
        Ok(())
    }

    fn request(&self, incremental: bool) -> Result<()> {
        let (w, h) = {
            let f = self.frame.lock().unwrap_or_else(|e| e.into_inner());
            (f.width as u16, f.height as u16)
        };
        let mut msg = vec![3, incremental as u8, 0, 0, 0, 0];
        msg.extend_from_slice(&w.to_be_bytes());
        msg.extend_from_slice(&h.to_be_bytes());
        self.send(&msg)
    }

    pub fn resize_desktop(&self, (width, height): (u16, u16), screen: ResizeScreen) -> Result<()> {
        validate_size(width.into(), height.into())?;
        // SetDesktopSize with one screen at the desktop origin.
        let mut msg = [0u8; 24];
        msg[0] = 251;
        msg[2..4].copy_from_slice(&width.to_be_bytes());
        msg[4..6].copy_from_slice(&height.to_be_bytes());
        msg[6] = 1;
        msg[8..12].copy_from_slice(&screen.id.to_be_bytes());
        msg[16..18].copy_from_slice(&width.to_be_bytes());
        msg[18..20].copy_from_slice(&height.to_be_bytes());
        msg[20..24].copy_from_slice(&screen.flags.to_be_bytes());
        self.send(&msg)
    }

    /// Ends the session; the reader thread stops when the socket closes.
    pub fn close(&self) {
        self.writer.lock().unwrap_or_else(|e| e.into_inner()).shutdown();
    }

    pub fn key(&self, keysym: u32, down: bool) {
        let mut msg = vec![4, down as u8, 0, 0];
        msg.extend_from_slice(&keysym.to_be_bytes());
        if self.send(&msg).is_ok() {
            let mut pressed = self.pressed_keys.lock().unwrap_or_else(|e| e.into_inner());
            if down {
                pressed.insert(keysym);
            } else {
                pressed.remove(&keysym);
            }
        }
    }

    /// Absolute pointer position in framebuffer pixels and the RFB button mask.
    pub fn pointer(&self, x: u16, y: u16, buttons: u8) {
        let absolute = self.frame.lock().unwrap_or_else(|e| e.into_inner()).absolute;
        let previous = self.last.lock().unwrap_or_else(|e| e.into_inner()).replace((x, y));
        // With pointer-type-change negotiated, QEMU reads relative-mode coordinates as
        // offsets from 0x7FFF.
        let (x, y) = match (absolute, previous) {
            (Some(false), Some((px, py))) => (
                (0x7fff + x as i32 - px as i32).clamp(0, 0xffff) as u16,
                (0x7fff + y as i32 - py as i32).clamp(0, 0xffff) as u16,
            ),
            (Some(false), None) => (0x7fff, 0x7fff),
            _ => (x, y),
        };
        self.pointer_packet(x, y, buttons);
    }

    /// Sends relative motion without deriving it from a clamped host position.
    pub fn relative_pointer(&self, mut dx: i32, mut dy: i32, buttons: u8) {
        loop {
            let x = dx.clamp(-32767, 32768);
            let y = dy.clamp(-32767, 32768);
            self.pointer_packet((0x7fff + x) as u16, (0x7fff + y) as u16, buttons);
            dx -= x;
            dy -= y;
            if dx == 0 && dy == 0 {
                break;
            }
        }
    }

    fn pointer_packet(&self, x: u16, y: u16, buttons: u8) {
        let mut msg = vec![5, buttons];
        msg.extend_from_slice(&x.to_be_bytes());
        msg.extend_from_slice(&y.to_be_bytes());
        let _ = self.send(&msg);
    }

    pub fn reset_pointer(&self) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Releases every forwarded key and mouse button, including non-modifier keys.
    pub fn release_input(&self) {
        let keys =
            std::mem::take(&mut *self.pressed_keys.lock().unwrap_or_else(|e| e.into_inner()));
        for key in keys {
            self.key(key, false);
        }
        let absolute = self.frame.lock().unwrap_or_else(|e| e.into_inner()).absolute;
        let last = self.last.lock().unwrap_or_else(|e| e.into_inner()).take();
        let (x, y) = if absolute == Some(false) {
            (0x7fff, 0x7fff)
        } else {
            last.unwrap_or((0, 0))
        };
        self.pointer_packet(x, y, 0);
    }

    fn read_loop(&self, mut s: Box<dyn Stream>) -> Result<()> {
        let mut row = Vec::new();
        loop {
            match u8_(&mut s)? {
                0 => {
                    skip(&mut s, 1)?;
                    let count = u16_(&mut s)?;
                    for _ in 0..count {
                        self.read_rectangle(&mut s, &mut row)?;
                    }
                    self.frame.lock().unwrap_or_else(|e| e.into_inner()).generation += 1;
                    self.request(true)?;
                }
                1 => {
                    skip(&mut s, 3)?;
                    let n = u16_(&mut s)? as usize;
                    skip(&mut s, n * 6)?;
                }
                2 => {}
                3 => {
                    skip(&mut s, 3)?;
                    let len = i32::from_be_bytes(array(&mut s)?);
                    let size = len.unsigned_abs() as usize;
                    if !self.share_clipboard || size > MAX_CLIPBOARD_MESSAGE {
                        skip(&mut s, size)?;
                        continue;
                    }
                    let mut payload = vec![0; size];
                    s.read_exact(&mut payload)?;
                    if len >= 0 {
                        // Classic ServerCutText: Latin-1 text.
                        self.received_clipboard(
                            payload.iter().map(|&b| b as char).collect(),
                            false,
                        );
                    } else {
                        self.extended_clipboard(&payload)?;
                    }
                }
                other => bail!("The guest display sent an unknown message ({other})."),
            }
        }
    }
    fn read_rectangle(&self, s: &mut impl Read, row: &mut Vec<u8>) -> Result<()> {
        let x = u16_(s)? as usize;
        let y = u16_(s)? as usize;
        let w = u16_(s)? as usize;
        let h = u16_(s)? as usize;
        let encoding = i32::from_be_bytes(array(s)?);
        match encoding {
            0 => {
                {
                    let f = self.frame.lock().unwrap_or_else(|e| e.into_inner());
                    validate_rect(x, y, w, h, f.width as usize, f.height as usize)?;
                }
                let bytes = w * h * 4;
                if bytes > row.len() {
                    row.try_reserve_exact(bytes - row.len())
                        .context("There is not enough memory for the VNC display update.")?;
                }
                row.resize(bytes, 0);
                s.read_exact(row)?;
                for px in row.as_chunks_mut::<4>().0 {
                    px[3] = 255;
                }
                let mut f = self.frame.lock().unwrap_or_else(|e| e.into_inner());
                let stride = f.width as usize * 4;
                for line in 0..h {
                    let start = (y + line) * stride + x * 4;
                    f.pixels[start..start + w * 4]
                        .copy_from_slice(&row[line * w * 4..(line + 1) * w * 4]);
                }
                f.mark_damage(Damage::new(x, y, w, h));
            }
            1 => {
                let sx = u16_(s)? as usize;
                let sy = u16_(s)? as usize;
                let mut f = self.frame.lock().unwrap_or_else(|e| e.into_inner());
                validate_rect(x, y, w, h, f.width as usize, f.height as usize)?;
                validate_rect(sx, sy, w, h, f.width as usize, f.height as usize)?;
                let stride = f.width as usize * 4;
                for index in 0..h {
                    let line = if sy < y {
                        h - 1 - index
                    } else {
                        index
                    };
                    let from = (sy + line) * stride + sx * 4;
                    let to = (y + line) * stride + x * 4;
                    f.pixels.copy_within(from..from + w * 4, to);
                }
                f.mark_damage(Damage::new(x, y, w, h));
            }
            -239 => {
                // Cursor pseudo-encoding: x, y are the hotspot.
                ensure!(w <= 1024 && h <= 1024, "The guest cursor size is unsupported.");
                let mut pixels = zeroed_pixels(w * h * 4)?;
                s.read_exact(&mut pixels)?;
                let stride = w.div_ceil(8);
                let mut mask = vec![0u8; stride * h];
                s.read_exact(&mut mask)?;
                for row in 0..h {
                    for col in 0..w {
                        let on = mask[row * stride + col / 8] & (0x80 >> (col % 8)) != 0;
                        pixels[(row * w + col) * 4 + 3] = if on {
                            255
                        } else {
                            0
                        };
                    }
                }
                let mut f = self.frame.lock().unwrap_or_else(|e| e.into_inner());
                f.cursor = (w > 0 && h > 0).then_some(Cursor {
                    width: w as u32,
                    height: h as u32,
                    hot_x: x as u32,
                    hot_y: y as u32,
                    pixels,
                });
                f.cursor_generation += 1;
            }
            -257 => {
                // QEMU pointer type change: x is 1 when the guest pointer is absolute.
                let absolute = x == 1;
                if std::env::var_os("HYDROXL_DEBUG_POINTER").is_some() {
                    eprintln!(
                        "guest pointer is {}",
                        if absolute {
                            "absolute"
                        } else {
                            "relative"
                        }
                    );
                }
                self.frame.lock().unwrap_or_else(|e| e.into_inner()).absolute = Some(absolute);
                self.reset_pointer();
            }
            EXTENDED_DESKTOP_SIZE | -223 => {
                let layout = if encoding == EXTENDED_DESKTOP_SIZE {
                    let [screens, ..] = array::<4>(s)?;
                    let mut single = None;
                    for _ in 0..screens {
                        let id = u32::from_be_bytes(array(s)?);
                        let _geometry = array::<8>(s)?;
                        let flags = u32::from_be_bytes(array(s)?);
                        if screens == 1 {
                            single = Some(ResizeScreen {
                                id,
                                flags,
                            });
                        }
                    }
                    // A server can reject a client request without changing
                    // the current layout or revoking resize support.
                    if x == 1 && y != 0 {
                        return Ok(());
                    }
                    Some(single)
                } else {
                    None
                };
                let mut f = self.frame.lock().unwrap_or_else(|e| e.into_inner());
                validate_size(w, h)?;
                if let Some(screen) = layout {
                    f.resize_screen = screen;
                }
                if (f.width, f.height) != (w as u32, h as u32) {
                    let pixels = zeroed_pixels(w * h * 4)?;
                    f.width = w as u32;
                    f.height = h as u32;
                    f.pixels = pixels;
                    f.damage.clear();
                    f.mark_damage(Damage::new(0, 0, w, h));
                }
            }
            other => {
                bail!("The guest display sent an unsupported encoding ({other}).")
            }
        }

        Ok(())
    }
}

fn zeroed_pixels(size: usize) -> Result<Vec<u8>> {
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(size).context("There is not enough memory for the VNC display.")?;
    pixels.resize(size, 0);
    Ok(pixels)
}

fn validate_size(w: usize, h: usize) -> Result<()> {
    ensure!(
        w > 0 && h > 0 && w <= MAX_DESKTOP_SIZE && h <= MAX_DESKTOP_SIZE,
        "The guest display size is unsupported."
    );
    Ok(())
}
fn validate_rect(
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    width: usize,
    height: usize,
) -> Result<()> {
    ensure!(
        x <= width && y <= height && w <= width - x && h <= height - y,
        "The guest display sent an out-of-bounds rectangle."
    );
    Ok(())
}

fn handshake(s: &mut Box<dyn Stream>, share_clipboard: bool) -> Result<(u32, u32)> {
    let mut version = [0u8; 12];
    s.read_exact(&mut version)?;
    if !version.starts_with(b"RFB ") {
        bail!("The guest display is not a VNC server.");
    }
    s.write_all(b"RFB 003.008\n")?;
    let count = u8_(s)?;
    if count == 0 {
        bail!("The guest display refused the connection.");
    }
    let mut types = vec![0u8; count as usize];
    s.read_exact(&mut types)?;
    if !types.contains(&1) {
        bail!("The guest display requires a password.");
    }
    s.write_all(&[1])?;
    if u32::from_be_bytes(array(s)?) != 0 {
        bail!("The guest display refused the connection.");
    }
    s.write_all(&[1])?; // shared
    let width = u16_(s)? as u32;
    let height = u16_(s)? as u32;
    validate_size(width as usize, height as usize)?;
    skip(s, 16)?;
    let name_len = u32::from_be_bytes(array(s)?) as usize;
    skip(s, name_len)?;
    // 32 bpp, depth 24, little endian, true colour, BGRX in memory (= GPUI's BGRA).
    let mut format = vec![0, 0, 0, 0, 32, 24, 0, 1];
    for max in [255u16; 3] {
        format.extend_from_slice(&max.to_be_bytes());
    }
    format.extend_from_slice(&[16, 8, 0, 0, 0, 0]);
    s.write_all(&format)?;
    // CopyRect, Raw, DesktopSize, Cursor: QEMU then sends the cursor shape instead of
    // painting it into the framebuffer, so the client draws it without lag.
    let mut encodings = vec![1, 0, -223, -239, -257, EXTENDED_DESKTOP_SIZE];
    if share_clipboard {
        encodings.push(CLIPBOARD_EXT);
    }
    let mut msg = vec![2, 0];
    msg.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
    for e in encodings {
        msg.extend_from_slice(&e.to_be_bytes());
    }
    s.write_all(&msg)?;
    Ok((width, height))
}

fn array<const N: usize>(s: &mut impl Read) -> Result<[u8; N]> {
    let mut b = [0u8; N];
    s.read_exact(&mut b)?;
    Ok(b)
}
fn u8_(s: &mut impl Read) -> Result<u8> {
    Ok(array::<1>(s)?[0])
}
fn u16_(s: &mut impl Read) -> Result<u16> {
    Ok(u16::from_be_bytes(array(s)?))
}
fn skip(s: &mut impl Read, n: usize) -> Result<()> {
    std::io::copy(&mut s.take(n as u64), &mut std::io::sink())?;
    Ok(())
}

/// X11 keysym for a GPUI key name, or `None` when the key has no guest meaning.
pub fn keysym(key: &str) -> Option<u32> {
    Some(match key {
        "enter" => 0xff0d,
        "backspace" => 0xff08,
        "tab" => 0xff09,
        "escape" => 0xff1b,
        "delete" => 0xffff,
        "insert" => 0xff63,
        "home" => 0xff50,
        "end" => 0xff57,
        "pageup" => 0xff55,
        "pagedown" => 0xff56,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        "space" => 0x20,
        "capslock" => 0xffe5,
        "menu" => 0xff67,
        "printscreen" => 0xff61,
        k if k.len() > 1 && k.starts_with('f') => {
            let n: u32 = k[1..].parse().ok()?;
            if !(1..=24).contains(&n) {
                return None;
            }
            0xffbe + n - 1
        }
        k => {
            let mut chars = k.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            let cp = c as u32;
            if (0x20..=0xff).contains(&cp) {
                cp
            } else {
                0x0100_0000 + cp
            }
        }
    })
}

/// RFB keysyms for modifier keys: shift, control, alt, super.
pub const MODIFIERS: [u32; 4] = [0xffe1, 0xffe3, 0xffe9, 0xffeb];

#[cfg(test)]
pub(crate) fn test_session() -> (Arc<super::session::Session>, Arc<Mutex<Vec<u8>>>) {
    let (session, _, packets) = tests::local_session(4, 3);
    (Arc::new(super::session::Session::Vnc(Arc::new(session))), packets)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone)]
    pub(super) struct TestStream {
        input: std::io::Cursor<Vec<u8>>,
        output: Arc<Mutex<Vec<u8>>>,
    }
    impl Read for TestStream {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(bytes)
        }
    }
    impl Write for TestStream {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.output.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Stream for TestStream {
        fn set_read_timeout(&self, _: Option<Duration>) -> std::io::Result<()> {
            Ok(())
        }
        fn duplicate(&self) -> std::io::Result<Box<dyn Stream>> {
            Ok(Box::new(self.clone()))
        }
        fn shutdown(&self) {}
    }
    pub(super) fn local_session(
        width: u32,
        height: u32,
    ) -> (Session, TestStream, Arc<Mutex<Vec<u8>>>) {
        let output = Arc::new(Mutex::new(Vec::new()));
        let client = TestStream {
            input: std::io::Cursor::new(Vec::new()),
            output: output.clone(),
        };
        let session = Session {
            frame: Arc::new(Mutex::new(Framebuffer {
                width,
                height,
                pixels: vec![0; (width * height * 4) as usize],
                ..Default::default()
            })),
            writer: Mutex::new(Box::new(client.clone())),
            last: Mutex::new(None),
            pressed_keys: Mutex::default(),
            clipboard: Mutex::default(),
            share_clipboard: true,
        };
        (session, client, output)
    }
    fn rect(x: u16, y: u16, w: u16, h: u16, encoding: i32, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0, 0, 0, 1];
        for n in [x, y, w, h] {
            bytes.extend_from_slice(&n.to_be_bytes());
        }
        bytes.extend_from_slice(&encoding.to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }
    fn apply(session: &Session, mut client: TestStream, _: Arc<Mutex<Vec<u8>>>, message: &[u8]) {
        client.input = std::io::Cursor::new(message.to_vec());
        assert!(session.read_loop(Box::new(client)).is_err()); // EOF after the update
    }
    #[test]
    fn raw_damage_and_copyrect_overlap() {
        let (session, client, peer) = local_session(4, 3);
        apply(&session, client, peer, &rect(1, 1, 2, 1, 0, &[1, 2, 3, 0, 4, 5, 6, 0]));
        let mut f = session.frame.lock().unwrap();
        let damage = f.take_damage();
        assert_eq!(damage.len(), 1);
        assert_eq!(damage[0].1, [1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(&f.pixels[16..20], &[0; 4]);
        drop(f);
        let (session, client, peer) = local_session(2, 3);
        session.frame.lock().unwrap().pixels = (0..24).collect();
        apply(&session, client, peer, &rect(0, 1, 2, 2, 1, &[0, 0, 0, 0]));
        let f = session.frame.lock().unwrap();
        assert_eq!(&f.pixels[8..24], &(0..16).collect::<Vec<u8>>());
    }
    #[test]
    fn cursor_and_pointer_updates_do_not_damage_pixels() {
        let (session, client, peer) = local_session(4, 3);
        let mut message = rect(1, 0, 0, 0, -257, &[]);
        message.extend(rect(0, 0, 1, 1, -239, &[3, 2, 1, 0, 0x80]));
        apply(&session, client, peer, &message);
        let mut f = session.frame.lock().unwrap();
        assert_eq!(f.absolute, Some(true));
        assert_eq!(f.cursor_generation, 1);
        assert_eq!(f.cursor.as_ref().unwrap().pixels, [3, 2, 1, 255]);
        assert!(f.take_damage().is_empty());
    }
    #[test]
    fn resize_replaces_damage_and_invalid_rect_is_rejected() {
        let (session, client, peer) = local_session(4, 3);
        session.frame.lock().unwrap().damage_all();
        apply(&session, client, peer, &rect(0, 0, 2, 2, -223, &[]));
        let mut f = session.frame.lock().unwrap();
        let damage = f.take_damage();
        assert_eq!(damage.len(), 1);
        assert_eq!(damage[0].1.len(), 16);
        drop(f);
        let (session, client, peer) = local_session(4, 3);
        apply(&session, client, peer, &rect(3, 0, 2, 1, 0, &[0; 8]));
        let mut f = session.frame.lock().unwrap();
        assert_eq!(f.generation, 0);
        assert!(f.take_damage().is_empty());
        assert_eq!(f.pixels, vec![0; 48]);
    }
    #[test]
    fn negotiated_guest_resize_uses_the_screen_id_and_preserves_rejected_frames() {
        let (session, client, peer) = local_session(4, 3);
        let mut layout = vec![1, 0, 0, 0];
        layout.extend_from_slice(&7u32.to_be_bytes());
        for value in [0u16, 0, 4, 3] {
            layout.extend_from_slice(&value.to_be_bytes());
        }
        layout.extend_from_slice(&2u32.to_be_bytes());
        let advertisement = rect(0, 0, 4, 3, EXTENDED_DESKTOP_SIZE, &layout);
        apply(&session, client.clone(), peer.clone(), &advertisement);
        let screen = session.frame.lock().unwrap().resize_screen.unwrap();
        peer.lock().unwrap().clear();
        session.resize_desktop((1440, 900), screen).unwrap();
        assert_eq!(
            *peer.lock().unwrap(),
            [251, 0, 5, 160, 3, 132, 1, 0, 0, 0, 0, 7, 0, 0, 0, 0, 5, 160, 3, 132, 0, 0, 0, 2]
        );
        let rejected = rect(1, 1, 1440, 900, EXTENDED_DESKTOP_SIZE, &layout);
        apply(&session, client.clone(), peer.clone(), &rejected);
        {
            let frame = session.frame.lock().unwrap();
            assert_eq!((frame.width, frame.height), (4, 3));
            assert_eq!(frame.resize_screen, Some(screen));
        }
        layout[12..16].copy_from_slice(&[0, 6, 0, 4]);
        let resized = rect(0, 0, 6, 4, EXTENDED_DESKTOP_SIZE, &layout);
        apply(&session, client, peer, &resized);
        let frame = session.frame.lock().unwrap();
        assert_eq!((frame.width, frame.height), (6, 4));
        assert_eq!(frame.pixels.len(), 6 * 4 * 4);
    }
    #[test]
    fn relative_packets_preserve_large_deltas_and_release_every_key() {
        let (session, _client, peer) = local_session(4, 3);
        session.frame.lock().unwrap().absolute = Some(false);
        session.relative_pointer(40000, -40000, 1);
        let packets = std::mem::take(&mut *peer.lock().unwrap());
        assert_eq!(&packets[..6], &[5, 1, 255, 255, 0, 0]);
        assert_eq!(&packets[6..], &[5, 1, 156, 63, 99, 190]);
        session.key(0x61, true);
        session.key(MODIFIERS[0], true);
        assert_eq!(std::mem::take(&mut *peer.lock().unwrap()).len(), 16);
        session.release_input();
        let up = std::mem::take(&mut *peer.lock().unwrap());
        assert_eq!(&up[..8], &[4, 0, 0, 0, 0, 0, 0, 0x61]);
        assert_eq!(&up[8..16], &[4, 0, 0, 0, 0, 0, 0xff, 0xe1]);
        assert_eq!(&up[16..], &[5, 0, 0x7f, 0xff, 0x7f, 0xff]);
        assert!(session.pressed_keys.lock().unwrap().is_empty());
    }

    /// Connects to a live QEMU VNC socket given in `HYDROXL_VNC_SOCKET`, waits for frames,
    /// and writes the framebuffer to `HYDROXL_VNC_PNG`. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_qemu_frames() {
        let socket = std::env::var("HYDROXL_VNC_SOCKET").expect("HYDROXL_VNC_SOCKET");
        let session = Session::connect(&Endpoint::Unix(socket.into()), true).expect("connect");
        let start = std::time::Instant::now();
        let mut updates = 0;
        let mut last = 0;
        while start.elapsed() < std::time::Duration::from_secs(4) {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let f = session.frame.lock().unwrap();
            assert!(f.closed.is_none(), "{:?}", f.closed);
            if f.generation != last {
                last = f.generation;
                updates += 1;
            }
        }
        session.pointer(10, 10, 0);
        session.key(0xff0d, true);
        session.key(0xff0d, false);
        let f = session.frame.lock().unwrap();
        assert!(f.width > 0 && f.height > 0);
        assert!(f.pixels.as_chunks::<4>().0.iter().any(|p| p[0] | p[1] | p[2] != 0), "blank frame");
        eprintln!("{}x{} after {updates} updates", f.width, f.height);
        if let Ok(path) = std::env::var("HYDROXL_VNC_PNG") {
            let mut rgba = f.pixels.clone();
            for px in rgba.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
            image::RgbaImage::from_raw(f.width, f.height, rgba).unwrap().save(path).unwrap();
        }
    }
}
