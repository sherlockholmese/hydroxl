//! Bounded framebuffer damage, independent of GPUI and transport details.
use super::frame::Framebuffer;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Damage {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl Damage {
    pub(super) fn new(x: usize, y: usize, width: usize, height: usize) -> Self {
        Self {
            x: x as u32,
            y: y as u32,
            width: width as u32,
            height: height as u32,
        }
    }
}
impl Framebuffer {
    pub(super) fn mark_damage(&mut self, rect: Damage) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        // Full damage subsumes subsequent rectangles until the UI drains it.
        if self.damage.first().is_some_and(|r| {
            r.x == 0 && r.y == 0 && r.width == self.width && r.height == self.height
        }) {
            return;
        }
        let updated_pixels: u64 = self
            .damage
            .iter()
            .chain([&rect])
            .map(|r| u64::from(r.width) * u64::from(r.height))
            .sum();
        if self.damage.len() >= 128
            || updated_pixels >= u64::from(self.width) * u64::from(self.height)
        {
            self.damage.clear();
            self.damage.push(Damage {
                x: 0,
                y: 0,
                width: self.width,
                height: self.height,
            });
        } else {
            self.damage.push(rect);
        }
    }
    /// Drains geometry without packing pixel copies. The caller must hold the
    /// framebuffer lock while borrowing the corresponding source rows.
    pub(crate) fn take_damage_rects(&mut self) -> Vec<Damage> {
        std::mem::take(&mut self.damage)
    }
    /// Drains only damaged pixels into tightly packed patches. Cursor and pointer-type
    /// updates do not copy framebuffer data. Unconsumed damage survives UI ticks.
    #[cfg(test)]
    pub fn take_damage(&mut self) -> Vec<(Damage, Vec<u8>)> {
        self.take_damage_rects()
            .into_iter()
            .map(|rect| {
                let mut bytes = Vec::with_capacity((rect.width * rect.height * 4) as usize);
                for y in rect.y..rect.y + rect.height {
                    let start = ((y * self.width + rect.x) * 4) as usize;
                    bytes.extend_from_slice(&self.pixels[start..start + rect.width as usize * 4]);
                }
                (rect, bytes)
            })
            .collect()
    }
    pub fn discard_damage(&mut self) {
        self.damage.clear();
    }
    pub fn damage_all(&mut self) {
        self.damage.clear();
        self.mark_damage(Damage {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_damage_and_packed_rows() {
        let mut f = Framebuffer {
            width: 4,
            height: 3,
            pixels: (0..48).collect(),
            ..Default::default()
        };
        f.mark_damage(Damage::new(1, 1, 2, 2));
        let damage = f.take_damage();
        assert_eq!(damage[0].1, [&f.pixels[20..28], &f.pixels[36..44]].concat());
        assert!(f.take_damage().is_empty());
        for _ in 0..200 {
            f.mark_damage(Damage::new(1, 1, 1, 1));
        }
        let damage = f.take_damage();
        assert_eq!(damage.len(), 1);
        assert_eq!(damage[0].1, f.pixels);
        f.mark_damage(Damage::new(3, 2, 1, 1));
        f.width = 2;
        f.height = 2;
        f.pixels.resize(16, 0);
        f.damage_all();
        assert_eq!(f.take_damage()[0].1.len(), 16);
    }
}
