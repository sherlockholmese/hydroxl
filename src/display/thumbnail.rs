pub(crate) fn sample(width: u32, height: u32, pixels: &[u8]) -> Option<image::RgbaImage> {
    if width == 0 || height == 0 || pixels.len() != width as usize * height as usize * 4 {
        return None;
    }
    let scale = (448. / f64::from(width)).min(280. / f64::from(height)).min(1.);
    let w = (f64::from(width) * scale).round().max(1.) as u32;
    let h = (f64::from(height) * scale).round().max(1.) as u32;
    Some(image::RgbaImage::from_fn(w, h, |x, y| {
        let start = ((y * height / h) * width + x * width / w) as usize * 4;
        image::Rgba(pixels[start..start + 4].try_into().expect("four channels"))
    }))
}
