use std::collections::HashMap;

use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{point_to_viewport, Term, TermMode};
use alacritty_terminal::vte::ansi::CursorShape;
use cosmic_text::{Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};
use slint::{Rgba8Pixel, SharedPixelBuffer};

use crate::paint::Canvas;
use crate::term::colors;
use crate::term::EventProxy;
use crate::theme::ThemeDef;

#[derive(Clone, Copy)]
struct GlyphPos {
    cache_key: CacheKey,
    x: i32,
    y: i32,
}

/// Rasterizes an alacritty grid into an RGBA pixel buffer. One instance per
/// (font, size, scale); glyph positions are cached per distinct character.
pub struct TermRenderer {
    font_family: String,
    font_size_px: f32,
    pub cell_w: u32,
    pub cell_h: u32,
    glyphs: HashMap<(char, bool), Option<GlyphPos>>,
    shape_buffer: Buffer,
}

impl TermRenderer {
    pub fn new(font_family: &str, font_size_px: f32, font_system: &mut FontSystem) -> Self {
        let cell_h = (font_size_px * 1.35).round();
        let metrics = Metrics::new(font_size_px, cell_h);
        let mut shape_buffer = Buffer::new(font_system, metrics);
        shape_buffer.set_size(Some(font_size_px * 4.0), Some(cell_h * 2.0));

        let mut renderer = Self {
            font_family: font_family.to_string(),
            font_size_px,
            cell_w: 0,
            cell_h: cell_h as u32,
            glyphs: HashMap::new(),
            shape_buffer,
        };
        renderer.cell_w = renderer.measure_advance(font_system).max(1.0).round() as u32;
        renderer
    }

    fn measure_advance(&mut self, font_system: &mut FontSystem) -> f32 {
        let attrs = Attrs::new().family(Family::Name(&self.font_family));
        self.shape_buffer.set_text("M", &attrs, Shaping::Advanced, None);
        self.shape_buffer.shape_until_scroll(font_system, false);
        self.shape_buffer
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first().map(|g| g.w))
            .unwrap_or(self.font_size_px * 0.6)
    }

    fn glyph(&mut self, font_system: &mut FontSystem, c: char, bold: bool) -> Option<GlyphPos> {
        if let Some(cached) = self.glyphs.get(&(c, bold)) {
            return *cached;
        }
        let mut text = [0u8; 4];
        let attrs = Attrs::new().family(Family::Name(&self.font_family));
        let attrs = if bold { attrs.weight(Weight::BOLD) } else { attrs };
        self.shape_buffer.set_text(c.encode_utf8(&mut text), &attrs, Shaping::Advanced, None);
        self.shape_buffer.shape_until_scroll(font_system, false);
        let pos = self.shape_buffer.layout_runs().next().and_then(|run| {
            run.glyphs.first().map(|glyph| {
                let physical = glyph.physical((0.0, 0.0), 1.0);
                GlyphPos {
                    cache_key: physical.cache_key,
                    x: physical.x,
                    y: run.line_y as i32 + physical.y,
                }
            })
        });
        self.glyphs.insert((c, bold), pos);
        pos
    }

    pub fn grid_size(&self, width_px: u32, height_px: u32) -> (u16, u16) {
        let cols = (width_px / self.cell_w.max(1)).max(2) as u16;
        let rows = (height_px / self.cell_h.max(1)).max(1) as u16;
        (cols, rows)
    }

    pub fn render(
        &mut self,
        font_system: &mut FontSystem,
        swash_cache: &mut SwashCache,
        term: &Term<EventProxy>,
        theme: &'static ThemeDef,
        focused: bool,
        width_px: u32,
        height_px: u32,
    ) -> SharedPixelBuffer<Rgba8Pixel> {
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width_px.max(1), height_px.max(1));
        let bg = colors::base_palette(theme)[0];
        let default_fg = colors::base_palette(theme)[7];
        let (buf_w, buf_h) = (buffer.width() as i32, buffer.height() as i32);
        let mut canvas = Canvas { pixels: buffer.make_mut_slice(), width: buf_w, height: buf_h };
        canvas.fill(bg);

        let content = term.renderable_content();
        let display_offset = content.display_offset;
        let overrides = content.colors;
        let selection = content.selection;
        let cell_w = self.cell_w as i32;
        let cell_h = self.cell_h as i32;

        struct DrawCell {
            c: char,
            col: i32,
            row: i32,
            fg: [u8; 3],
            bold: bool,
            flags: Flags,
        }
        let mut draw_cells: Vec<DrawCell> = Vec::with_capacity(1024);

        for indexed in content.display_iter {
            let flags = indexed.cell.flags;
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let Some(view) = point_to_viewport(display_offset, indexed.point) else { continue };
            let (col, row) = (view.column.0 as i32, view.line as i32);

            let mut fg = colors::resolve(indexed.cell.fg, overrides, theme);
            let mut cell_bg = colors::resolve(indexed.cell.bg, overrides, theme);
            let selected = selection.is_some_and(|range| range.contains(indexed.point));
            if flags.contains(Flags::INVERSE) != selected {
                std::mem::swap(&mut fg, &mut cell_bg);
            }
            if flags.intersects(Flags::DIM) {
                fg = [(fg[0] as u16 * 2 / 3) as u8, (fg[1] as u16 * 2 / 3) as u8, (fg[2] as u16 * 2 / 3) as u8];
            }

            if cell_bg != bg {
                canvas.fill_rect(col * cell_w, row * cell_h, cell_w, cell_h, cell_bg);
            }

            let c = indexed.cell.c;
            if c != ' ' && c != '\t' && !flags.contains(Flags::HIDDEN) {
                draw_cells.push(DrawCell { c, col, row, fg, bold: flags.intersects(Flags::BOLD), flags });
            } else if flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT) {
                draw_cells.push(DrawCell { c: ' ', col, row, fg, bold: false, flags });
            }
        }

        for cell in &draw_cells {
            let origin_x = cell.col * cell_w;
            let origin_y = cell.row * cell_h;
            if cell.c != ' ' {
                if let Some(pos) = self.glyph(font_system, cell.c, cell.bold) {
                    let fg = cell.fg;
                    let base = cosmic_text::Color::rgb(fg[0], fg[1], fg[2]);
                    swash_cache.with_pixels(font_system, pos.cache_key, base, |px, py, color| {
                        let x = origin_x + pos.x + px;
                        let y = origin_y + pos.y + py;
                        canvas.blend_pixel(x, y, color);
                    });
                }
            }
            if cell.flags.intersects(Flags::ALL_UNDERLINES) {
                canvas.fill_rect(origin_x, origin_y + cell_h - 2, cell_w, 1, cell.fg);
            }
            if cell.flags.contains(Flags::STRIKEOUT) {
                canvas.fill_rect(origin_x, origin_y + cell_h / 2, cell_w, 1, cell.fg);
            }
        }

        // Cursor (hidden while the grid is scrolled away or by the app).
        if !content.mode.contains(TermMode::SHOW_CURSOR) || content.cursor.shape == CursorShape::Hidden {
            return buffer;
        }
        if let Some(view) = point_to_viewport(display_offset, content.cursor.point) {
            let (col, row) = (view.column.0 as i32, view.line as i32);
            let (x, y) = (col * cell_w, row * cell_h);
            let cursor_color = default_fg;
            if focused && content.cursor.shape == CursorShape::Block {
                canvas.fill_rect(x, y, cell_w, cell_h, cursor_color);
                // Redraw the glyph under the cursor in background color.
                let under = term.grid()[content.cursor.point].c;
                if under != ' ' {
                    if let Some(pos) = self.glyph(font_system, under, false) {
                        let base = cosmic_text::Color::rgb(bg[0], bg[1], bg[2]);
                        swash_cache.with_pixels(font_system, pos.cache_key, base, |px, py, color| {
                            canvas.blend_pixel(x + pos.x + px, y + pos.y + py, color);
                        });
                    }
                }
            } else {
                match content.cursor.shape {
                    CursorShape::Beam => canvas.fill_rect(x, y, 2, cell_h, cursor_color),
                    CursorShape::Underline => {
                        canvas.fill_rect(x, y + cell_h - 2, cell_w, 2, cursor_color)
                    }
                    // Unfocused block (and HollowBlock) renders as an outline.
                    _ => {
                        canvas.fill_rect(x, y, cell_w, 1, cursor_color);
                        canvas.fill_rect(x, y + cell_h - 1, cell_w, 1, cursor_color);
                        canvas.fill_rect(x, y, 1, cell_h, cursor_color);
                        canvas.fill_rect(x + cell_w - 1, y, 1, cell_h, cursor_color);
                    }
                }
            }
        }

        buffer
    }
}
