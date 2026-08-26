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
/// (font, size, scale); glyph positions are cached per distinct cell cluster.
pub struct TermRenderer {
    font_family: String,
    font_size_px: f32,
    pub cell_w: u32,
    pub cell_h: u32,
    /// Shaped glyphs per cell cluster, indexed `[bold as usize]` so a lookup
    /// can be keyed by `&str` without building an owned key per cell.
    clusters: [HashMap<String, Vec<GlyphPos>>; 2],
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
            clusters: [HashMap::new(), HashMap::new()],
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

    /// The glyphs one cell draws: its character plus the zero-width marks
    /// stacked on it. Thai spells a syllable that way — a consonant carrying a
    /// vowel above or below and a tone mark over that — and the marks only sit
    /// where they belong when the whole stack is shaped in one go, so the
    /// cluster is the unit that gets shaped and cached, not the character.
    fn cluster(&mut self, font_system: &mut FontSystem, text: &str, bold: bool) -> &[GlyphPos] {
        let slot = bold as usize;
        if !self.clusters[slot].contains_key(text) {
            let attrs = Attrs::new().family(Family::Name(&self.font_family));
            let attrs = if bold { attrs.weight(Weight::BOLD) } else { attrs };
            self.shape_buffer.set_text(text, &attrs, Shaping::Advanced, None);
            self.shape_buffer.shape_until_scroll(font_system, false);
            let shaped = self
                .shape_buffer
                .layout_runs()
                .next()
                .map(|run| {
                    run.glyphs
                        .iter()
                        .map(|glyph| {
                            let physical = glyph.physical((0.0, 0.0), 1.0);
                            GlyphPos {
                                cache_key: physical.cache_key,
                                x: physical.x,
                                y: run.line_y as i32 + physical.y,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            self.clusters[slot].insert(text.to_string(), shaped);
        }
        &self.clusters[slot][text]
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
            /// Zero-width marks alacritty stacked onto this cell, if any.
            marks: Option<Box<str>>,
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
            let marks = indexed
                .cell
                .zerowidth()
                .filter(|marks| !marks.is_empty())
                .map(|marks| marks.iter().collect::<String>().into_boxed_str());
            // A cell holding only marks still has something to draw: a lone
            // Thai vowel typed with no consonant before it lands on a space.
            let printable = (c != ' ' && c != '\t') || marks.is_some();
            if printable && !flags.contains(Flags::HIDDEN) {
                let bold = flags.intersects(Flags::BOLD);
                draw_cells.push(DrawCell { c, marks, col, row, fg, bold, flags });
            } else if flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT) {
                draw_cells.push(DrawCell { c: ' ', marks: None, col, row, fg, bold: false, flags });
            }
        }

        // Reused across cells so an ordinary screenful of text shapes without
        // allocating a key per cell.
        let mut cluster_key = String::new();
        for cell in &draw_cells {
            let origin_x = cell.col * cell_w;
            let origin_y = cell.row * cell_h;
            if cell.c != ' ' || cell.marks.is_some() {
                cluster_key.clear();
                cluster_key.push(cell.c);
                if let Some(marks) = &cell.marks {
                    cluster_key.push_str(marks);
                }
                let fg = cell.fg;
                let base = cosmic_text::Color::rgb(fg[0], fg[1], fg[2]);
                for pos in self.cluster(font_system, &cluster_key, cell.bold) {
                    let pos = *pos;
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
                // Redraw the glyph under the cursor in background color, marks
                // and all — a Thai syllable sitting under a block cursor is
                // otherwise left showing its bare consonant.
                let under_cell = &term.grid()[content.cursor.point];
                let mut under: String = under_cell.c.to_string();
                under.extend(under_cell.zerowidth().into_iter().flatten());
                if under != " " {
                    let base = cosmic_text::Color::rgb(bg[0], bg[1], bg[2]);
                    for pos in self.cluster(font_system, &under, false) {
                        let pos = *pos;
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


#[cfg(test)]
mod tests {
    use super::*;

    /// A Thai syllable is one grid cell: a consonant carrying a vowel and a
    /// tone mark, both zero-width. Every mark has to reach the rasterizer, and
    /// above the letter rather than beside it, or the terminal shows Thai
    /// stripped down to bare consonants.
    #[test]
    fn a_cell_draws_the_marks_stacked_on_it() {
        let mut font_system = FontSystem::new();
        let mut renderer = TermRenderer::new("Menlo", 14.0, &mut font_system);
        let bare = renderer.cluster(&mut font_system, "ท", false).len();
        assert_eq!(bare, 1, "a consonant on its own is one glyph");
        // ท + SARA II + MAI EK, the three codepoints behind "ที่".
        let stacked = renderer.cluster(&mut font_system, "ท\u{0e35}\u{0e48}", false).to_vec();
        assert_eq!(stacked.len(), 3, "consonant, vowel and tone mark each draw");

        // Shaped together, the marks rasterize clear of the consonant's own
        // ink; shaped one codepoint at a time they would pile onto it.
        let mut swash_cache = SwashCache::new();
        let tops: Vec<i32> = stacked
            .iter()
            .map(|glyph| top_of_ink(&mut font_system, &mut swash_cache, glyph))
            .collect();
        assert!(
            tops[1..].iter().all(|mark| *mark < tops[0]),
            "the marks sit above the consonant, not over it: {tops:?}"
        );
    }

    /// The topmost row this glyph paints, in the cell's own coordinates.
    fn top_of_ink(
        font_system: &mut FontSystem,
        swash_cache: &mut SwashCache,
        glyph: &GlyphPos,
    ) -> i32 {
        let mut top = i32::MAX;
        let white = cosmic_text::Color::rgb(255, 255, 255);
        swash_cache.with_pixels(font_system, glyph.cache_key, white, |_x, y, color| {
            if color.a() > 0 {
                top = top.min(glyph.y + y);
            }
        });
        top
    }
}
