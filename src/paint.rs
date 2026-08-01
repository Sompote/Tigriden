use slint::Rgba8Pixel;

pub struct Canvas<'a> {
    pub pixels: &'a mut [Rgba8Pixel],
    pub width: i32,
    pub height: i32,
}

impl<'a> Canvas<'a> {
    pub fn fill(&mut self, color: [u8; 3]) {
        for px in self.pixels.iter_mut() {
            *px = Rgba8Pixel { r: color[0], g: color[1], b: color[2], a: 255 };
        }
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: [u8; 3]) {
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x + w).min(self.width);
        let y1 = (y + h).min(self.height);
        for row in y0..y1 {
            let base = row as usize * self.width as usize;
            for col in x0..x1 {
                self.pixels[base + col as usize] =
                    Rgba8Pixel { r: color[0], g: color[1], b: color[2], a: 255 };
            }
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
