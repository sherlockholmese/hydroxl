//! Shared bounds for guest drawing, cursor rendering, and coordinate mapping.
use super::frame::MAX_DESKTOP_SIZE;
use gpui_kit::{Bounds, Pixels, point, px, size};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scaling {
    #[default]
    Fit,
    GuestResize,
    Stretch,
    Fill,
    ActualSize,
}

impl Scaling {
    pub const ALL: [Self; 5] =
        [Self::Fit, Self::GuestResize, Self::Stretch, Self::Fill, Self::ActualSize];

    pub fn key(self) -> &'static str {
        match self {
            Self::Fit => "fit",
            Self::GuestResize => "guest-resize",
            Self::Stretch => "stretch",
            Self::Fill => "fill",
            Self::ActualSize => "actual",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Fit => "Fit image",
            Self::GuestResize => "Fit guest",
            Self::Stretch => "Stretch",
            Self::Fill => "Fill",
            Self::ActualSize => "1:1",
        }
    }

    pub fn menu_label(self) -> &'static str {
        match self {
            Self::Fit => "Scale image to fit",
            Self::GuestResize => "Resize guest to fit",
            other => other.label(),
        }
    }
}

pub fn guest_resize_target(bounds: Bounds<Pixels>, device_scale: f32) -> Option<(u16, u16)> {
    let width = f32::from(bounds.size.width) * device_scale;
    let height = f32::from(bounds.size.height) * device_scale;
    // Ignore transient or unusably small bounds during layout and minimization.
    if !width.is_finite() || !height.is_finite() || width < 320. || height < 200. {
        return None;
    }
    let scale = (MAX_DESKTOP_SIZE as f32 / width.max(height)).min(1.);
    Some(((width * scale).round() as u16, (height * scale).round().max(1.) as u16))
}
#[derive(Clone, Copy, Debug)]
pub struct GuestViewport {
    pub x: f32,
    pub y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    width: u32,
    height: u32,
}
impl GuestViewport {
    pub fn new(
        dimensions: (u32, u32),
        bounds: Bounds<Pixels>,
        scaling: Scaling,
        device_scale: f32,
    ) -> Option<Self> {
        let (width, height) = dimensions;
        let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (available_width, available_height) =
            (f32::from(bounds.size.width), f32::from(bounds.size.height));
        if width == 0
            || height == 0
            || ![x, y, available_width, available_height].iter().all(|n| n.is_finite())
            || available_width <= 0.
            || available_height <= 0.
        {
            return None;
        }
        let (sx, sy) = (available_width / width as f32, available_height / height as f32);
        let (scale_x, scale_y) = match scaling {
            Scaling::Fit | Scaling::GuestResize => (sx.min(sy), sx.min(sy)),
            Scaling::Stretch => (sx, sy),
            Scaling::Fill => (sx.max(sy), sx.max(sy)),
            Scaling::ActualSize => {
                let unit = device_scale.recip();
                if !unit.is_finite() || unit <= 0. {
                    return None;
                }
                (unit, unit)
            }
        };
        // A scale within a few percent of one device pixel per guest pixel is drawn at
        // exactly that. Otherwise a guest a few pixels off the window size (common in
        // guest-resize mode) drops or doubles one column every few hundred, which reads
        // as vertical lines. The few pixels of difference are letterboxed or cropped.
        let unit = device_scale.recip();
        let (scale_x, scale_y) = if matches!(scaling, Scaling::Fit | Scaling::GuestResize)
            && unit.is_finite()
            && unit > 0.
            && ((scale_x - unit) / unit).abs() < 0.03
        {
            (unit, unit)
        } else {
            (scale_x, scale_y)
        };
        // Whole device pixels: a fractional origin blurs every column under linear filtering.
        let snap = |value: f32| {
            if device_scale.is_finite() && device_scale > 0. {
                (value * device_scale).round() / device_scale
            } else {
                value
            }
        };
        Some(Self {
            x: snap(x + (available_width - width as f32 * scale_x) / 2.),
            y: snap(y + (available_height - height as f32 * scale_y) / 2.),
            scale_x,
            scale_y,
            width,
            height,
        })
    }
    pub fn bounds(self) -> Bounds<Pixels> {
        Bounds::new(
            point(px(self.x), px(self.y)),
            size(px(self.width as f32 * self.scale_x), px(self.height as f32 * self.scale_y)),
        )
    }
    pub fn guest_point(self, x: f32, y: f32) -> (u16, u16) {
        (
            ((x - self.x) / self.scale_x).clamp(0., (self.width - 1) as f32).round() as u16,
            ((y - self.y) / self.scale_y).clamp(0., (self.height - 1) as f32).round() as u16,
        )
    }
    pub fn move_pointer(self, x: f32, y: f32, dx: i32, dy: i32) -> (f32, f32) {
        (
            (x + dx as f32 * self.scale_x)
                .clamp(self.x, self.x + (self.width - 1) as f32 * self.scale_x),
            (y + dy as f32 * self.scale_y)
                .clamp(self.y, self.y + (self.height - 1) as f32 * self.scale_y),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_fit_follows_viewport_size_at_the_guest_aspect_ratio() {
        let mode = Scaling::from_key("fit").unwrap();
        assert_eq!(mode, Scaling::default());
        for (width, height, scale) in [(640., 480., 0.5), (2560., 1600., 2.)] {
            let bounds = Bounds::new(point(px(0.), px(0.)), size(px(width), px(height)));
            let view = GuestViewport::new((1280, 720), bounds, mode, 1.).unwrap();
            assert_eq!((view.scale_x, view.scale_y), (scale, scale));
            assert_eq!(view.guest_point(width / 2., height / 2.), (640, 360));
            assert_eq!(view.bounds().size, size(px(1280. * scale), px(720. * scale)));
        }
    }
    #[test]
    fn guest_resize_targets_physical_pixels_and_preserves_viewport_ratio_at_the_limit() {
        let bounds = |w, h| Bounds::new(point(px(0.), px(0.)), size(px(w), px(h)));
        assert_eq!(guest_resize_target(bounds(960., 540.), 1.5), Some((1440, 810)));
        assert_eq!(guest_resize_target(bounds(100., 100.), 1.), None);
        assert_eq!(guest_resize_target(bounds(10000., 5000.), 1.), Some((8192, 4096)));
    }
    #[test]
    fn scaling_modes_map_cropped_and_stretched_coordinates() {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(400.), px(300.)));
        for (mode, expected) in [
            (Scaling::Fit, (0, 0)),
            (Scaling::Stretch, (0, 0)),
            (Scaling::Fill, (133, 0)),
            (Scaling::ActualSize, (0, 0)),
        ] {
            let view = GuestViewport::new((800, 400), bounds, mode, 2.).unwrap();
            assert_eq!(view.guest_point(200., 150.), (400, 200));
            assert_eq!(view.guest_point(0., 0.), expected);
            if mode == Scaling::Stretch {
                assert_eq!(view.bounds(), bounds);
            }
            if mode == Scaling::ActualSize {
                assert_eq!(view.scale_x * 2., 1.);
            }
        }
    }
    #[test]
    fn near_native_size_draws_one_device_pixel_per_guest_pixel() {
        // A 1999-pixel window showing a 2000-pixel guest at 1.5x device scale.
        let bounds = Bounds::new(point(px(0.5), px(0.)), size(px(1999. / 1.5), px(1000.)));
        let view = GuestViewport::new((2000, 1200), bounds, Scaling::GuestResize, 1.5).unwrap();
        assert_eq!(view.scale_x * 1.5, 1.);
        assert_eq!(view.scale_y * 1.5, 1.);
        assert_eq!((view.x * 1.5).fract(), 0.);
        // Far from native size still scales to fit.
        let small = Bounds::new(point(px(0.), px(0.)), size(px(640.), px(480.)));
        let view = GuestViewport::new((1280, 960), small, Scaling::Fit, 1.).unwrap();
        assert_eq!(view.scale_x, 0.5);
    }

    #[test]
    fn fractional_scale_letterboxing_and_fullscreen() {
        for scale in [1.25, 1.5, 1.75] {
            for (w, h, top) in [(1920., 1080., 0.), (1280., 844., 44.)] {
                // X11 physical coordinates and Wayland surface coordinates arrive
                // in the same logical coordinate space as GPUI layout.
                let view = GuestViewport::new(
                    (1920, 1080),
                    Bounds::new(point(px(0.), px(top)), size(px(w / scale), px(h / scale - top))),
                    Scaling::Fit,
                    scale,
                )
                .unwrap();
                for (gx, gy) in [(0, 0), (960, 540), (1919, 1079)] {
                    let physical_x = (view.x + gx as f32 * view.scale_x) * scale;
                    let physical_y = (view.y + gy as f32 * view.scale_y) * scale;
                    assert_eq!(view.guest_point(physical_x / scale, physical_y / scale), (gx, gy));
                }
                assert_eq!(view.guest_point(-100., -100.), (0, 0));
                assert_eq!(view.guest_point(10000., 10000.), (1919, 1079));
            }
        }
        assert!(GuestViewport::new((1, 1), Bounds::default(), Scaling::Fit, 1.).is_none());
    }
}
