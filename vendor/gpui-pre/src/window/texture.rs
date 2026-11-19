//! Window-owned mutable textures with bounded damage and CPU recovery.
use super::*;

/// A mutable BGRA texture owned by the window that created it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureId {
    image: crate::ImageId,
    window: WindowId,
}

pub(super) struct WindowTexture {
    image: Option<RenderImage>,
    extent: Size<DevicePixels>,
    damage: Vec<Bounds<DevicePixels>>,
}

/// The layout of a producer-owned, single-plane RGB DMA-BUF scanout.
#[cfg(target_os = "linux")]
pub struct DmaBufFrame {
    /// A duplicate of the scanout plane's file descriptor.
    pub fd: std::os::fd::OwnedFd,
    /// Dimensions in device pixels.
    pub extent: Size<DevicePixels>,
    /// DRM fourcc pixel format.
    pub fourcc: u32,
    /// DRM layout modifier.
    pub modifier: u64,
    /// Byte offset from the beginning of the DMA-BUF.
    pub offset: u32,
    /// Byte stride between rows.
    pub stride: u32,
    /// Whether row zero is at the top of the image.
    pub top_down: bool,
    /// The region that changed since the previous frame, in top-down pixels. `None`
    /// copies the whole frame. A new texture is always copied whole.
    pub damage: Option<Bounds<DevicePixels>>,
    /// Called once the GPU copy has completed, from another thread. When set,
    /// `Window::copy_dma_buf` returns as soon as the copy is queued; the producer may
    /// reuse its buffer only after this runs. Dropped without being called when the
    /// copy is not queued.
    pub on_complete: Option<Box<dyn FnOnce(bool) + Send>>,
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for DmaBufFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DmaBufFrame")
            .field("fd", &self.fd)
            .field("extent", &self.extent)
            .field("fourcc", &self.fourcc)
            .field("modifier", &self.modifier)
            .field("offset", &self.offset)
            .field("stride", &self.stride)
            .field("top_down", &self.top_down)
            .field("damage", &self.damage)
            .field("on_complete", &self.on_complete.is_some())
            .finish()
    }
}

impl TextureId {
    fn key(self) -> crate::AtlasKey {
        RenderImageParams {
            image_id: self.image,
            frame_index: 0,
        }
        .into()
    }
}

impl Window {
    /// Reports whether the current renderer can copy a DMA-BUF on the GPU.
    #[cfg(target_os = "linux")]
    pub fn supports_dma_buf(&self) -> bool {
        self.sprite_atlas.supports_dma_buf()
    }

    /// Copies a scanout to a texture. Without `on_complete`, returns after the producer
    /// can reuse its buffer; with it, returns once the copy is queued.
    ///
    /// # Safety
    /// The descriptor must exactly match a valid DMA-BUF in a foreign producer's
    /// GENERAL layout with write fences registered on its reservation object.
    /// The producer must not start a new write until this call returns, or until
    /// `on_complete` runs when it is set.
    #[cfg(target_os = "linux")]
    pub unsafe fn copy_dma_buf(
        &mut self,
        previous: Option<TextureId>,
        frame: DmaBufFrame,
    ) -> Result<Option<TextureId>> {
        let extent = frame.extent;
        let id = previous
            .filter(|id| {
                self.textures
                    .get(id)
                    .is_some_and(|entry| entry.image.is_none() && entry.extent == extent)
            })
            .unwrap_or_else(|| TextureId {
                image: crate::ImageId::next(),
                window: self.handle.window_id(),
            });
        if !unsafe { self.sprite_atlas.copy_dma_buf(id.key(), frame) }? {
            return Ok(None);
        }
        self.textures.entry(id).or_insert(WindowTexture {
            image: None,
            extent,
            damage: Vec::new(),
        });
        self.refresh();
        Ok(Some(id))
    }

    /// Creates a window-owned texture from tightly packed BGRA pixels. The CPU copy is
    /// retained for device-loss recovery. Release it with `drop_texture` when replaced.
    pub fn create_texture(
        &mut self,
        extent: Size<DevicePixels>,
        bytes: &[u8],
    ) -> Result<TextureId> {
        let (w, h) = (extent.width.0, extent.height.0);
        anyhow::ensure!(w > 0 && h > 0, "texture dimensions must be positive");
        anyhow::ensure!(
            (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(4))
                == Some(bytes.len()),
            "invalid texture byte length"
        );
        let buffer = image::RgbaImage::from_raw(w as u32, h as u32, bytes.to_vec())
            .ok_or_else(|| anyhow::anyhow!("invalid texture size"))?;
        let image = RenderImage::new(vec![image::Frame::new(buffer)]);
        let id = TextureId {
            image: image.id,
            window: self.handle.window_id(),
        };
        self.textures.insert(
            id,
            WindowTexture {
                image: Some(image),
                extent,
                damage: Vec::new(),
            },
        );
        self.refresh();
        Ok(id)
    }

    /// Updates a rectangle in BGRA order. Backends without partial uploads rebuild
    /// the tile on next paint. Validation precedes all mutation.
    pub fn update_texture(
        &mut self,
        id: TextureId,
        rect: Bounds<DevicePixels>,
        bytes: &[u8],
    ) -> Result<()> {
        let (w, h) = (rect.size.width.0, rect.size.height.0);
        anyhow::ensure!(w > 0 && h > 0, "texture update dimensions must be positive");
        anyhow::ensure!(
            (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(4))
                == Some(bytes.len()),
            "invalid texture update byte length"
        );
        self.update_texture_with_stride(id, rect, bytes, w as usize * 4)
    }

    /// Updates BGRA rows directly from a borrowed buffer with a byte stride.
    /// `bytes` starts at the first pixel of the rectangle, not the image origin.
    /// Copies into window-owned memory before returning; no producer pointer is retained.
    pub fn update_texture_with_stride(
        &mut self,
        id: TextureId,
        rect: Bounds<DevicePixels>,
        bytes: &[u8],
        bytes_per_row: usize,
    ) -> Result<()> {
        let texture = self
            .textures
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("texture does not belong to this window"))?;
        let image = texture
            .image
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("use a DMA-BUF update for this texture"))?;
        let extent = texture.extent;
        let (x, y, w, h) =
            (rect.origin.x.0, rect.origin.y.0, rect.size.width.0, rect.size.height.0);
        anyhow::ensure!(
            x >= 0
                && y >= 0
                && w > 0
                && h > 0
                && x.checked_add(w).is_some_and(|end| end <= extent.width.0)
                && y.checked_add(h).is_some_and(|end| end <= extent.height.0),
            "texture update is out of bounds"
        );
        anyhow::ensure!(
            bytes_per_row >= w as usize * 4
                && (h as usize - 1)
                    .checked_mul(bytes_per_row)
                    .and_then(|n| n.checked_add(w as usize * 4))
                    .is_some_and(|required| required <= bytes.len()),
            "invalid texture update byte length"
        );
        let buffer = image.data[0].buffer_mut().as_mut();
        for row in 0..h as usize {
            let start = ((y as usize + row) * extent.width.0 as usize + x as usize) * 4;
            buffer[start..start + w as usize * 4]
                .copy_from_slice(&bytes[row * bytes_per_row..row * bytes_per_row + w as usize * 4]);
        }
        // Defer GPU writes until paint, so hidden windows cannot accumulate queued
        // uploads. Keep bounded damage and rebuild from the CPU copy if necessary.
        let full = Bounds::new(Point::default(), extent);
        if texture.damage.first() != Some(&full) {
            let updated_pixels: i64 = texture
                .damage
                .iter()
                .chain([&rect])
                .map(|r| i64::from(r.size.width.0) * i64::from(r.size.height.0))
                .sum();
            if texture.damage.len() >= 128
                || updated_pixels >= i64::from(extent.width.0) * i64::from(extent.height.0)
            {
                texture.damage.clear();
                texture.damage.push(full);
            } else {
                texture.damage.push(rect);
            }
        }
        self.refresh();
        Ok(())
    }

    /// Releases a texture. IDs from another window cannot affect its allocations.
    pub fn drop_texture(&mut self, id: TextureId) {
        if self.textures.remove(&id).is_some() {
            self.sprite_atlas.remove(&id.key());
            self.refresh();
        }
    }

    /// Paints a texture with centered contain fitting, using logical layout bounds.
    pub fn paint_texture(&mut self, id: TextureId, bounds: Bounds<Pixels>) -> Result<()> {
        let extent =
            self.textures.get(&id).ok_or_else(|| anyhow::anyhow!("unknown texture"))?.extent;
        let image_bounds = crate::ObjectFit::Contain.get_bounds(bounds, extent);
        self.paint_texture_in(id, bounds, image_bounds)
    }

    /// Paints this texture at explicit bounds, clipped to the viewport.
    pub fn paint_texture_in(
        &mut self,
        id: TextureId,
        viewport: Bounds<Pixels>,
        image_bounds: Bounds<Pixels>,
    ) -> Result<()> {
        self.invalidator.debug_assert_paint();
        let visible = viewport.intersect(&image_bounds);
        if visible.size.width <= px(0.) || visible.size.height <= px(0.) {
            return Ok(());
        }
        let texture =
            self.textures.get_mut(&id).ok_or_else(|| anyhow::anyhow!("unknown texture"))?;
        #[cfg(target_os = "linux")]
        if texture.image.is_none() {
            if let Some(tile) = self.sprite_atlas.dma_buf_tile(&id.key()) {
                return self.paint_image_tile(
                    visible,
                    image_bounds,
                    Corners::default(),
                    tile,
                    false,
                    true,
                );
            }
            // Device recovery clears the atlas. The next scanout recreates the tile.
            return Ok(());
        }
        let image = texture.image.as_ref().expect("CPU texture");
        let extent = texture.extent;
        for rect in std::mem::take(&mut texture.damage) {
            let mut bytes =
                Vec::with_capacity(rect.size.width.0 as usize * rect.size.height.0 as usize * 4);
            for y in rect.origin.y.0..rect.origin.y.0 + rect.size.height.0 {
                let start = (y as usize * extent.width.0 as usize + rect.origin.x.0 as usize) * 4;
                bytes.extend_from_slice(
                    &image.as_bytes(0).unwrap()[start..start + rect.size.width.0 as usize * 4],
                );
            }
            match self.sprite_atlas.update_image(&id.key(), rect, &bytes) {
                Ok(true) => {}
                result => {
                    if let Err(error) = result {
                        log::warn!("Partial texture update failed; rebuilding: {error}");
                    }
                    self.sprite_atlas.remove(&id.key());
                    break;
                }
            }
        }
        let tile = self
            .sprite_atlas
            .get_or_insert_with(id.key(), &mut || {
                Ok(Some((extent, Cow::Borrowed(image.as_bytes(0).unwrap()))))
            })?
            .ok_or_else(|| anyhow::anyhow!("texture allocation failed"))?;
        self.paint_image_tile(visible, image_bounds, Corners::default(), tile, false, false)
    }
}

/// An updatable texture element. Apply normal sizing styles; the image is letterboxed.
pub fn texture(id: TextureId) -> crate::Canvas<()> {
    crate::canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            if let Err(error) = window.paint_texture(id, bounds) {
                log::error!("{error}");
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EmptyView, TestAppContext};

    #[gpui::test]
    fn guest_texture_updates_validate_before_mutation(cx: &mut TestAppContext) {
        let first = cx.add_window(|_, _| EmptyView);
        let second = cx.add_window(|_, _| EmptyView);
        let id = first
            .update(cx, |_, window, _| {
                let id = window
                    .create_texture(size(DevicePixels(3), DevicePixels(2)), &[0; 24])
                    .unwrap();
                let rect = Bounds::new(
                    point(DevicePixels(1), DevicePixels(0)),
                    size(DevicePixels(1), DevicePixels(2)),
                );
                window.update_texture(id, rect, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
                let pixels = window.textures[&id].image.as_ref().unwrap().as_bytes(0).unwrap();
                assert_eq!(&pixels[4..8], &[1, 2, 3, 4]);
                assert_eq!(&pixels[16..20], &[5, 6, 7, 8]);
                assert_eq!(&pixels[8..16], &[0; 8]);
                assert!(window.update_texture(id, rect, &[9; 7]).is_err());
                let outside =
                    Bounds::new(point(DevicePixels(i32::MAX), DevicePixels(0)), rect.size);
                assert!(window.update_texture(id, outside, &[9; 8]).is_err());
                assert_eq!(
                    &window.textures[&id].image.as_ref().unwrap().as_bytes(0).unwrap()[4..8],
                    &[1, 2, 3, 4]
                );
                // Source rows have padding and start at an offset in a larger
                // framebuffer. Neither padding nor neighboring pixels may leak.
                let source = [
                    91, 92, 93, 94, 11, 13, 17, 255, 95, 96, 97, 98, 99, 100, 101, 102, 19, 23, 29,
                    255,
                ];
                window.update_texture_with_stride(id, rect, &source[4..], 12).unwrap();
                let expected = [
                    0, 0, 0, 0, 11, 13, 17, 255, 0, 0, 0, 0, 0, 0, 0, 0, 19, 23, 29, 255, 0, 0, 0,
                    0,
                ];
                assert_eq!(
                    window.textures[&id].image.as_ref().unwrap().as_bytes(0).unwrap(),
                    expected
                );
                assert!(window.update_texture_with_stride(id, rect, &source[4..19], 12).is_err());
                assert!(window.update_texture_with_stride(id, rect, &source, 3).is_err());
                assert!(window.update_texture_with_stride(id, rect, &source, usize::MAX).is_err());
                assert_eq!(
                    window.textures[&id].image.as_ref().unwrap().as_bytes(0).unwrap(),
                    expected
                );
                // Damage stays bounded even if a window never paints.
                for _ in 0..1000 {
                    window.update_texture(id, rect, &[1; 8]).unwrap();
                }
                assert_eq!(window.textures[&id].damage.len(), 1);
                assert_eq!(
                    window.textures[&id].damage[0].size,
                    size(DevicePixels(3), DevicePixels(2))
                );
                id
            })
            .unwrap();
        second
            .update(cx, |_, window, _| {
                let rect = Bounds::new(Point::default(), size(DevicePixels(1), DevicePixels(1)));
                assert!(window.update_texture(id, rect, &[0; 4]).is_err());
                window.drop_texture(id); // Does not touch the first window.
            })
            .unwrap();
        first
            .update(cx, |_, window, _| {
                assert!(window.textures.contains_key(&id));
                window.drop_texture(id);
                assert!(!window.textures.contains_key(&id));
                assert!(
                    window.create_texture(size(DevicePixels(-1), DevicePixels(2)), &[]).is_err()
                );
            })
            .unwrap();
    }
}
