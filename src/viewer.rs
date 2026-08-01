use std::path::{Path, PathBuf};

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight, Wrap};
use image::RgbaImage;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use slint::{Rgba8Pixel, SharedPixelBuffer};

use crate::paint::Canvas;
use crate::term::colors;

#[derive(Clone, Copy, PartialEq)]
pub enum ViewKind {
    Image,
    Markdown,
    Csv,
    PdfText,
}

enum Block {
    Text { buffer: Buffer, indent: f32, bg: Option<[u8; 3]>, height: f32 },
    Picture { scaled: RgbaImage, height: f32 },
    Rule,
}

/// A read-only rendered view of a file (image / markdown / table / pdf text),
/// drawn into the editor pane's pixel buffer.
pub struct ViewerState {
    pub path: PathBuf,
    pub kind: ViewKind,
    pub scroll: f32,
    blocks: Vec<Block>,
    /// Original images kept for re-scaling on resize.
    sources: Vec<Option<RgbaImage>>,
    width_px: f32,
    content_h: f32,
    margin: f32,
    spacing: f32,
    font_px: f32,
    dark: bool,
    font_family: &'static str,
}

fn mono(family: &'static str) -> Attrs<'static> {
    Attrs::new().family(Family::Name(family))
}

fn ui_attrs() -> Attrs<'static> {
    Attrs::new().family(Family::SansSerif)
}

impl ViewerState {
    pub fn open(
        font_system: &mut FontSystem,
        path: &Path,
        kind: ViewKind,
        font_family: &'static str,
        font_px: f32,
        dark: bool,
        width_px: f32,
    ) -> Result<Self, String> {
        let mut viewer = Self {
            path: path.to_path_buf(),
            kind,
            scroll: 0.0,
            blocks: Vec::new(),
            sources: Vec::new(),
            width_px: width_px.max(64.0),
            content_h: 0.0,
            margin: (font_px * 1.2).round(),
            spacing: (font_px * 0.5).round(),
            font_px,
            dark,
            font_family,
        };
        match kind {
            ViewKind::Image => viewer.build_image(path)?,
            ViewKind::Markdown => viewer.build_markdown(font_system, path)?,
            ViewKind::Csv => viewer.build_csv(font_system, path)?,
            ViewKind::PdfText => viewer.build_pdf(font_system, path)?,
        }
        viewer.reflow(font_system);
        Ok(viewer)
    }

    fn fg(&self) -> [u8; 3] {
        colors::base_palette(self.dark)[7]
    }

    fn text_color(&self) -> Color {
        let c = self.fg();
        Color::rgb(c[0], c[1], c[2])
    }

    fn text_width(&self) -> f32 {
        (self.width_px - 2.0 * self.margin).max(48.0)
    }

    fn push_spans(
        &mut self,
        font_system: &mut FontSystem,
        spans: &[(String, Attrs<'static>)],
        base_px: f32,
        indent: f32,
        bg: Option<[u8; 3]>,
    ) {
        if spans.iter().all(|(t, _)| t.trim().is_empty()) {
            return;
        }
        let mut buffer =
            Buffer::new(font_system, Metrics::new(base_px, (base_px * 1.45).round()));
        buffer.set_wrap(Wrap::WordOrGlyph);
        let default = ui_attrs().color(self.text_color());
        buffer.set_rich_text(
            spans.iter().map(|(t, a)| (t.as_str(), a.clone())),
            &default,
            Shaping::Advanced,
            None,
        );
        self.blocks.push(Block::Text { buffer, indent, bg, height: 0.0 });
        self.sources.push(None);
    }

    fn push_plain(&mut self, font_system: &mut FontSystem, text: &str, attrs: Attrs<'static>, wrap: Wrap) {
        let mut buffer =
            Buffer::new(font_system, Metrics::new(self.font_px, (self.font_px * 1.45).round()));
        buffer.set_wrap(wrap);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        self.blocks.push(Block::Text { buffer, indent: 0.0, bg: None, height: 0.0 });
        self.sources.push(None);
    }

    fn push_image_file(&mut self, path: &Path) -> bool {
        match image::open(path) {
            Ok(img) => {
                self.blocks.push(Block::Picture { scaled: RgbaImage::new(1, 1), height: 0.0 });
                self.sources.push(Some(img.to_rgba8()));
                true
            }
            Err(_) => false,
        }
    }

    fn build_image(&mut self, path: &Path) -> Result<(), String> {
        if !self.push_image_file(path) {
            return Err(format!("cannot decode image {}", path.display()));
        }
        Ok(())
    }

    fn build_pdf(&mut self, font_system: &mut FontSystem, path: &Path) -> Result<(), String> {
        let text = pdf_extract::extract_text(path)
            .map_err(|e| format!("cannot read pdf: {e}"))?;
        let text = if text.trim().is_empty() { "(no extractable text in this PDF)".into() } else { text };
        let attrs = ui_attrs().color(self.text_color());
        self.push_plain(font_system, &text, attrs, Wrap::WordOrGlyph);
        Ok(())
    }

    fn build_csv(&mut self, font_system: &mut FontSystem, path: &Path) -> Result<(), String> {
        const MAX_ROWS: usize = 1000;
        const MAX_CELL: usize = 40;
        let delimiter = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("tsv")) {
            b'\t'
        } else {
            b','
        };
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .delimiter(delimiter)
            .flexible(true)
            .from_path(path)
            .map_err(|e| format!("cannot read csv: {e}"))?;
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut truncated = false;
        for record in reader.records() {
            let Ok(record) = record else { continue };
            if rows.len() >= MAX_ROWS {
                truncated = true;
                break;
            }
            rows.push(
                record
                    .iter()
                    .map(|c| {
                        let mut cell: String = c.chars().take(MAX_CELL).collect();
                        if c.chars().count() > MAX_CELL {
                            cell.push('…');
                        }
                        cell
                    })
                    .collect(),
            );
        }
        if rows.is_empty() {
            return Err("empty csv".into());
        }
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut widths = vec![0usize; cols];
        for row in &rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }
        let fmt_row = |row: &Vec<String>| -> String {
            (0..cols)
                .map(|i| {
                    let cell = row.get(i).map(String::as_str).unwrap_or("");
                    format!("{cell:<width$}", width = widths[i])
                })
                .collect::<Vec<_>>()
                .join("  ")
        };
        let mut out = String::new();
        out.push_str(&fmt_row(&rows[0]));
        out.push('\n');
        out.push_str(&"─".repeat(widths.iter().sum::<usize>() + 2 * (cols.saturating_sub(1))));
        out.push('\n');
        for row in &rows[1..] {
            out.push_str(&fmt_row(row));
            out.push('\n');
        }
        if truncated {
            out.push_str(&format!("… (showing first {MAX_ROWS} rows)\n"));
        }
        let attrs = mono(self.font_family).color(self.text_color());
        self.push_plain(font_system, &out, attrs, Wrap::None);
        Ok(())
    }

    fn build_markdown(&mut self, font_system: &mut FontSystem, path: &Path) -> Result<(), String> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let base_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let accent: [u8; 3] = [0xe8, 0x91, 0x2d];
        let code_bg = if self.dark { [0x2b, 0x31, 0x38] } else { [0xe8, 0xe8, 0xe8] };

        let mut spans: Vec<(String, Attrs<'static>)> = Vec::new();
        let mut bold = 0usize;
        let mut italic = 0usize;
        let mut link = 0usize;
        let mut heading: Option<f32> = None;
        let mut in_code_block = false;
        let mut code_text = String::new();
        let mut list_stack: Vec<Option<u64>> = Vec::new();
        let mut quote_depth = 0usize;

        macro_rules! flush {
            ($self:ident, $spans:ident, $heading:ident, $list_stack:ident, $quote_depth:ident) => {{
                let base = $heading.unwrap_or($self.font_px);
                let indent = ($list_stack.len() as f32 * 1.5 + $quote_depth as f32 * 1.5)
                    * $self.font_px;
                let taken = std::mem::take(&mut $spans);
                $self.push_spans(font_system, &taken, base, indent, None);
            }};
        }

        let fg = self.text_color();
        let attrs_for = |bold: usize, italic: usize, link: usize, code: bool, fam: &'static str| {
            let mut attrs = if code { mono(fam) } else { ui_attrs() };
            attrs = attrs.color(fg);
            if bold > 0 {
                attrs = attrs.weight(Weight::BOLD);
            }
            if italic > 0 {
                attrs = attrs.style(Style::Italic);
            }
            if link > 0 || code {
                attrs = attrs.color(Color::rgb(accent[0], accent[1], accent[2]));
            }
            attrs
        };

        let parser = Parser::new_ext(&source, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH);
        for event in parser {
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    flush!(self, spans, heading, list_stack, quote_depth);
                    let scale = match level {
                        HeadingLevel::H1 => 1.7,
                        HeadingLevel::H2 => 1.4,
                        HeadingLevel::H3 => 1.2,
                        _ => 1.05,
                    };
                    heading = Some((self.font_px * scale).round());
                    bold += 1;
                }
                Event::End(TagEnd::Heading(_)) => {
                    flush!(self, spans, heading, list_stack, quote_depth);
                    heading = None;
                    bold = bold.saturating_sub(1);
                }
                Event::Start(Tag::Paragraph) => {}
                Event::End(TagEnd::Paragraph) => flush!(self, spans, heading, list_stack, quote_depth),
                Event::Start(Tag::Strong) => bold += 1,
                Event::End(TagEnd::Strong) => bold = bold.saturating_sub(1),
                Event::Start(Tag::Emphasis) => italic += 1,
                Event::End(TagEnd::Emphasis) => italic = italic.saturating_sub(1),
                Event::Start(Tag::Link { .. }) => link += 1,
                Event::End(TagEnd::Link) => link = link.saturating_sub(1),
                Event::Start(Tag::BlockQuote(_)) => {
                    flush!(self, spans, heading, list_stack, quote_depth);
                    quote_depth += 1;
                }
                Event::End(TagEnd::BlockQuote(_)) => {
                    flush!(self, spans, heading, list_stack, quote_depth);
                    quote_depth = quote_depth.saturating_sub(1);
                }
                Event::Start(Tag::List(start)) => {
                    flush!(self, spans, heading, list_stack, quote_depth);
                    list_stack.push(start);
                }
                Event::End(TagEnd::List(_)) => {
                    flush!(self, spans, heading, list_stack, quote_depth);
                    list_stack.pop();
                }
                Event::Start(Tag::Item) => {
                    let marker = match list_stack.last_mut() {
                        Some(Some(n)) => {
                            let m = format!("{n}. ");
                            *n += 1;
                            m
                        }
                        _ => "•  ".to_string(),
                    };
                    spans.push((marker, attrs_for(1, 0, 0, false, self.font_family)));
                }
                Event::End(TagEnd::Item) => flush!(self, spans, heading, list_stack, quote_depth),
                Event::Start(Tag::CodeBlock(_)) => {
                    flush!(self, spans, heading, list_stack, quote_depth);
                    in_code_block = true;
                    code_text.clear();
                }
                Event::End(TagEnd::CodeBlock) => {
                    in_code_block = false;
                    let attrs = mono(self.font_family).color(fg);
                    let text = code_text.trim_end().to_string();
                    if !text.is_empty() {
                        let mut buffer = Buffer::new(
                            font_system,
                            Metrics::new(self.font_px, (self.font_px * 1.45).round()),
                        );
                        buffer.set_wrap(Wrap::WordOrGlyph);
                        buffer.set_text(&text, &attrs, Shaping::Advanced, None);
                        self.blocks.push(Block::Text {
                            buffer,
                            indent: 0.0,
                            bg: Some(code_bg),
                            height: 0.0,
                        });
                        self.sources.push(None);
                    }
                }
                Event::Start(Tag::Image { dest_url, .. }) => {
                    flush!(self, spans, heading, list_stack, quote_depth);
                    let url = dest_url.to_string();
                    if !url.starts_with("http") {
                        let img_path = base_dir.join(&url);
                        if !self.push_image_file(&img_path) {
                            spans.push((
                                format!("[image: {url}]"),
                                attrs_for(0, 1, 0, false, self.font_family),
                            ));
                        }
                    } else {
                        spans.push((
                            format!("[image: {url}]"),
                            attrs_for(0, 1, 1, false, self.font_family),
                        ));
                    }
                }
                Event::End(TagEnd::Image) => {}
                Event::Text(text) => {
                    if in_code_block {
                        code_text.push_str(&text);
                    } else {
                        spans.push((
                            text.to_string(),
                            attrs_for(bold, italic, link, false, self.font_family),
                        ));
                    }
                }
                Event::Code(code) => {
                    spans.push((code.to_string(), attrs_for(bold, italic, 0, true, self.font_family)));
                }
                Event::SoftBreak => spans.push((" ".into(), attrs_for(0, 0, 0, false, self.font_family))),
                Event::HardBreak => spans.push(("\n".into(), attrs_for(0, 0, 0, false, self.font_family))),
                Event::Rule => {
                    flush!(self, spans, heading, list_stack, quote_depth);
                    self.blocks.push(Block::Rule);
                    self.sources.push(None);
                }
                _ => {}
            }
        }
        flush!(self, spans, heading, list_stack, quote_depth);
        if self.blocks.is_empty() {
            let attrs = ui_attrs().color(fg);
            self.push_plain(font_system, "(empty file)", attrs, Wrap::WordOrGlyph);
        }
        Ok(())
    }

    /// Recomputes block sizes for the current width.
    fn reflow(&mut self, font_system: &mut FontSystem) {
        let text_w = self.text_width();
        let mut total = self.margin;
        for (block, source) in self.blocks.iter_mut().zip(self.sources.iter()) {
            match block {
                Block::Text { buffer, indent, bg, height } => {
                    buffer.set_size(Some((text_w - *indent).max(48.0)), None);
                    buffer.shape_until_scroll(font_system, false);
                    let mut h = 0.0f32;
                    for run in buffer.layout_runs() {
                        h = h.max(run.line_top + run.line_height);
                    }
                    *height = h + if bg.is_some() { self.font_px } else { 0.0 };
                    total += *height + self.spacing;
                }
                Block::Picture { scaled, height } => {
                    if let Some(original) = source {
                        let (ow, oh) = (original.width() as f32, original.height() as f32);
                        let draw_w = ow.min(text_w).max(1.0);
                        let draw_h = (oh * draw_w / ow).max(1.0);
                        if scaled.width() != draw_w as u32 || scaled.height() != draw_h as u32 {
                            *scaled = image::imageops::resize(
                                original,
                                draw_w as u32,
                                draw_h as u32,
                                image::imageops::FilterType::Triangle,
                            );
                        }
                        *height = draw_h;
                        total += draw_h + self.spacing;
                    }
                }
                Block::Rule => total += self.font_px + self.spacing,
            }
        }
        self.content_h = total + self.margin;
        self.clamp_scroll();
    }

    pub fn set_viewport(&mut self, font_system: &mut FontSystem, width_px: f32, _height_px: f32) {
        if (width_px - self.width_px).abs() > 1.0 {
            self.width_px = width_px.max(64.0);
            self.reflow(font_system);
        }
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.clamp(0.0, (self.content_h - 100.0).max(0.0));
    }

    pub fn scroll_by(&mut self, delta_px: f32) {
        self.scroll -= delta_px;
        self.clamp_scroll();
    }

    pub fn render(
        &mut self,
        font_system: &mut FontSystem,
        swash_cache: &mut SwashCache,
        width_px: u32,
        height_px: u32,
    ) -> SharedPixelBuffer<Rgba8Pixel> {
        let mut frame = SharedPixelBuffer::<Rgba8Pixel>::new(width_px.max(1), height_px.max(1));
        let bg = colors::base_palette(self.dark)[0];
        let (w, h) = (frame.width() as i32, frame.height() as i32);
        let mut canvas = Canvas { pixels: frame.make_mut_slice(), width: w, height: h };
        canvas.fill(bg);

        let mut y = self.margin - self.scroll;
        let margin = self.margin;
        let text_w = self.text_width();
        let fg = self.fg();
        let default_color = Color::rgb(fg[0], fg[1], fg[2]);
        for (block, _) in self.blocks.iter_mut().zip(self.sources.iter()) {
            match block {
                Block::Text { buffer, indent, bg: block_bg, height } => {
                    if y + *height >= 0.0 && y <= h as f32 {
                        let x0 = (margin + *indent) as i32;
                        let pad = if block_bg.is_some() { (self.font_px / 2.0) as i32 } else { 0 };
                        if let Some(color) = block_bg {
                            canvas.fill_rect(
                                x0 - pad,
                                y as i32,
                                (text_w - *indent) as i32 + 2 * pad,
                                *height as i32,
                                *color,
                            );
                        }
                        let oy = y as i32 + pad;
                        buffer.draw(font_system, swash_cache, default_color, |px, py, pw, ph, color| {
                            canvas.blend_rect(x0 + px, oy + py, pw as i32, ph as i32, color);
                        });
                    }
                    y += *height + self.spacing;
                }
                Block::Picture { scaled, height } => {
                    if y + *height >= 0.0 && y <= h as f32 {
                        blit_image(&mut canvas, scaled, margin as i32, y as i32);
                    }
                    y += *height + self.spacing;
                }
                Block::Rule => {
                    let ry = (y + self.font_px / 2.0) as i32;
                    let dim = colors::base_palette(self.dark)[8];
                    canvas.fill_rect(margin as i32, ry, text_w as i32, 1, dim);
                    y += self.font_px + self.spacing;
                }
            }
        }
        frame
    }
}

fn blit_image(canvas: &mut Canvas, img: &RgbaImage, x0: i32, y0: i32) {
    let iw = img.width() as i32;
    for (row_idx, row) in img.rows().enumerate() {
        let y = y0 + row_idx as i32;
        if y < 0 || y >= canvas.height {
            continue;
        }
        for (col_idx, px) in row.enumerate() {
            let x = x0 + col_idx as i32;
            if x < 0 || x >= iw + x0 || x >= canvas.width {
                continue;
            }
            let [r, g, b, a] = px.0;
            if a == 0 {
                continue;
            }
            canvas.blend_pixel(x, y, Color::rgba(r, g, b, a));
        }
    }
}

/// Classifies a path by extension into a viewer kind (None = open in editor).
pub fn classify(path: &Path) -> Option<ViewKind> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" => Some(ViewKind::Image),
        "md" | "markdown" => Some(ViewKind::Markdown),
        "csv" | "tsv" => Some(ViewKind::Csv),
        "pdf" => Some(ViewKind::PdfText),
        _ => None,
    }
}
