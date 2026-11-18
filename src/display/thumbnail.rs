/// Largest thumbnail, in device pixels: the overview well is 280pt tall, so this keeps the
/// preview sharp up to 2x density for the common guest aspect ratios.
const MAX_WIDTH: u32 = 1280;
const MAX_HEIGHT: u32 = 720;

/// Downscale a frame for the overview well by averaging each output pixel's source area.
/// Point sampling drops whole rows of glyph strokes and makes guest text unreadable.
pub(crate) fn sample(width: u32, height: u32, pixels: &[u8]) -> Option<image::RgbaImage> {
    if width == 0 || height == 0 || pixels.len() != width as usize * height as usize * 4 {
        return None;
    }
    let scale = (f64::from(MAX_WIDTH) / f64::from(width))
        .min(f64::from(MAX_HEIGHT) / f64::from(height))
        .min(1.);
    let w = (f64::from(width) * scale).round().max(1.) as u32;
    let h = (f64::from(height) * scale).round().max(1.) as u32;
    if (w, h) == (width, height) {
        return image::RgbaImage::from_raw(w, h, pixels.to_vec());
    }
    let span = |i: u32, out: u32, src: u32| {
        let start = (u64::from(i) * u64::from(src) / u64::from(out)) as u32;
        let end = ((u64::from(i) + 1) * u64::from(src) / u64::from(out)) as u32;
        start..end.max(start + 1).min(src)
    };
    let columns: Vec<_> = (0..w).map(|x| span(x, w, width)).collect();
    let mut out = image::RgbaImage::new(w, h);
    for y in 0..h {
        let rows = span(y, h, height);
        for (x, cols) in columns.iter().enumerate() {
            let mut sum = [0u32; 4];
            for row in rows.clone() {
                let base = row as usize * width as usize;
                for col in cols.clone() {
                    let at = (base + col as usize) * 4;
                    for (s, v) in sum.iter_mut().zip(&pixels[at..at + 4]) {
                        *s += u32::from(*v);
                    }
                }
            }
            let n = rows.len() as u32 * cols.len() as u32;
            out.put_pixel(x as u32, y, image::Rgba(sum.map(|s| ((s + n / 2) / n) as u8)));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn averages_and_bounds() {
        let (w, h) = (2560, 1440);
        let mut pixels = vec![0u8; w * h * 4];
        // Alternating white and black columns average to mid grey, never to one of them.
        for y in 0..h {
            for x in (0..w).step_by(2) {
                pixels[(y * w + x) * 4..][..4].copy_from_slice(&[255; 4]);
            }
        }
        let out = super::sample(w as u32, h as u32, &pixels).unwrap();
        assert_eq!(out.dimensions(), (1280, 720));
        assert!(out.pixels().all(|p| (127..=128).contains(&p.0[0])));
    }

    #[test]
    fn small_frames_pass_through() {
        let out = super::sample(2, 1, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        assert_eq!(out.into_raw(), vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
