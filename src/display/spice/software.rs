//! SPICE primary-surface lifetime and CPU framebuffer updates.
use super::{
    Client,
    api::{Object, Primary},
};
use crate::display::{
    damage::Damage,
    frame::{Framebuffer, MAX_DESKTOP_SIZE},
};
use std::ffi::c_int;

#[derive(Default)]
pub(super) struct SoftwareDisplay {
    // Valid only on the SPICE thread, from primary-create until primary-destroy.
    primary: Option<Primary>,
    damage: Vec<Damage>,
    marked: bool,
    pub(super) requested: bool,
}

impl SoftwareDisplay {
    pub(super) fn create(&mut self, primary: Primary) {
        let (width, height) = (primary.width, primary.height);
        self.marked = primary.marked != 0;
        self.primary = Some(primary);
        self.damage.clear();
        self.invalidate(0, 0, width, height);
    }

    pub(super) fn discard_damage(&mut self) {
        self.damage.clear();
    }

    pub(super) fn destroy(&mut self) {
        self.primary = None;
        self.damage.clear();
        self.marked = false;
    }

    pub(super) fn invalidate(&mut self, x: i32, y: i32, w: i32, h: i32) {
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
    pub(super) unsafe fn publish(&mut self, frame: &mut Framebuffer) {
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
            frame.display_error = None;
            frame.generation += 1;
        }
    }
}

pub(super) unsafe extern "C" fn on_create(
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

pub(super) unsafe extern "C" fn on_primary_destroy(_: Object, data: Object) {
    let client = unsafe { &*data.cast::<Client>() };
    client.software.borrow_mut().destroy();
}

pub(super) unsafe extern "C" fn on_damage(
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

pub(super) unsafe extern "C" fn on_display_mark(_: Object, marked: c_int, data: Object) {
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
            for pixel in dest.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
        } else {
            let green_bits = if primary.format == 80 {
                6
            } else {
                5
            };
            let green_mask = (1 << green_bits) - 1;
            for (source, pixel) in source.as_chunks::<2>().0.iter().zip(dest.as_chunks_mut::<4>().0)
            {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
