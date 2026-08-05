use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight, Wrap};
use image::RgbaImage;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use slint::{Rgba8Pixel, SharedPixelBuffer};

use crate::paint::Canvas;
use crate::term::colors;
use crate::theme::ThemeDef;

#[derive(Clone, Copy, PartialEq)]
pub enum ViewKind {
    Image,
    Markdown,
    Csv,
    Pdf,
}

/// Zoom bounds for images and PDF pages (factor over the fit-to-width size).
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 8.0;
/// Rasterization caps: a zoomed rescale must never allocate unbounded pixels.
const MAX_DRAW_W: f32 = 4096.0;
const MAX_DRAW_H: f32 = 16384.0;
/// Scrollbar (physical px): track width and minimum thumb height.
const SCROLLBAR_W: f32 = 12.0;
const SCROLLBAR_MIN_THUMB: f32 = 32.0;

/// Clamps a target draw width so the resulting bitmap stays within the caps,
/// preserving the w:h aspect ratio.
fn fit_draw(w: f32, h: f32, target_w: f32) -> (f32, f32) {
    let mut dw = target_w.clamp(1.0, MAX_DRAW_W);
    let mut dh = (h * dw / w.max(1.0)).max(1.0);
    if dh > MAX_DRAW_H {
        dh = MAX_DRAW_H;
        dw = (w * dh / h.max(1.0)).max(1.0);
    }
    (dw, dh)
}

enum Block {
    Text { buffer: Buffer, indent: f32, bg: Option<[u8; 3]>, height: f32 },
    /// An image; `size` is the original dimensions (layout comes from the
    /// header alone) and `scaled` the latest bitmap from the image worker —
    /// empty until the first one arrives.
    Picture { scaled: RgbaImage, size: (f32, f32), height: f32 },
    /// One PDF page, rasterized lazily when it scrolls into view.
    Page { index: usize, size: (f32, f32), height: f32 },
    Rule,
    Table {
        /// rows -> cells; the first row is the header.
        rows: Vec<Vec<Buffer>>,
        font_px: f32,
        col_widths: Vec<f32>,
        row_heights: Vec<f32>,
        height: f32,
    },
}

/// A read-only rendered view of a file (image / markdown / table / pdf text),
/// drawn into the editor pane's pixel buffer.
pub struct ViewerState {
    pub path: PathBuf,
    pub kind: ViewKind,
    pub scroll: f32,
    blocks: Vec<Block>,
    width_px: f32,
    content_h: f32,
    /// Widest block at the current zoom (plus margins); > width_px pans.
    content_w: f32,
    /// Horizontal pan, used when a zoomed image/page is wider than the pane.
    scroll_x: f32,
    /// Magnification over the fit-to-width size for images and PDF pages.
    zoom: f32,
    /// Background rasterizer that owns the parsed PDF; None for non-PDF
    /// views and for the text-extraction fallback.
    worker: Option<PdfWorker>,
    /// Rasterized pages by index; the bitmap width encodes the render width.
    page_cache: HashMap<usize, RgbaImage>,
    /// Widths requested from the worker but not yet delivered, by page index.
    pending: HashMap<usize, u32>,
    /// Background decoder/rescaler owning the original image bitmaps.
    img_worker: Option<ImgWorker>,
    /// Sizes requested from the image worker but not yet delivered, by block.
    pending_imgs: HashMap<usize, (u32, u32)>,
    /// (block index, path) of images found during build; consumed by `open`
    /// to spawn the image worker.
    img_paths: Vec<(usize, PathBuf)>,
    /// Some while the scrollbar thumb is dragged: grab offset within the thumb.
    drag: Option<f32>,
    margin: f32,
    spacing: f32,
    font_px: f32,
    theme: &'static ThemeDef,
    /// Effective accent (theme default or the user's override).
    accent: [u8; 3],
    font_family: &'static str,
}

/// Called from worker threads whenever a bitmap is ready; the app uses it to
/// schedule a repaint on the UI event loop.
pub type Notify = std::sync::Arc<dyn Fn() + Send + Sync + 'static>;

/// Handle to a per-document rasterizer thread. Dropping it hangs up the
/// request channel, which shuts the thread down.
struct PdfWorker {
    req_tx: Sender<(usize, u32)>,
    res_rx: Receiver<(usize, u32, RgbaImage)>,
    /// Pages near the viewport right now; lets the worker skip queued
    /// requests for pages a fast scroll has already left behind.
    wanted: Arc<Mutex<HashSet<usize>>>,
}

/// Parses the PDF on a dedicated thread and returns the page sizes plus a
/// handle for requesting page bitmaps, or None if hayro cannot parse it.
///
/// The thread keeps the parsed document and one hayro `RenderCache` alive for
/// its whole life: fonts, images and outlines decoded once are reused for
/// every page and re-render, and the UI thread never blocks on rasterization.
fn spawn_pdf_worker(bytes: Vec<u8>, notify: Notify) -> Option<(PdfWorker, Vec<(f32, f32)>)> {
    let (req_tx, req_rx) = mpsc::channel::<(usize, u32)>();
    let (res_tx, res_rx) = mpsc::channel();
    let (init_tx, init_rx) = mpsc::channel();
    let wanted = Arc::new(Mutex::new(HashSet::new()));
    let wanted_worker = wanted.clone();
    std::thread::spawn(move || {
        let Ok(pdf) = hayro::hayro_syntax::Pdf::new(bytes) else {
            let _ = init_tx.send(None);
            return;
        };
        let sizes: Vec<(f32, f32)> = pdf.pages().iter().map(|p| p.render_dimensions()).collect();
        if init_tx.send(Some(sizes)).is_err() {
            return;
        }
        let pages = pdf.pages();
        let cache = hayro::RenderCache::new();
        while let Ok(first) = req_rx.recv() {
            let mut order = std::collections::VecDeque::from([first.0]);
            let mut want = HashMap::from([first]);
            loop {
                // Latest width wins: re-drain before every render so a zoom
                // gesture collapses to the final width per page instead of
                // rasterizing every intermediate size.
                for (index, width) in req_rx.try_iter() {
                    if want.insert(index, width).is_none() {
                        order.push_back(index);
                    }
                }
                let Some(index) = order.pop_front() else { break };
                let Some(width) = want.remove(&index) else { continue };
                if !wanted_worker.lock().is_ok_and(|w| w.contains(&index)) {
                    continue;
                }
                if let Some(img) = rasterize_page(pages, index, width, &cache) {
                    if res_tx.send((index, width, img)).is_err() {
                        return;
                    }
                    notify();
                }
            }
        }
    });
    init_rx
        .recv()
        .ok()
        .flatten()
        .map(|sizes| (PdfWorker { req_tx, res_rx, wanted }, sizes))
}

/// Handle to a per-viewer image thread that decodes the originals and serves
/// scaled copies; dropping it (with the request sender) shuts it down.
struct ImgWorker {
    req_tx: Sender<(usize, u32, u32)>,
    res_rx: Receiver<(usize, RgbaImage)>,
}

/// Decoding and high-quality rescaling of photos is far too slow for the UI
/// thread (each zoom tick used to re-run a full Triangle resample). The
/// worker owns the decoded originals; requests are latest-wins per image so
/// a zoom gesture collapses to the final size.
fn spawn_img_worker(images: Vec<(usize, PathBuf)>, notify: Notify) -> ImgWorker {
    let (req_tx, req_rx) = mpsc::channel::<(usize, u32, u32)>();
    let (res_tx, res_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut originals: HashMap<usize, RgbaImage> = HashMap::new();
        for (slot, path) in images {
            if let Ok(img) = image::open(&path) {
                originals.insert(slot, img.to_rgba8());
            }
        }
        while let Ok(first) = req_rx.recv() {
            let mut order = std::collections::VecDeque::from([first.0]);
            let mut want = HashMap::from([(first.0, (first.1, first.2))]);
            loop {
                for (slot, w, h) in req_rx.try_iter() {
                    if want.insert(slot, (w, h)).is_none() {
                        order.push_back(slot);
                    }
                }
                let Some(slot) = order.pop_front() else { break };
                let Some((w, h)) = want.remove(&slot) else { continue };
                let Some(original) = originals.get(&slot) else { continue };
                let scaled = if original.width() == w && original.height() == h {
                    original.clone()
                } else {
                    image::imageops::resize(
                        original,
                        w.max(1),
                        h.max(1),
                        image::imageops::FilterType::Triangle,
                    )
                };
                if res_tx.send((slot, scaled)).is_err() {
                    return;
                }
                notify();
            }
        }
    });
    ImgWorker { req_tx, res_rx }
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
        theme: &'static ThemeDef,
        accent: [u8; 3],
        width_px: f32,
        notify: Notify,
    ) -> Result<Self, String> {
        let mut viewer = Self {
            path: path.to_path_buf(),
            kind,
            scroll: 0.0,
            blocks: Vec::new(),
            width_px: width_px.max(64.0),
            content_h: 0.0,
            content_w: 0.0,
            scroll_x: 0.0,
            zoom: 1.0,
            worker: None,
            page_cache: HashMap::new(),
            pending: HashMap::new(),
            img_worker: None,
            pending_imgs: HashMap::new(),
            img_paths: Vec::new(),
            drag: None,
            margin: (font_px * 1.2).round(),
            spacing: (font_px * 0.5).round(),
            font_px,
            theme,
            accent,
            font_family,
        };
        match kind {
            ViewKind::Image => viewer.build_image(path)?,
            ViewKind::Markdown => viewer.build_markdown(font_system, path)?,
            ViewKind::Csv => viewer.build_csv(font_system, path)?,
            ViewKind::Pdf => viewer.build_pdf(font_system, path, notify.clone())?,
        }
        if !viewer.img_paths.is_empty() {
            viewer.img_worker = Some(spawn_img_worker(std::mem::take(&mut viewer.img_paths), notify));
        }
        viewer.reflow(font_system);
        Ok(viewer)
    }

    fn fg(&self) -> [u8; 3] {
        colors::base_palette(self.theme)[7]
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
    }

    fn push_plain(&mut self, font_system: &mut FontSystem, text: &str, attrs: Attrs<'static>, wrap: Wrap) {
        let mut buffer =
            Buffer::new(font_system, Metrics::new(self.font_px, (self.font_px * 1.45).round()));
        buffer.set_wrap(wrap);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        self.blocks.push(Block::Text { buffer, indent: 0.0, bg: None, height: 0.0 });
    }

    fn push_table(
        &mut self,
        font_system: &mut FontSystem,
        rows: Vec<Vec<Vec<(String, Attrs<'static>)>>>,
    ) {
        if rows.iter().all(|row| row.iter().all(|cell| cell.iter().all(|(t, _)| t.trim().is_empty()))) {
            return;
        }
        // Slightly smaller than body text so wide tables fit more per column.
        let px = (self.font_px * 0.9).round().max(8.0);
        let default = ui_attrs().color(self.text_color());
        let mut buffers: Vec<Vec<Buffer>> = Vec::with_capacity(rows.len());
        for cells in rows {
            let mut row = Vec::with_capacity(cells.len());
            for cell in cells {
                let mut buffer = Buffer::new(font_system, Metrics::new(px, (px * 1.35).round()));
                buffer.set_wrap(Wrap::WordOrGlyph);
                buffer.set_rich_text(
                    cell.iter().map(|(t, a)| (t.as_str(), a.clone())),
                    &default,
                    Shaping::Advanced,
                    None,
                );
                row.push(buffer);
            }
            buffers.push(row);
        }
        self.blocks.push(Block::Table {
            rows: buffers,
            font_px: px,
            col_widths: Vec::new(),
            row_heights: Vec::new(),
            height: 0.0,
        });
    }

    /// Registers an image block from its header dimensions alone; the worker
    /// decodes the pixels later, off the UI thread.
    fn push_image_file(&mut self, path: &Path) -> bool {
        match image::image_dimensions(path) {
            Ok((w, h)) => {
                self.blocks.push(Block::Picture {
                    scaled: RgbaImage::new(0, 0),
                    size: (w as f32, h as f32),
                    height: 0.0,
                });
                self.img_paths.push((self.blocks.len() - 1, path.to_path_buf()));
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

    /// Renders the actual PDF pages (a worker thread rasterizes them as they
    /// scroll into view). Files hayro cannot parse (e.g. encrypted) fall back
    /// to plain text extraction so something still shows.
    fn build_pdf(&mut self, font_system: &mut FontSystem, path: &Path, notify: Notify) -> Result<(), String> {
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        match spawn_pdf_worker(bytes, notify) {
            Some((worker, sizes)) if !sizes.is_empty() => {
                for (index, size) in sizes.into_iter().enumerate() {
                    self.blocks.push(Block::Page { index, size, height: 0.0 });
                }
                self.worker = Some(worker);
                Ok(())
            }
            _ => self.build_pdf_text(font_system, path),
        }
    }

    fn build_pdf_text(&mut self, font_system: &mut FontSystem, path: &Path) -> Result<(), String> {
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
        let accent = self.accent;
        let code_bg = self.theme.ui.panel_hover;

        let mut spans: Vec<(String, Attrs<'static>)> = Vec::new();
        let mut bold = 0usize;
        let mut italic = 0usize;
        let mut link = 0usize;
        let mut heading: Option<f32> = None;
        let mut in_code_block = false;
        let mut code_text = String::new();
        let mut list_stack: Vec<Option<u64>> = Vec::new();
        let mut quote_depth = 0usize;
        // rows -> cells -> styled spans; Some while inside a table.
        let mut table: Option<Vec<Vec<Vec<(String, Attrs<'static>)>>>> = None;
        let mut table_row: Vec<Vec<(String, Attrs<'static>)>> = Vec::new();

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
                Event::Start(Tag::Table(_)) => {
                    flush!(self, spans, heading, list_stack, quote_depth);
                    table = Some(Vec::new());
                }
                Event::End(TagEnd::Table) => {
                    if let Some(rows) = table.take() {
                        self.push_table(font_system, rows);
                    }
                    spans.clear();
                }
                Event::Start(Tag::TableHead) => {
                    bold += 1;
                    table_row.clear();
                    spans.clear();
                }
                Event::End(TagEnd::TableHead) => {
                    bold = bold.saturating_sub(1);
                    if let Some(rows) = table.as_mut() {
                        rows.push(std::mem::take(&mut table_row));
                    }
                }
                Event::Start(Tag::TableRow) => {
                    table_row.clear();
                    spans.clear();
                }
                Event::End(TagEnd::TableRow) => {
                    if let Some(rows) = table.as_mut() {
                        rows.push(std::mem::take(&mut table_row));
                    }
                }
                Event::Start(Tag::TableCell) => spans.clear(),
                Event::End(TagEnd::TableCell) => table_row.push(std::mem::take(&mut spans)),
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

    /// Recomputes block sizes for the current width and zoom.
    fn reflow(&mut self, font_system: &mut FontSystem) {
        let text_w = self.text_width();
        let zoom = self.zoom;
        let mut total = self.margin;
        let mut max_w = text_w;
        for block in self.blocks.iter_mut() {
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
                Block::Picture { size, height, .. } => {
                    let (draw_w, draw_h) = fit_draw(size.0, size.1, size.0.min(text_w) * zoom);
                    *height = draw_h;
                    max_w = max_w.max(draw_w);
                    total += draw_h + self.spacing;
                }
                Block::Page { size, height, .. } => {
                    let (draw_w, draw_h) = fit_draw(size.0, size.1, text_w * zoom);
                    *height = draw_h;
                    max_w = max_w.max(draw_w);
                    total += draw_h + self.spacing;
                }
                Block::Rule => total += self.font_px + self.spacing,
                Block::Table { rows, font_px, col_widths, row_heights, height } => {
                    let ncols = rows.iter().map(Vec::len).max().unwrap_or(0);
                    if ncols == 0 {
                        continue;
                    }
                    let pad_h = (*font_px * 0.5).round();
                    let pad_v = (*font_px * 0.3).round();
                    // Natural (unwrapped) width of each column's widest cell.
                    let mut natural = vec![1.0f32; ncols];
                    for row in rows.iter_mut() {
                        for (i, buffer) in row.iter_mut().enumerate() {
                            buffer.set_size(None, None);
                            buffer.shape_until_scroll(font_system, false);
                            let w = buffer.layout_runs().map(|r| r.line_w).fold(0.0f32, f32::max);
                            natural[i] = natural[i].max(w + 2.0);
                        }
                    }
                    // Width left for cell content after padding and 1px grid lines.
                    let avail =
                        (text_w - ncols as f32 * 2.0 * pad_h - (ncols + 1) as f32).max(ncols as f32 * 16.0);
                    let sum: f32 = natural.iter().sum();
                    let mut cols: Vec<f32> = if sum <= avail {
                        natural
                    } else {
                        // Distribute proportionally, but keep narrow columns readable
                        // by taking the shortfall out of the widest ones.
                        let min_w = (*font_px * 4.0).min(avail / ncols as f32);
                        let mut widths: Vec<f32> = natural.iter().map(|n| avail * n / sum).collect();
                        let mut deficit = 0.0f32;
                        for w in widths.iter_mut() {
                            if *w < min_w {
                                deficit += min_w - *w;
                                *w = min_w;
                            }
                        }
                        if deficit > 0.0 {
                            let flexible: f32 =
                                widths.iter().filter(|w| **w > min_w).map(|w| *w - min_w).sum();
                            if flexible > 0.0 {
                                let k = (deficit / flexible).min(1.0);
                                for w in widths.iter_mut() {
                                    if *w > min_w {
                                        *w -= (*w - min_w) * k;
                                    }
                                }
                            }
                        }
                        widths
                    };
                    for w in cols.iter_mut() {
                        *w = w.max(8.0).round();
                    }
                    let mut heights = Vec::with_capacity(rows.len());
                    for row in rows.iter_mut() {
                        let mut row_h = *font_px * 1.35;
                        for (i, buffer) in row.iter_mut().enumerate() {
                            buffer.set_size(Some(cols[i]), None);
                            buffer.shape_until_scroll(font_system, false);
                            for run in buffer.layout_runs() {
                                row_h = row_h.max(run.line_top + run.line_height);
                            }
                        }
                        heights.push((row_h + 2.0 * pad_v).round());
                    }
                    *height = heights.iter().sum::<f32>() + (rows.len() + 1) as f32;
                    *col_widths = cols;
                    *row_heights = heights;
                    total += *height + self.spacing;
                }
            }
        }
        self.content_h = total + self.margin;
        self.content_w = max_w + 2.0 * self.margin;
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
        self.scroll_x = self.scroll_x.clamp(0.0, (self.content_w - self.width_px).max(0.0));
    }

    pub fn scroll_by(&mut self, delta_x: f32, delta_y: f32) {
        self.scroll -= delta_y;
        self.scroll_x -= delta_x;
        self.clamp_scroll();
    }

    /// PageUp / PageDown: moves nearly one viewport, keeping a little overlap.
    pub fn scroll_page(&mut self, view_h: f32, down: bool) {
        let step = (view_h * 0.9).max(48.0);
        self.scroll += if down { step } else { -step };
        self.clamp_scroll();
    }

    /// Home / End: jumps to the top or the bottom of the document.
    pub fn scroll_home(&mut self, end: bool) {
        self.scroll = if end { f32::MAX } else { 0.0 };
        self.clamp_scroll();
    }

    /// Scrollbar geometry for a viewport `view_h` tall: (track x, thumb top,
    /// thumb height), or None when the content fits without scrolling.
    fn scrollbar(&self, view_h: f32) -> Option<(f32, f32, f32)> {
        if view_h <= 0.0 || self.content_h - view_h <= 1.0 {
            return None;
        }
        let thumb_h = (view_h * view_h / self.content_h)
            .max(SCROLLBAR_MIN_THUMB)
            .min(view_h);
        let t = (self.scroll / self.max_scroll(view_h)).clamp(0.0, 1.0);
        Some((self.width_px - SCROLLBAR_W, t * (view_h - thumb_h), thumb_h))
    }

    /// The scroll position the thumb's bottom stop maps to.
    fn max_scroll(&self, view_h: f32) -> f32 {
        (self.content_h - view_h).max(1.0)
    }

    /// Mouse on the pane (physical px; kind 0=down 1=up 2=move). Consumes
    /// presses on the scrollbar and drags of its thumb; returns whether the
    /// event changed anything worth repainting.
    pub fn handle_mouse(&mut self, kind: i32, x: f32, y: f32, view_h: f32) -> bool {
        match kind {
            0 => {
                let Some((bar_x, thumb_y, thumb_h)) = self.scrollbar(view_h) else {
                    return false;
                };
                if x < bar_x {
                    return false;
                }
                // Grabbing the thumb keeps the cursor's spot on it; a track
                // click jumps there and continues as a centered drag.
                let grab = if y >= thumb_y && y < thumb_y + thumb_h {
                    y - thumb_y
                } else {
                    thumb_h / 2.0
                };
                self.drag = Some(grab);
                self.drag_to(y, grab, view_h);
                true
            }
            2 => match self.drag {
                Some(grab) => {
                    self.drag_to(y, grab, view_h);
                    true
                }
                None => false,
            },
            1 => self.drag.take().is_some(),
            _ => false,
        }
    }

    fn drag_to(&mut self, y: f32, grab: f32, view_h: f32) {
        let Some((_, _, thumb_h)) = self.scrollbar(view_h) else { return };
        let denom = (view_h - thumb_h).max(1.0);
        self.scroll = ((y - grab) / denom * self.max_scroll(view_h)).clamp(0.0, self.max_scroll(view_h));
        self.clamp_scroll();
    }

    /// Whether zoom applies: images always, PDFs only when pages rendered
    /// (the text-extraction fallback has nothing to magnify).
    pub fn zoomable(&self) -> bool {
        matches!(self.kind, ViewKind::Image) || self.worker.is_some()
    }

    /// Multiplies the zoom, keeping the viewport center roughly anchored.
    pub fn zoom_by(&mut self, font_system: &mut FontSystem, factor: f32) {
        if !self.zoomable() {
            return;
        }
        let next = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        if (next - self.zoom).abs() < 1e-3 {
            return;
        }
        let ratio = next / self.zoom;
        self.zoom = next;
        self.reflow(font_system);
        self.scroll *= ratio;
        self.scroll_x = (self.scroll_x + self.width_px / 2.0) * ratio - self.width_px / 2.0;
        self.clamp_scroll();
    }

    pub fn zoom_reset(&mut self, font_system: &mut FontSystem) {
        if !self.zoomable() || (self.zoom - 1.0).abs() < 1e-3 {
            return;
        }
        let ratio = 1.0 / self.zoom;
        self.zoom = 1.0;
        self.scroll_x = 0.0;
        self.reflow(font_system);
        self.scroll *= ratio;
        self.clamp_scroll();
    }

    /// Collects finished page bitmaps from the worker, requests the pages at
    /// (and adjacent to) the viewport at the current width/zoom, and drops
    /// far-away cached pages to bound memory. Never blocks: a page draws as a
    /// placeholder until its bitmap arrives and the worker triggers a repaint.
    fn prepare_pages(&mut self, view_h: f32) {
        let Some(worker) = self.worker.as_ref() else { return };
        for (index, width, img) in worker.res_rx.try_iter() {
            if self.pending.get(&index) == Some(&width) {
                self.pending.remove(&index);
            }
            self.page_cache.insert(index, img);
        }
        let target_w = self.text_width() * self.zoom;
        let mut y = self.margin - self.scroll;
        // (page index, size, intersects viewport) in document order.
        let mut pages: Vec<(usize, (f32, f32), bool)> = Vec::new();
        for block in &self.blocks {
            let advance = match block {
                Block::Text { height, .. }
                | Block::Picture { height, .. }
                | Block::Table { height, .. } => *height,
                Block::Rule => self.font_px,
                Block::Page { index, size, height } => {
                    pages.push((*index, *size, y + *height >= 0.0 && y <= view_h));
                    *height
                }
            };
            y += advance + self.spacing;
        }
        let (Some(first), Some(last)) = (
            pages.iter().position(|&(_, _, v)| v),
            pages.iter().rposition(|&(_, _, v)| v),
        ) else {
            return;
        };
        // Visible pages first, then one prefetch neighbor on each side so the
        // next page is usually ready before it scrolls in.
        let lo = first.saturating_sub(1);
        let hi = (last + 1).min(pages.len() - 1);
        let keep: HashSet<usize> = pages[lo..=hi].iter().map(|&(i, _, _)| i).collect();
        // Publish before requesting so the worker never skips a live request.
        if let Ok(mut wanted) = worker.wanted.lock() {
            *wanted = keep.clone();
        }
        let order = (first..=last).chain((lo..first).chain(last + 1..=hi));
        for slot in order {
            let (index, size, _) = pages[slot];
            let want = fit_draw(size.0, size.1, target_w).0 as u32;
            if self.page_cache.get(&index).is_some_and(|img| img.width() == want)
                || self.pending.get(&index) == Some(&want)
            {
                continue;
            }
            if worker.req_tx.send((index, want)).is_ok() {
                self.pending.insert(index, want);
            }
        }
        // Keep only the requested window cached.
        self.page_cache.retain(|k, _| keep.contains(k));
        self.pending.retain(|k, _| keep.contains(k));
    }

    /// Collects finished bitmaps from the image worker and requests scaled
    /// copies for the visible pictures whose bitmap does not match the
    /// current draw size. Never blocks; pictures show a placeholder or a
    /// nearest-scaled preview until the proper bitmap arrives.
    fn prepare_images(&mut self, view_h: f32) {
        let Some(worker) = self.img_worker.as_ref() else { return };
        let mut done: HashMap<usize, RgbaImage> = HashMap::new();
        for (slot, img) in worker.res_rx.try_iter() {
            self.pending_imgs.remove(&slot);
            done.insert(slot, img);
        }
        let text_w = self.text_width();
        let zoom = self.zoom;
        let mut y = self.margin - self.scroll;
        for (idx, block) in self.blocks.iter_mut().enumerate() {
            let advance = match block {
                Block::Text { height, .. } | Block::Table { height, .. } => *height,
                Block::Rule => self.font_px,
                Block::Page { height, .. } => *height,
                Block::Picture { scaled, size, height } => {
                    if let Some(img) = done.remove(&idx) {
                        *scaled = img;
                    }
                    let (dw, dh) = fit_draw(size.0, size.1, size.0.min(text_w) * zoom);
                    let want = (dw as u32, dh as u32);
                    let visible = y + *height >= 0.0 && y <= view_h;
                    if visible
                        && (scaled.width(), scaled.height()) != want
                        && self.pending_imgs.get(&idx) != Some(&want)
                        && worker.req_tx.send((idx, want.0, want.1)).is_ok()
                    {
                        self.pending_imgs.insert(idx, want);
                    }
                    *height
                }
            };
            y += advance + self.spacing;
        }
    }

    pub fn render(
        &mut self,
        font_system: &mut FontSystem,
        swash_cache: &mut SwashCache,
        width_px: u32,
        height_px: u32,
    ) -> SharedPixelBuffer<Rgba8Pixel> {
        self.prepare_pages(height_px as f32);
        self.prepare_images(height_px as f32);
        let mut frame = SharedPixelBuffer::<Rgba8Pixel>::new(width_px.max(1), height_px.max(1));
        let bg = colors::base_palette(self.theme)[0];
        let (w, h) = (frame.width() as i32, frame.height() as i32);
        let mut canvas = Canvas { pixels: frame.make_mut_slice(), width: w, height: h };
        canvas.fill(bg);

        let mut y = self.margin - self.scroll;
        let margin = self.margin;
        let scroll_x = self.scroll_x;
        let text_w = self.text_width();
        let fg = self.fg();
        let default_color = Color::rgb(fg[0], fg[1], fg[2]);
        let page_cache = &self.page_cache;
        for block in self.blocks.iter_mut() {
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
                Block::Picture { scaled, size, height } => {
                    if y + *height >= 0.0 && y <= h as f32 {
                        let x0 = (margin - scroll_x) as i32;
                        let draw_w = fit_draw(size.0, size.1, size.0.min(text_w) * self.zoom).0 as i32;
                        if scaled.width() == draw_w as u32 && scaled.height() == *height as u32 {
                            blit_image(&mut canvas, scaled, x0, y as i32);
                        } else if scaled.width() > 0 {
                            // Decode/rescale still in flight: nearest preview.
                            blit_scaled(&mut canvas, scaled, x0, y as i32, draw_w, *height as i32);
                        } else {
                            canvas.fill_rect(x0, y as i32, draw_w, *height as i32, self.theme.ui.panel_hover);
                        }
                    }
                    y += *height + self.spacing;
                }
                Block::Page { index, size, height } => {
                    if y + *height >= 0.0 && y <= h as f32 {
                        let x0 = (margin - scroll_x) as i32;
                        let dim = colors::base_palette(self.theme)[8];
                        let draw_w = fit_draw(size.0, size.1, text_w * self.zoom).0 as i32;
                        match page_cache.get(index) {
                            Some(img) if img.width() == draw_w as u32 => {
                                blit_opaque(&mut canvas, img, x0, y as i32)
                            }
                            // A re-render at the new width is still in
                            // flight: show the old bitmap rescaled meanwhile.
                            Some(img) => {
                                blit_scaled(&mut canvas, img, x0, y as i32, draw_w, *height as i32)
                            }
                            // Not rasterized yet: draw a blank sheet so the
                            // layout doesn't jump.
                            None => canvas.fill_rect(x0, y as i32, draw_w, *height as i32, [255, 255, 255]),
                        }
                        // Hairline frame so white pages read as pages on
                        // light backgrounds too.
                        let ph = *height as i32;
                        canvas.fill_rect(x0 - 1, y as i32 - 1, draw_w + 2, 1, dim);
                        canvas.fill_rect(x0 - 1, y as i32 + ph, draw_w + 2, 1, dim);
                        canvas.fill_rect(x0 - 1, y as i32, 1, ph, dim);
                        canvas.fill_rect(x0 + draw_w, y as i32, 1, ph, dim);
                    }
                    y += *height + self.spacing;
                }
                Block::Rule => {
                    let ry = (y + self.font_px / 2.0) as i32;
                    let dim = colors::base_palette(self.theme)[8];
                    canvas.fill_rect(margin as i32, ry, text_w as i32, 1, dim);
                    y += self.font_px + self.spacing;
                }
                Block::Table { rows, font_px, col_widths, row_heights, height } => {
                    if y + *height >= 0.0 && y <= h as f32 && !col_widths.is_empty() {
                        let dim = colors::base_palette(self.theme)[8];
                        let head_bg = self.theme.ui.panel_hover;
                        let pad_h = (*font_px * 0.5).round();
                        let pad_v = (*font_px * 0.3).round();
                        let table_w = (col_widths.iter().map(|w| w + 2.0 * pad_h).sum::<f32>()
                            + (col_widths.len() + 1) as f32) as i32;
                        canvas.fill_rect(margin as i32, y as i32, table_w, 1, dim);
                        let mut row_y = y + 1.0;
                        for (ri, row) in rows.iter_mut().enumerate() {
                            let row_h = row_heights[ri];
                            if ri == 0 {
                                canvas.fill_rect(
                                    margin as i32 + 1,
                                    row_y as i32,
                                    table_w - 2,
                                    row_h as i32,
                                    head_bg,
                                );
                            }
                            let mut cell_x = margin + 1.0;
                            for (ci, buffer) in row.iter_mut().enumerate() {
                                let ox = (cell_x + pad_h) as i32;
                                let oy = (row_y + pad_v) as i32;
                                buffer.draw(
                                    font_system,
                                    swash_cache,
                                    default_color,
                                    |px, py, pw, ph, color| {
                                        canvas.blend_rect(ox + px, oy + py, pw as i32, ph as i32, color);
                                    },
                                );
                                cell_x += col_widths[ci] + 2.0 * pad_h + 1.0;
                            }
                            row_y += row_h;
                            canvas.fill_rect(margin as i32, row_y as i32, table_w, 1, dim);
                            row_y += 1.0;
                        }
                        let mut line_x = margin;
                        canvas.fill_rect(line_x as i32, y as i32, 1, *height as i32, dim);
                        for w in col_widths.iter() {
                            line_x += w + 2.0 * pad_h + 1.0;
                            canvas.fill_rect(line_x as i32, y as i32, 1, *height as i32, dim);
                        }
                    }
                    y += *height + self.spacing;
                }
            }
        }
        // Scrollbar along the right edge whenever the content overflows.
        if let Some((bar_x, thumb_y, thumb_h)) = self.scrollbar(h as f32) {
            let track = self.theme.ui.panel_hover;
            let thumb = colors::base_palette(self.theme)[8];
            canvas.fill_rect(bar_x as i32, 0, SCROLLBAR_W as i32, h, track);
            canvas.fill_rect(
                bar_x as i32 + 2,
                thumb_y as i32,
                SCROLLBAR_W as i32 - 4,
                thumb_h as i32,
                thumb,
            );
        }
        frame
    }
}

/// Renders one PDF page to a bitmap `draw_w` pixels wide. The white opaque
/// background makes hayro's premultiplied output plain RGBA.
fn rasterize_page<'a>(
    pages: &'a [hayro::hayro_syntax::page::Page<'a>],
    index: usize,
    draw_w: u32,
    cache: &hayro::RenderCache<'a>,
) -> Option<RgbaImage> {
    let page = pages.get(index)?;
    let (page_w, _) = page.render_dimensions();
    let scale = draw_w as f32 / page_w.max(1.0);
    let settings = hayro::RenderSettings {
        x_scale: scale,
        y_scale: scale,
        width: None,
        height: None,
        bg_color: hayro::vello_cpu::color::palette::css::WHITE,
    };
    let pixmap = hayro::render(
        page,
        cache,
        &hayro::hayro_interpret::InterpreterSettings::default(),
        &settings,
    );
    RgbaImage::from_raw(
        pixmap.width() as u32,
        pixmap.height() as u32,
        pixmap.data_as_u8_slice().to_vec(),
    )
}

/// Clips an `iw` x `ih` bitmap placed at (`x0`, `y0`) against the canvas;
/// returns the covered source range as (sx0, sx1, sy0, sy1), empty if none.
fn clip_blit(canvas: &Canvas, iw: i32, ih: i32, x0: i32, y0: i32) -> (i32, i32, i32, i32) {
    (
        (-x0).max(0),
        (canvas.width - x0).min(iw),
        (-y0).max(0),
        (canvas.height - y0).min(ih),
    )
}

/// Fast path for opaque bitmaps (PDF pages render onto an opaque white
/// background): rows are copied without per-pixel bounds checks or alpha math.
fn blit_opaque(canvas: &mut Canvas, img: &RgbaImage, x0: i32, y0: i32) {
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    let (sx0, sx1, sy0, sy1) = clip_blit(canvas, iw, ih, x0, y0);
    if sx0 >= sx1 || sy0 >= sy1 {
        return;
    }
    let raw = img.as_raw();
    let n = (sx1 - sx0) as usize;
    for sy in sy0..sy1 {
        let dst0 = ((y0 + sy) * canvas.width + x0 + sx0) as usize;
        let src0 = ((sy * iw + sx0) as usize) * 4;
        let dst = &mut canvas.pixels[dst0..dst0 + n];
        let src = &raw[src0..src0 + n * 4];
        for (d, s) in dst.iter_mut().zip(src.chunks_exact(4)) {
            *d = Rgba8Pixel { r: s[0], g: s[1], b: s[2], a: 255 };
        }
    }
}

fn blit_image(canvas: &mut Canvas, img: &RgbaImage, x0: i32, y0: i32) {
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    let (sx0, sx1, sy0, sy1) = clip_blit(canvas, iw, ih, x0, y0);
    if sx0 >= sx1 || sy0 >= sy1 {
        return;
    }
    let raw = img.as_raw();
    let n = (sx1 - sx0) as usize;
    for sy in sy0..sy1 {
        let dst0 = ((y0 + sy) * canvas.width + x0 + sx0) as usize;
        let src0 = ((sy * iw + sx0) as usize) * 4;
        let dst = &mut canvas.pixels[dst0..dst0 + n];
        let src = &raw[src0..src0 + n * 4];
        for (d, s) in dst.iter_mut().zip(src.chunks_exact(4)) {
            match s[3] {
                0 => {}
                255 => *d = Rgba8Pixel { r: s[0], g: s[1], b: s[2], a: 255 },
                a => {
                    let (a, inv) = (a as u32, 255 - a as u32);
                    *d = Rgba8Pixel {
                        r: ((s[0] as u32 * a + d.r as u32 * inv) / 255) as u8,
                        g: ((s[1] as u32 * a + d.g as u32 * inv) / 255) as u8,
                        b: ((s[2] as u32 * a + d.b as u32 * inv) / 255) as u8,
                        a: 255,
                    };
                }
            }
        }
    }
}

/// Nearest-neighbor draw of `img` into a `dw` x `dh` rectangle. Only used as
/// a transient while a zoomed page waits for its re-rasterized bitmap.
fn blit_scaled(canvas: &mut Canvas, img: &RgbaImage, x0: i32, y0: i32, dw: i32, dh: i32) {
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    if dw <= 0 || dh <= 0 || iw <= 0 || ih <= 0 {
        return;
    }
    let (dx0, dx1, dy0, dy1) = clip_blit(canvas, dw, dh, x0, y0);
    if dx0 >= dx1 || dy0 >= dy1 {
        return;
    }
    let raw = img.as_raw();
    // 16.16 fixed-point source stepping: no divides in the pixel loop.
    let step_x = ((iw as u64) << 16) / dw as u64;
    let step_y = ((ih as u64) << 16) / dh as u64;
    for dy in dy0..dy1 {
        let sy = (((dy as u64 * step_y) >> 16) as i32).min(ih - 1);
        let row = &raw[(sy * iw) as usize * 4..(sy * iw + iw) as usize * 4];
        let dst0 = ((y0 + dy) * canvas.width + x0 + dx0) as usize;
        let dst = &mut canvas.pixels[dst0..dst0 + (dx1 - dx0) as usize];
        let mut sx_fp = dx0 as u64 * step_x;
        for d in dst.iter_mut() {
            let sx = ((sx_fp >> 16) as i32).min(iw - 1) as usize;
            sx_fp += step_x;
            let s = &row[sx * 4..sx * 4 + 4];
            match s[3] {
                0 => {}
                255 => *d = Rgba8Pixel { r: s[0], g: s[1], b: s[2], a: 255 },
                a => {
                    let (a, inv) = (a as u32, 255 - a as u32);
                    *d = Rgba8Pixel {
                        r: ((s[0] as u32 * a + d.r as u32 * inv) / 255) as u8,
                        g: ((s[1] as u32 * a + d.g as u32 * inv) / 255) as u8,
                        b: ((s[2] as u32 * a + d.b as u32 * inv) / 255) as u8,
                        a: 255,
                    };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal one-page PDF (200x100pt, "Hello PDF" in Helvetica)
    /// with a correct xref table so strict parsing paths work too.
    fn hello_pdf() -> Vec<u8> {
        let objects = [
            "<</Type/Catalog/Pages 2 0 R>>".to_string(),
            "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>".to_string(),
            {
                let stream = "BT /F1 24 Tf 20 40 Td (Hello PDF) Tj ET";
                format!("<</Length {}>>stream\n{stream}\nendstream", stream.len() + 1)
            },
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_string(),
        ];
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
        }
        let xref = out.len();
        out.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
        for off in &offsets {
            out.push_str(&format!("{off:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    #[test]
    fn image_arrives_from_the_worker() {
        let path = std::env::temp_dir().join("tigriden-viewer-test.png");
        let mut img = RgbaImage::new(8, 8);
        for p in img.pixels_mut() {
            p.0 = [255, 0, 0, 255];
        }
        img.save(&path).unwrap();

        let mut font_system = FontSystem::new();
        let theme = crate::theme::default_theme();
        let mut viewer = ViewerState::open(
            &mut font_system,
            &path,
            ViewKind::Image,
            "Menlo",
            13.0,
            theme,
            [0, 0, 0],
            400.0,
            std::sync::Arc::new(|| {}),
        )
        .expect("viewer opens the image");

        // The bitmap decodes on the worker thread; poll until the red pixels
        // replace the placeholder.
        let mut swash_cache = SwashCache::new();
        let m = viewer.margin as usize;
        let mut found = false;
        for _ in 0..200 {
            let frame = viewer.render(&mut font_system, &mut swash_cache, 400, 300);
            let px = frame.as_slice()[m * 400 + m];
            if px.r > 200 && px.g < 60 && px.b < 60 {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(found, "decoded image should arrive from the worker and draw");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pdf_renders_pages_and_zooms() {
        let path = std::env::temp_dir().join("tigriden-viewer-test.pdf");
        std::fs::write(&path, hello_pdf()).unwrap();

        let mut font_system = FontSystem::new();
        let theme = crate::theme::default_theme();
        let mut viewer = ViewerState::open(
            &mut font_system,
            &path,
            ViewKind::Pdf,
            "Menlo",
            13.0,
            theme,
            [0, 0, 0],
            800.0,
            std::sync::Arc::new(|| {}),
        )
        .expect("viewer opens the pdf");
        assert!(viewer.worker.is_some(), "hayro should parse the PDF, not fall back to text");
        assert_eq!(viewer.blocks.len(), 1, "one page, one block");

        let mut swash_cache = SwashCache::new();
        // The page (2:1 aspect, fit to width) must show as a mostly white
        // sheet with dark glyph pixels on it. The bitmap comes from the
        // worker thread, so poll until it lands.
        let margin = viewer.margin as usize;
        let page_w = (800.0 - 2.0 * viewer.margin) as usize;
        let page_h = page_w / 2;
        let (mut white, mut dark) = (0usize, 0usize);
        for _ in 0..200 {
            let frame = viewer.render(&mut font_system, &mut swash_cache, 800, 600);
            let pixels = frame.as_slice();
            (white, dark) = (0, 0);
            for y in margin..margin + page_h {
                for x in margin..margin + page_w {
                    let p = pixels[y * 800 + x];
                    if p.r > 240 && p.g > 240 && p.b > 240 {
                        white += 1;
                    } else if p.r < 96 && p.g < 96 && p.b < 96 {
                        dark += 1;
                    }
                }
            }
            if dark > 100 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(white > page_w * page_h / 2, "page should render as a white sheet, got {white}");
        assert!(dark > 100, "glyphs should render on the page, got {dark} dark pixels");

        // Zooming in widens the content and re-rasterizes; panning unlocks.
        let before = viewer.content_w;
        viewer.zoom_by(&mut font_system, 2.0);
        assert!(viewer.content_w > before, "zoom must widen the content");

        // The zoomed page overflows a 600px viewport: the scrollbar appears,
        // a press on its track jumps the scroll position, and PageDown /
        // Home-End style jumps move the viewport.
        assert!(viewer.scrollbar(600.0).is_some(), "overflowing content shows a scrollbar");
        assert!(viewer.handle_mouse(0, 795.0, 500.0, 600.0), "press on the track is consumed");
        assert!(viewer.scroll > 0.0, "track click scrolls the document");
        viewer.handle_mouse(1, 795.0, 500.0, 600.0);
        viewer.scroll_home(false);
        assert_eq!(viewer.scroll, 0.0, "Home returns to the top");
        viewer.scroll_page(600.0, true);
        assert!(viewer.scroll > 0.0, "PageDown moves down");
        viewer.scroll_home(false);
        viewer.scroll_by(-10_000.0, 0.0);
        assert!(viewer.scroll_x > 0.0, "zoomed content must pan horizontally");
        viewer.zoom_reset(&mut font_system);
        assert_eq!(viewer.scroll_x, 0.0, "reset returns to fit-to-width");

        let _ = std::fs::remove_file(&path);
    }
}

/// Classifies a path by extension into a viewer kind (None = open in editor).
pub fn classify(path: &Path) -> Option<ViewKind> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" => Some(ViewKind::Image),
        "md" | "markdown" => Some(ViewKind::Markdown),
        "csv" | "tsv" => Some(ViewKind::Csv),
        "pdf" => Some(ViewKind::Pdf),
        _ => None,
    }
}
