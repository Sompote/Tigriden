use slint::{Rgba8Pixel, SharedPixelBuffer};

/// The last two frames a pane handed out. `SharedPixelBuffer::new` writes
/// every pixel one at a time, and the `Canvas::fill` that follows overwrites
/// all of it, so a fresh buffer per paint costs a full pane of stores for
/// nothing. A pane holds only its newest frame, which leaves the one before
/// it unshared and free to draw into again.
#[derive(Default)]
pub struct Frames {
    slots: [Option<SharedPixelBuffer<Rgba8Pixel>>; 2],
}

impl Frames {
    /// The frame from two paints ago if the pane has not changed size, a new
    /// one otherwise. Pass what comes back to `keep` on the way out.
    pub fn take(&mut self, w: u32, h: u32) -> SharedPixelBuffer<Rgba8Pixel> {
        match self.slots[0].take() {
            Some(buf) if buf.width() == w && buf.height() == h => buf,
            _ => SharedPixelBuffer::<Rgba8Pixel>::new(w, h),
        }
    }

    /// Ages the two handles: what the pane is about to show moves into the
    /// young slot, what it showed last moves into the old one.
    pub fn keep(&mut self, buf: &SharedPixelBuffer<Rgba8Pixel>) {
        self.slots[0] = self.slots[1].take();
        self.slots[1] = Some(buf.clone());
    }
}

pub struct Canvas<'a> {
    pub pixels: &'a mut [Rgba8Pixel],
    pub width: i32,
    pub height: i32,
}

impl<'a> Canvas<'a> {
    pub fn fill(&mut self, color: [u8; 3]) {
        self.pixels.fill(Rgba8Pixel { r: color[0], g: color[1], b: color[2], a: 255 });
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: [u8; 3]) {
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x + w).min(self.width);
        let y1 = (y + h).min(self.height);
        if x0 >= x1 {
            return;
        }
        let px = Rgba8Pixel { r: color[0], g: color[1], b: color[2], a: 255 };
        for row in y0..y1 {
            let base = row as usize * self.width as usize;
            self.pixels[base + x0 as usize..base + x1 as usize].fill(px);
        }
    }

    pub fn blend_pixel(&mut self, x: i32, y: i32, color: cosmic_text::Color) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let a = color.a() as u32;
        if a == 0 {
            return;
        }
        let idx = y as usize * self.width as usize + x as usize;
        let dst = self.pixels[idx];
        let inv = 255 - a;
        self.pixels[idx] = Rgba8Pixel {
            r: ((color.r() as u32 * a + dst.r as u32 * inv) / 255) as u8,
            g: ((color.g() as u32 * a + dst.g as u32 * inv) / 255) as u8,
            b: ((color.b() as u32 * a + dst.b as u32 * inv) / 255) as u8,
            a: 255,
        };
    }

    pub fn blend_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: cosmic_text::Color) {
        if color.a() == 255 {
            self.fill_rect(x, y, w, h, [color.r(), color.g(), color.b()]);
            return;
        }
        for row in y..y + h {
            for col in x..x + w {
                self.blend_pixel(col, row, color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pane holds one frame at a time, so the frame before it is free to
    /// draw into. If it is not, `make_mut_slice` copies the whole pane pixel
    /// by pixel on every paint, which is the cost the two slots exist to
    /// avoid. Addresses, not contents: a copy lands somewhere else.
    #[test]
    fn a_paint_reuses_the_frame_from_two_paints_ago() {
        let mut frames = Frames::default();
        let mut pane: Option<SharedPixelBuffer<Rgba8Pixel>> = None;
        let mut seen = Vec::new();
        for _ in 0..4 {
            let mut buf = frames.take(64, 64);
            seen.push(buf.make_mut_slice().as_ptr());
            frames.keep(&buf);
            // Setting the pane's frame drops the one it showed before.
            pane = Some(buf);
        }
        drop(pane);
        assert_eq!(seen[2], seen[0], "the third paint draws into the first frame");
        assert_eq!(seen[3], seen[1], "the fourth paint draws into the second");
    }
}
