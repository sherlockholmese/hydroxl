//! SPICE GL scanout and the draw acknowledgment lifecycle.
use super::{Client, api::Object, wake_worker};
use crate::display::{damage::Damage, scanout, thumbnail};
use anyhow::Result;
use std::{
    ffi::c_int,
    ptr::null_mut,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct ScanoutLayout {
    width: u32,
    height: u32,
    format: u32,
    modifier: u64,
    planes: u32,
}

impl From<scanout::Scanout> for ScanoutLayout {
    fn from(scanout: scanout::Scanout) -> Self {
        Self {
            width: scanout.width,
            height: scanout.height,
            format: scanout.format,
            modifier: scanout.modifier,
            planes: scanout.planes,
        }
    }
}

#[derive(Default)]
pub(super) struct ReadbackProfile {
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

pub(super) unsafe extern "C" fn on_scanout_change(_: Object, _: Object, data: Object) {
    let client = unsafe { &*data.cast::<Client>() };
    client.scanout.take();
}

pub(super) unsafe extern "C" fn on_gl_draw(
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
    client.software.borrow_mut().discard_damage();
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
    let described = ScanoutLayout::from(scanout);
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

pub(super) struct PendingDraw {
    pub(super) channel: Object,
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
    pub(super) fn advance_gpu_draw(&self) {
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
                let lit =
                    pixels.as_chunks::<4>().0.iter().filter(|p| p[0] | p[1] | p[2] != 0).count();
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
            frame.display_error = None;
            if gpu_copied {
                frame.thumbnail = thumbnail::sample(w as u32, h as u32, &pixels);
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
            let mut frame = self.frame();
            if frame.display_error.is_none() {
                let message = format!("Could not draw the SPICE GPU buffer. {error:#}");
                eprintln!("{message}");
                frame.display_error = Some(message);
                frame.generation += 1;
            }
        }
    }
}
