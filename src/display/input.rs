//! GPUI capture policy stays here; VNC only handles guest input packets.
use gpui_kit::{Point, Window};

#[derive(Default)]
pub struct InputCapture {
    relative: bool,
    locked: bool,
    remainder: Point<f64>,
}
impl InputCapture {
    pub fn begin(&mut self, window: &mut Window, relative: bool) {
        if let Err(error) = window.set_cursor_hidden(true) {
            eprintln!("Guest cursor hiding: {error}");
        }
        if let Err(error) = window.inhibit_system_shortcuts(true) {
            eprintln!("Guest shortcut capture: {error}");
        }
        self.relative = false;
        self.set_relative(window, relative);
    }
    pub fn set_relative(&mut self, window: &mut Window, relative: bool) {
        if self.relative == relative {
            return;
        }
        self.relative = relative;
        self.remainder = Point::default();
        if let Err(error) = window.lock_pointer(relative) {
            eprintln!("Guest pointer capture: {error}");
        }
        self.locked = window.is_pointer_locked();
        window.take_raw_mouse_motion();
    }
    pub fn locked(&self) -> bool {
        self.locked
    }
    /// Returns None when an active lock was revoked by the desktop.
    pub fn poll(&mut self, window: &mut Window) -> Option<(i32, i32)> {
        let locked = window.is_pointer_locked();
        if self.locked && !locked {
            return None;
        }
        self.locked = locked;
        let delta = window.take_raw_mouse_motion();
        if !locked {
            return Some((0, 0));
        }
        Some(accumulate(&mut self.remainder, delta))
    }
    pub fn release(&mut self, window: &mut Window) {
        // Each release is attempted even if another one fails.
        for result in [
            window.lock_pointer(false),
            window.inhibit_system_shortcuts(false),
            window.set_cursor_hidden(false),
        ] {
            if let Err(error) = result {
                eprintln!("Releasing guest capture: {error}");
            }
        }
        window.take_raw_mouse_motion();
        *self = Self::default();
    }
}

fn accumulate(remainder: &mut Point<f64>, delta: Point<f64>) -> (i32, i32) {
    if !delta.x.is_finite() || !delta.y.is_finite() {
        return (0, 0);
    }
    remainder.x += delta.x;
    remainder.y += delta.y;
    let (x, y) = (remainder.x.trunc() as i32, remainder.y.trunc() as i32);
    remainder.x -= x as f64;
    remainder.y -= y as f64;
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relative_motion_keeps_fractional_deltas() {
        let mut remainder = Point::default();
        assert_eq!(
            accumulate(
                &mut remainder,
                Point {
                    x: 0.75,
                    y: -0.75
                }
            ),
            (0, 0)
        );
        assert_eq!(
            accumulate(
                &mut remainder,
                Point {
                    x: 0.75,
                    y: -0.75
                }
            ),
            (1, -1)
        );
        assert_eq!(
            accumulate(
                &mut remainder,
                Point {
                    x: 0.5,
                    y: -0.5
                }
            ),
            (1, -1)
        );
    }
}
