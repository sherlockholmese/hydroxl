//! Guest cursor images for the Wayland and X11 backends.
use gpui::CursorImage;

/// A cursor resampled for one output scale.
pub(crate) struct ScaledCursor {
    pub width: u32,
    pub height: u32,
    pub hot_x: u32,
    pub hot_y: u32,
    /// Premultiplied ARGB32 in native byte order (B, G, R, A in memory).
    pub bgra: Vec<u8>,
}

/// Resamples `image` to `pixels_per_logical` device pixels per logical pixel with
/// nearest-neighbour sampling, which keeps pointer edges crisp.
pub(crate) fn scale_cursor(image: &CursorImage, pixels_per_logical: f32) -> Option<ScaledCursor> {
    let (w, h) = (image.width, image.height);
    if w == 0 || h == 0 || image.bgra.len() != (w * h * 4) as usize {
        return None;
    }
    let factor = (image.scale * pixels_per_logical).clamp(0.25, 8.);
    let width = ((w as f32 * factor).round() as u32).clamp(1, 256);
    let height = ((h as f32 * factor).round() as u32).clamp(1, 256);
    let mut bgra = vec![0; (width * height * 4) as usize];
    for y in 0..height {
        let sy = ((y as f32 + 0.5) * h as f32 / height as f32) as u32;
        for x in 0..width {
            let sx = ((x as f32 + 0.5) * w as f32 / width as f32) as u32;
            let s = ((sy.min(h - 1) * w + sx.min(w - 1)) * 4) as usize;
            let [b, g, r, a] = [image.bgra[s], image.bgra[s + 1], image.bgra[s + 2], image.bgra[s + 3]];
            let premultiply = |c: u8| ((c as u16 * a as u16 + 127) / 255) as u8;
            let d = ((y * width + x) * 4) as usize;
            bgra[d..d + 4].copy_from_slice(&[premultiply(b), premultiply(g), premultiply(r), a]);
        }
    }
    Some(ScaledCursor {
        width,
        height,
        hot_x: ((image.hot_x as f32 * width as f32 / w as f32) as u32).min(width - 1),
        hot_y: ((image.hot_y as f32 * height as f32 / h as f32) as u32).min(height - 1),
        bgra,
    })
}
