use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use cosmic_text::{Align, Attrs, Buffer, Color, Cursor, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight, Wrap};
use image::RgbaImage;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use slint::{Rgba8Pixel, SharedPixelBuffer};

use hayro::vello_cpu::kurbo;

use crate::mathlayout::{self, MathBox, MathItem};
use crate::paint::Canvas;
use crate::term::colors;
use crate::theme::ThemeDef;

#[derive(Clone, Copy, PartialEq)]
pub enum ViewKind {
    Image,
    Markdown,
    Csv,
    Pdf,
    Tex,
}

/// Zoom bounds for images and PDF pages (factor over the fit-to-width size).
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 8.0;
/// Rasterization caps: a zoomed rescale must never allocate unbounded pixels.
const MAX_DRAW_W: f32 = 4096.0;
const MAX_DRAW_H: f32 = 16384.0;
/// Colors for the paper sheet a LaTeX document is painted on, chosen to match
/// how this same viewer shows a real PDF page.
const PAPER_BG: [u8; 3] = [255, 255, 255];
const PAPER_INK: [u8; 3] = [26, 26, 29];
const PAPER_MUTED: [u8; 3] = [92, 92, 100];
const PAPER_LINK: [u8; 3] = [24, 78, 158];
const PAPER_RULE: [u8; 3] = [188, 188, 194];
const PAPER_CODE_BG: [u8; 3] = [243, 243, 240];

/// Smallest body type a typeset page is set at (device pixels). A pane too
/// narrow for that gets a page wider than itself, which pans sideways.
const MIN_BODY_PX: f32 = 11.0;

/// Gap between the page sheets of a paginated document, and the gutter of
/// app background kept either side of the sheet.
const PAGE_GAP: f32 = 18.0;
const PAGE_GUTTER: f32 = 14.0;

/// The geometry of one page of a typeset document, in device pixels: the
/// sheet, the margins printed around the text, and the column grid inside it.
/// Everything is derived from the paper size the class asks for, scaled so the
/// sheet fits the pane, which is what makes the view read like the PDF the
/// same source compiles to.
#[derive(Clone, Copy)]
struct PageGeom {
    w: f32,
    h: f32,
    margin_x: f32,
    margin_y: f32,
    columns: usize,
    /// Gap between columns of a two-column page.
    gutter: f32,
}

impl PageGeom {
    /// Width of one column of text.
    fn column_w(&self) -> f32 {
        let text_w = self.w - 2.0 * self.margin_x;
        let n = self.columns.max(1) as f32;
        ((text_w - (n - 1.0) * self.gutter) / n).max(48.0)
    }

    /// Height available for text on a page.
    fn column_h(&self) -> f32 {
        (self.h - 2.0 * self.margin_y).max(48.0)
    }

    /// Left edge of column `i`, relative to the sheet's left edge.
    fn column_x(&self, i: usize) -> f32 {
        self.margin_x + i as f32 * (self.column_w() + self.gutter)
    }
}

/// Where a block — or a slice of one, for a paragraph continued in the next
/// column — sits in the document: its top-left corner in content coordinates
/// (before scrolling), the width it was laid out for, and the vertical slice
/// of the block it draws.
#[derive(Clone, Copy)]
struct Frag {
    block: usize,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    /// Buffer-space top and bottom of the slice; `0.0` to `f32::MAX` is the
    /// whole block.
    from: f32,
    to: f32,
}

/// Scrollbar (physical px): track width and minimum thumb height.
const SCROLLBAR_W: f32 = 12.0;
const SCROLLBAR_MIN_THUMB: f32 = 32.0;

/// The width a figure draws at: an explicit `width=…\linewidth` fills that
/// fraction of the column, otherwise the image keeps its natural size, capped
/// to the column.
fn picture_target_w(size: (f32, f32), fill: Option<f32>, text_w: f32, zoom: f32) -> f32 {
    let base = match fill {
        Some(f) => text_w * f,
        None => size.0.min(text_w),
    };
    base * zoom
}

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

/// Splits a display formula from a trailing `\tag{…}`. Markdown has no
/// `equation` environment, so `$$…\tag{1}$$` is how a numbered equation is
/// written; pulling the tag out lets it be set flush right exactly as the
/// LaTeX view numbers `\begin{equation}`.
fn split_math_tag(body: &str) -> (String, Option<String>) {
    let Some(at) = body.rfind("\\tag") else { return (body.to_string(), None) };
    let rest = body[at + 4..].trim_start();
    let Some(inner) = rest.strip_prefix('{').and_then(|r| r.find('}').map(|e| &r[..e])) else {
        return (body.to_string(), None);
    };
    let number = inner.trim();
    if number.is_empty() {
        return (body.to_string(), None);
    }
    (body[..at].to_string(), Some(format!("({number})")))
}

enum Block {
    Text {
        buffer: Buffer,
        indent: f32,
        /// Space kept on the right, for the blocks TeX pulls in from both
        /// margins (an abstract, a quote).
        inset_r: f32,
        bg: Option<[u8; 3]>,
        height: f32,
    },
    /// An image; `size` is the original dimensions (layout comes from the
    /// header alone) and `scaled` the latest bitmap from the image worker —
    /// empty until the first one arrives.
    Picture {
        scaled: RgbaImage,
        size: (f32, f32),
        /// Requested width as a fraction of the column, from
        /// \includegraphics[width=…]; None means natural size.
        fill: Option<f32>,
        height: f32,
    },
    /// One PDF page, rasterized lazily when it scrolls into view.
    Page { index: usize, size: (f32, f32), height: f32 },
    Rule,
    /// A display equation laid out as boxes, with the number TeX would print
    /// at the right margin. `text` is never drawn: it holds the flattened
    /// form so Cmd+A / Cmd+C still capture the equation.
    Math { bx: MathBox, number: Option<MathBox>, text: Buffer, height: f32 },
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
    /// Selectable text by page index. Kept for every page that has scrolled
    /// into view — the boxes are small next to the bitmaps.
    page_text: HashMap<usize, PageText>,
    /// Text layers requested from the worker but not yet delivered.
    pending_text: HashSet<usize>,
    /// Selection on a page-rendered PDF as (page index, caret within that
    /// page's text). Pages and flowed text never share a document, so this is
    /// independent of `sel_anchor` / `sel_head`.
    page_sel_anchor: Option<(usize, usize)>,
    page_sel_head: Option<(usize, usize)>,
    /// Background decoder/rescaler owning the original image bitmaps.
    img_worker: Option<ImgWorker>,
    /// Sizes requested from the image worker but not yet delivered, by block.
    pending_imgs: HashMap<usize, (u32, u32)>,
    /// (block index, path) of images found during build; consumed by `open`
    /// to spawn the image worker.
    img_paths: Vec<(usize, PathBuf)>,
    /// Some while the scrollbar thumb is dragged: grab offset within the thumb.
    drag: Option<f32>,
    /// Text selection endpoints as (block index, text cursor): `sel_anchor`
    /// is where the drag started, `sel_head` follows the pointer.
    sel_anchor: Option<(usize, Cursor)>,
    sel_head: Option<(usize, Cursor)>,
    /// True while the left button is dragging out a text selection.
    selecting: bool,
    /// Extracted text of a page-rendered PDF, filled lazily on first copy
    /// (the page bitmaps carry no selectable glyphs).
    pdf_text: Option<String>,
    /// Set for LaTeX documents, which are painted on a white sheet with dark
    /// ink the way a PDF of the same source would look.
    paper: bool,
    /// Page grid for a paginated document (LaTeX): blocks are packed into
    /// its columns and the sheets are painted under them. None for the
    /// continuously flowing views (Markdown, CSV, images, PDF pages).
    page_geom: Option<PageGeom>,
    /// Every laid-out fragment, in document order; a paragraph split across
    /// columns contributes one fragment per column.
    places: Vec<Frag>,
    /// Space to keep above each block, indexed like `blocks`; missing
    /// entries fall back to the view's uniform `spacing`.
    gaps: Vec<f32>,
    /// Blocks that run across every column instead of sitting in one: the
    /// title block of a two-column paper. Indexed like `blocks`.
    spanning: Vec<bool>,
    /// Blocks the page builder may hold over to the top of a later column,
    /// as TeX floats a figure or table that will not fit. Indexed like
    /// `blocks`.
    floating: Vec<bool>,
    /// Blocks that must not be left at the bottom of a column without the
    /// block that follows them (headings, captions above a figure). Indexed
    /// like `blocks`; missing entries mean "may break after".
    keep_next: Vec<bool>,
    /// Sheets of a paginated document as (top, height) in content
    /// coordinates; empty for the flowing views.
    sheets: Vec<(f32, f32)>,
    margin: f32,
    spacing: f32,
    font_px: f32,
    /// Line height as a multiple of the type size, for flowed text.
    leading: f32,
    theme: &'static ThemeDef,
    /// Effective accent (theme default or the user's override).
    accent: [u8; 3],
    font_family: &'static str,
}

/// Called from worker threads whenever a bitmap is ready; the app uses it to
/// schedule a repaint on the UI event loop.
pub type Notify = std::sync::Arc<dyn Fn() + Send + Sync + 'static>;

/// One character extracted from a PDF page, boxed in page points with the
/// origin at the page's top-left corner.
struct PageChar {
    text: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    /// Baseline y, used only while grouping characters into lines.
    baseline: f32,
    /// Reading-order line, which decides where copied text breaks.
    line: u32,
}

/// A page's selectable text in reading order: lines top to bottom, characters
/// left to right within each line. Positions come from the same content
/// stream the renderer draws, so the boxes line up with the visible glyphs.
#[derive(Default)]
struct PageText {
    chars: Vec<PageChar>,
}

impl PageText {
    /// Sorts freshly collected characters into reading order and assigns each
    /// one a line, so a selection range is a plain slice of the vector.
    ///
    /// Order is column-major: a two-column paper reads down its left column
    /// before its right, rather than zig-zagging across the gutter row by row
    /// the way a naive baseline sort would.
    fn from_chars(mut chars: Vec<PageChar>, page_w: f32) -> Self {
        if chars.is_empty() {
            return Self::default();
        }
        let cmp = |a: f32, b: f32| a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal);
        chars.sort_by(|a, b| cmp(a.baseline, b.baseline).then(cmp(a.x, b.x)));
        // Characters whose baselines sit within half a line height belong to
        // the same visual row; superscripts and mixed font sizes shift the
        // baseline slightly without starting a new row.
        let mut heights: Vec<f32> = chars.iter().map(|c| c.h).collect();
        heights.sort_by(|a, b| cmp(*a, *b));
        let tolerance = heights[heights.len() / 2] * 0.5;
        let (mut row, mut row_baseline) = (0u32, chars[0].baseline);
        for c in chars.iter_mut() {
            if (c.baseline - row_baseline).abs() > tolerance {
                row += 1;
                row_baseline = c.baseline;
            }
            c.line = row;
        }
        // Break each row into runs at wide horizontal gaps, so body text and
        // a figure label that happen to share a baseline stay apart.
        let mut runs: Vec<(f32, f32)> = Vec::new();
        let mut run_of: Vec<usize> = Vec::with_capacity(chars.len());
        for (i, c) in chars.iter().enumerate() {
            let split = match (i.checked_sub(1).map(|p| &chars[p]), runs.last()) {
                (Some(prev), Some(run)) => prev.line != c.line || c.x - run.1 > c.h * 0.6,
                _ => true,
            };
            if split {
                runs.push((c.x, c.x + c.w));
            } else if let Some(run) = runs.last_mut() {
                run.1 = run.1.max(c.x + c.w);
            }
            run_of.push(runs.len() - 1);
        }
        let gutters = column_gutters(&runs, page_w, row as usize + 1);
        // Sort by column, then row, then position along the row.
        // Each character picks its own column from where its middle falls, so
        // a row whose columns nearly touch — too close for the run split to
        // catch — is still divided at the gutter.
        let mut keyed: Vec<((u32, u32, f32), PageChar)> = chars
            .into_iter()
            .map(|c| {
                let middle = c.x + c.w / 2.0;
                let column = gutters.iter().filter(|g| middle >= **g).count() as u32;
                ((column, c.line, c.x), c)
            })
            .collect();
        keyed.sort_by(|a, b| {
            a.0 .0.cmp(&b.0 .0).then(a.0 .1.cmp(&b.0 .1)).then(cmp(a.0 .2, b.0 .2))
        });
        // Renumber so consecutive lines stay consecutive after the reorder;
        // copied text breaks wherever this number changes.
        let mut chars = Vec::with_capacity(keyed.len());
        let mut line = 0u32;
        let mut prev: Option<(u32, u32)> = None;
        for ((column, row, _), mut c) in keyed {
            if prev.is_some_and(|p| p != (column, row)) {
                line += 1;
            }
            prev = Some((column, row));
            c.line = line;
            chars.push(c);
        }
        Self { chars }
    }

    /// Caret position (0..=len) nearest a page-space point. The row is chosen
    /// first so dragging sideways stays on the line under the pointer.
    fn caret_at(&self, px: f32, py: f32) -> usize {
        let mut line = 0u32;
        let mut best = f32::MAX;
        for c in &self.chars {
            let d = if py < c.y {
                c.y - py
            } else if py > c.y + c.h {
                py - c.y - c.h
            } else {
                0.0
            };
            if d < best {
                (best, line) = (d, c.line);
            }
        }
        let mut caret = 0;
        for (i, c) in self.chars.iter().enumerate() {
            if c.line != line {
                continue;
            }
            if px < c.x + c.w / 2.0 {
                return i;
            }
            caret = i + 1;
        }
        caret
    }
}

/// Finds the x positions of a page's column gutters: interior vertical bands
/// wide enough to separate columns that no text run crosses. Returns nothing
/// for single-column pages and for pages too sparse to judge, which leaves
/// their characters in plain top-to-bottom order.
fn column_gutters(runs: &[(f32, f32)], page_w: f32, rows: usize) -> Vec<f32> {
    const BUCKETS: usize = 200;
    // Below a handful of rows, white space is just layout, not a gutter.
    if rows < 8 || page_w <= 0.0 {
        return Vec::new();
    }
    let mut hits = [0u32; BUCKETS];
    let bucket = |x: f32| ((x / page_w) * BUCKETS as f32).clamp(0.0, BUCKETS as f32);
    for &(x0, x1) in runs {
        let (a, b) = (bucket(x0).floor() as usize, (bucket(x1).ceil() as usize).min(BUCKETS));
        for c in hits.iter_mut().take(b).skip(a) {
            *c += 1;
        }
    }
    // Runs never overlap within a row, so a bucket's count is the number of
    // rows reaching it. A gutter has to stay clear down most of the page, but
    // not all of it: a full-width header or a straddling figure is normal.
    let noise = (rows as u32 * 15 / 100).max(1);
    let covered: Vec<bool> = hits.iter().map(|h| *h > noise).collect();
    // Only gaps between text count; the page margins are not gutters.
    let (Some(first), Some(last)) =
        (covered.iter().position(|c| *c), covered.iter().rposition(|c| *c))
    else {
        return Vec::new();
    };
    // Justified columns run nearly flush to the gutter, so the clear band can
    // be only a few points wide; having to stay clear down most of the page is
    // what separates it from ordinary word spacing.
    let min_width = 2;
    let mut gutters = Vec::new();
    let mut gap_start = None;
    for i in first..=last {
        if !covered[i] {
            gap_start.get_or_insert(i);
        } else if let Some(start) = gap_start.take() {
            if i - start >= min_width {
                gutters.push((start + i) as f32 / 2.0 / BUCKETS as f32 * page_w);
            }
        }
    }
    gutters
}

/// Work for the PDF thread: rasterize a page at a width, or extract the
/// selectable text of one.
enum PdfReq {
    Render(usize, u32),
    Text(usize),
}

/// A finished piece of that work.
enum PdfRes {
    Page(usize, u32, RgbaImage),
    Text(usize, PageText),
}

/// Files one worker request into the render or text queue. Renders coalesce
/// per page so the latest width wins and a zoom gesture collapses to its final
/// size instead of rasterizing every step along the way.
fn queue_req(
    req: PdfReq,
    order: &mut std::collections::VecDeque<usize>,
    want: &mut HashMap<usize, u32>,
    texts: &mut std::collections::VecDeque<usize>,
) {
    match req {
        PdfReq::Render(index, width) => {
            if want.insert(index, width).is_none() {
                order.push_back(index);
            }
        }
        PdfReq::Text(index) => texts.push_back(index),
    }
}

/// Handle to a per-document rasterizer thread. Dropping it hangs up the
/// request channel, which shuts the thread down.
struct PdfWorker {
    req_tx: Sender<PdfReq>,
    res_rx: Receiver<PdfRes>,
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
    let (req_tx, req_rx) = mpsc::channel::<PdfReq>();
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
        let text_cache = hayro::hayro_interpret::InterpreterCache::new();
        while let Ok(first) = req_rx.recv() {
            let mut order = std::collections::VecDeque::new();
            let mut want: HashMap<usize, u32> = HashMap::new();
            let mut texts = std::collections::VecDeque::new();
            queue_req(first, &mut order, &mut want, &mut texts);
            loop {
                for req in req_rx.try_iter() {
                    queue_req(req, &mut order, &mut want, &mut texts);
                }
                // Pixels first: a page the reader is scrolling towards matters
                // more than a text layer nobody has dragged across yet.
                if let Some(index) = order.pop_front() {
                    let Some(width) = want.remove(&index) else { continue };
                    if !wanted_worker.lock().is_ok_and(|w| w.contains(&index)) {
                        continue;
                    }
                    if let Some(img) = rasterize_page(pages, index, width, &cache) {
                        if res_tx.send(PdfRes::Page(index, width, img)).is_err() {
                            return;
                        }
                        notify();
                    }
                    continue;
                }
                let Some(index) = texts.pop_front() else { break };
                let text = extract_page_text(&pdf, index, &text_cache).unwrap_or_default();
                if res_tx.send(PdfRes::Text(index, text)).is_err() {
                    return;
                }
                notify();
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
            page_text: HashMap::new(),
            pending_text: HashSet::new(),
            page_sel_anchor: None,
            page_sel_head: None,
            img_worker: None,
            pending_imgs: HashMap::new(),
            img_paths: Vec::new(),
            drag: None,
            sel_anchor: None,
            sel_head: None,
            selecting: false,
            pdf_text: None,
            paper: false,
            page_geom: None,
            places: Vec::new(),
            keep_next: Vec::new(),
            gaps: Vec::new(),
            spanning: Vec::new(),
            floating: Vec::new(),
            sheets: Vec::new(),
            margin: (font_px * 1.2).round(),
            spacing: (font_px * 0.5).round(),
            font_px,
            leading: 1.5,
            theme,
            accent,
            font_family,
        };
        match kind {
            ViewKind::Image => viewer.build_image(path)?,
            ViewKind::Markdown => viewer.build_markdown(font_system, path)?,
            ViewKind::Csv => viewer.build_csv(font_system, path)?,
            ViewKind::Pdf => viewer.build_pdf(font_system, path, notify.clone())?,
            ViewKind::Tex => viewer.build_tex(font_system, path)?,
        }
        if !viewer.img_paths.is_empty() {
            viewer.img_worker = Some(spawn_img_worker(std::mem::take(&mut viewer.img_paths), notify));
        }
        viewer.reflow(font_system);
        Ok(viewer)
    }

    fn fg(&self) -> [u8; 3] {
        if self.paper {
            return PAPER_INK;
        }
        colors::base_palette(self.theme)[7]
    }

    fn text_color(&self) -> Color {
        let c = self.fg();
        Color::rgb(c[0], c[1], c[2])
    }

    fn text_width(&self) -> f32 {
        (self.width_px - 2.0 * self.margin).max(48.0)
    }

    fn push_plain(&mut self, font_system: &mut FontSystem, text: &str, attrs: Attrs<'static>, wrap: Wrap) {
        let mut buffer =
            Buffer::new(font_system, Metrics::new(self.font_px, (self.font_px * 1.45).round()));
        buffer.set_wrap(wrap);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        self.blocks.push(Block::Text {
            buffer,
            indent: 0.0,
            inset_r: 0.0,
            bg: None,
            height: 0.0,
        });
    }

    fn push_table(
        &mut self,
        font_system: &mut FontSystem,
        rows: Vec<Vec<Vec<(String, Attrs<'static>)>>>,
    ) {
        // Slightly smaller than body text so wide tables fit more per column.
        let px = (self.font_px * 0.9).round().max(8.0);
        self.push_table_sized(font_system, rows, px);
    }

    /// Lays out a table at an explicit type size.
    fn push_table_sized(
        &mut self,
        font_system: &mut FontSystem,
        rows: Vec<Vec<Vec<(String, Attrs<'static>)>>>,
        px: f32,
    ) {
        if rows.iter().all(|row| row.iter().all(|cell| cell.iter().all(|(t, _)| t.trim().is_empty()))) {
            return;
        }
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
    fn push_image_file(&mut self, path: &Path, fill: Option<f32>) -> bool {
        match image::image_dimensions(path) {
            Ok((w, h)) => {
                self.blocks.push(Block::Picture {
                    scaled: RgbaImage::new(0, 0),
                    size: (w as f32, h as f32),
                    fill,
                    height: 0.0,
                });
                self.img_paths.push((self.blocks.len() - 1, path.to_path_buf()));
                true
            }
            Err(_) => false,
        }
    }

    fn build_image(&mut self, path: &Path) -> Result<(), String> {
        if !self.push_image_file(path, None) {
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

        // The same sheet the LaTeX view paints on: a Markdown paper should
        // read the way its .tex twin does, white page and all.
        self.paper = true;
        self.margin = (self.font_px * 2.8).round();
        self.spacing = (self.font_px * 0.62).round();

        let accent = PAPER_LINK;
        let code_bg = PAPER_CODE_BG;
        let serif = mathlayout::serif_family(font_system);

        let mut spans: Vec<(String, Attrs<'static>)> = Vec::new();
        let mut bold = 0usize;
        let mut italic = 0usize;
        let mut link = 0usize;
        let mut heading: Option<f32> = None;
        let mut in_code_block = false;
        let mut code_math = false;
        let mut code_text = String::new();
        let mut list_stack: Vec<Option<u64>> = Vec::new();
        let mut quote_depth = 0usize;
        // rows -> cells -> styled spans; Some while inside a table.
        let mut table: Option<Vec<Vec<Vec<(String, Attrs<'static>)>>>> = None;
        let mut table_row: Vec<Vec<(String, Attrs<'static>)>> = Vec::new();

        macro_rules! flush {
            ($self:ident, $spans:ident, $heading:ident, $list_stack:ident, $quote_depth:ident) => {
                flush!($self, $spans, $heading, $list_stack, $quote_depth, None)
            };
            ($self:ident, $spans:ident, $heading:ident, $list_stack:ident, $quote_depth:ident,
             $align:expr) => {{
                let base = $heading.unwrap_or($self.font_px);
                let indent = ($list_stack.len() as f32 * 1.5 + $quote_depth as f32 * 1.5)
                    * $self.font_px;
                let taken = std::mem::take(&mut $spans);
                $self.push_aligned(font_system, &taken, base, indent, $align);
            }};
        }

        let fg = self.text_color();
        let attrs_for = |bold: usize, italic: usize, link: usize, code: bool, fam: &'static str| {
            let mut attrs = if code { mono(fam) } else { Attrs::new().family(serif) };
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

        // ENABLE_MATH hands us `$…$` / `$$…$$` bodies verbatim. Without it the
        // CommonMark parser gets there first and eats `\{` as an escape and
        // `a_i…b_j` as emphasis, so the formula never survives to be typeset.
        let parser = Parser::new_ext(
            &source,
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_MATH,
        );
        // Math is set in the same serif face the LaTeX view uses, so a formula
        // reads the same whichever file it came from.
        let math_family = mathlayout::serif_family(font_system);
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
                Event::End(TagEnd::Paragraph) => {
                    // Justified columns are what make the page read as typeset;
                    // list and quote bodies stay ragged, as LaTeX sets them.
                    let align =
                        (list_stack.is_empty() && quote_depth == 0).then_some(Align::Justified);
                    flush!(self, spans, heading, list_stack, quote_depth, align);
                }
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
                Event::Start(Tag::CodeBlock(kind)) => {
                    flush!(self, spans, heading, list_stack, quote_depth);
                    in_code_block = true;
                    // ```math is GitHub's spelling of a display equation.
                    code_math = matches!(&kind, CodeBlockKind::Fenced(lang)
                        if lang.trim().eq_ignore_ascii_case("math"));
                    code_text.clear();
                }
                Event::End(TagEnd::CodeBlock) => {
                    in_code_block = false;
                    let attrs = mono(self.font_family).color(fg);
                    let text = code_text.trim_end().to_string();
                    if code_math {
                        let (body, number) = split_math_tag(&text);
                        let node = crate::tex::parse_math(&body);
                        self.push_math(font_system, &node, number.as_deref());
                    } else if !text.is_empty() {
                        let px = (self.font_px * 0.92).round();
                        let mut buffer =
                            Buffer::new(font_system, Metrics::new(px, (px * 1.4).round()));
                        buffer.set_wrap(Wrap::WordOrGlyph);
                        buffer.set_text(&text, &attrs, Shaping::Advanced, None);
                        self.blocks.push(Block::Text {
                            buffer,
                            indent: 0.0,
                            inset_r: 0.0,
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
                        if !self.push_image_file(&img_path, None) {
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
                // Inline math stays in the line of running text, so it gets the
                // same Unicode fallback (x², xᵢ, α) the LaTeX view uses inline —
                // with variables slanted and operators upright.
                Event::InlineMath(body) => {
                    let node = crate::tex::parse_math(&body);
                    for span in crate::tex::math_spans(&node) {
                        let mut attrs =
                            attrs_for(bold, italic, 0, false, self.font_family).family(math_family);
                        if span.italic {
                            attrs = attrs.style(Style::Italic);
                        }
                        attrs = attrs.metadata(match span.script {
                            1 => SCRIPT_SUP,
                            -1 => SCRIPT_SUB,
                            _ => SCRIPT_NONE,
                        });
                        if span.script != 0 {
                            let px = (self.font_px * 0.72).round().max(5.0);
                            attrs = attrs.metrics(Metrics::new(px, (self.font_px * 1.5).round()));
                        }
                        spans.push((span.text, attrs));
                    }
                }
                // A display equation becomes its own laid-out block — except
                // inside a table, where a block would break the grid.
                Event::DisplayMath(body) => {
                    let (body, number) = split_math_tag(&body);
                    let node = crate::tex::parse_math(&body);
                    if table.is_some() {
                        for span in crate::tex::math_spans(&node) {
                            let mut attrs = attrs_for(bold, italic, 0, false, self.font_family)
                                .family(math_family);
                            if span.italic {
                                attrs = attrs.style(Style::Italic);
                            }
                            spans.push((span.text, attrs));
                        }
                    } else {
                        flush!(self, spans, heading, list_stack, quote_depth);
                        self.push_math(font_system, &node, number.as_deref());
                    }
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

    /// Lays out a display equation as boxes — fraction bars, radicals,
    /// stretched delimiters and all. Shared by the LaTeX and Markdown views so
    /// the same formula renders identically whichever file it came from.
    fn push_math(
        &mut self,
        font_system: &mut FontSystem,
        node: &crate::tex::MathNode,
        number: Option<&str>,
    ) {
        let width = self.column_width();
        self.push_math_within(font_system, node, number, width);
    }

    /// Lays out a display equation to fit `max_w`. A formula wider than its
    /// column is set a size or two smaller, the way one would in TeX, rather
    /// than running into the neighbouring column.
    fn push_math_within(
        &mut self,
        font_system: &mut FontSystem,
        node: &crate::tex::MathNode,
        number: Option<&str>,
        max_w: f32,
    ) {
        let ink = self.text_color();
        let px = self.font_px;
        let num = number
            .map(|n| mathlayout::layout_number(font_system, n, px, ink).into_top_left());
        let room = (max_w - num.as_ref().map_or(0.0, |n: &MathBox| n.width + px)).max(px * 4.0);
        let mut size = px * 1.06;
        let mut bx = mathlayout::layout(font_system, node, size, ink).into_top_left();
        if bx.width > room {
            // One measured shrink, floored so a runaway formula stays legible
            // (and is clipped by the column instead of shrinking to nothing).
            size = (size * room / bx.width).max(px * 0.62);
            bx = mathlayout::layout(font_system, node, size, ink).into_top_left();
        }
        // Hidden copy of the formula so Cmd+A still picks it up.
        let attrs = Attrs::new().family(mathlayout::serif_family(font_system)).color(ink);
        let mut text = Buffer::new(font_system, Metrics::new(size, (size * 1.4).round()));
        text.set_text(&crate::tex::math_text(node), &attrs, Shaping::Advanced, None);
        let height = bx.height().max(num.as_ref().map_or(0.0, MathBox::height));
        self.blocks.push(Block::Math { bx, number: num, text, height });
    }

    /// Pushes a paragraph-like block with an explicit alignment.
    fn push_aligned(
        &mut self,
        font_system: &mut FontSystem,
        spans: &[(String, Attrs<'static>)],
        base_px: f32,
        indent: f32,
        align: Option<Align>,
    ) {
        self.push_inset(font_system, spans, base_px, indent, 0.0, align)
    }

    /// Same, with space kept on the right as well: an abstract or a quote is
    /// pulled in from both margins.
    fn push_inset(
        &mut self,
        font_system: &mut FontSystem,
        spans: &[(String, Attrs<'static>)],
        base_px: f32,
        indent: f32,
        inset_r: f32,
        align: Option<Align>,
    ) {
        if spans.iter().all(|(t, _)| t.trim().is_empty()) {
            return;
        }
        let mut buffer =
            Buffer::new(font_system, Metrics::new(base_px, (base_px * self.leading).round()));
        buffer.set_wrap(Wrap::WordOrGlyph);
        let default = Attrs::new()
            .family(mathlayout::serif_family(font_system))
            .color(self.text_color());
        buffer.set_rich_text(
            spans.iter().map(|(t, a)| (t.as_str(), a.clone())),
            &default,
            Shaping::Advanced,
            None,
        );
        if align.is_some() {
            for line in buffer.lines.iter_mut() {
                line.set_align(align);
            }
        }
        self.blocks.push(Block::Text { buffer, indent, inset_r, bg: None, height: 0.0 });
    }

    /// Adds a figure. Bitmaps go through the image worker as usual; a vector
    /// PDF is rasterized once here, since the worker only decodes rasters.
    fn push_figure(&mut self, path: &Path, fill: Option<f32>) -> bool {
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
            // One generous rasterization, scaled down to whatever width the
            // column ends up being.
            const FIGURE_W: u32 = 1600;
            let Some((img, _)) = rasterize_pdf_figure(path, FIGURE_W) else {
                return false;
            };
            // Measure from the raster, not the PDF's point size: papers set
            // vector figures at \linewidth, and we have the pixels for it.
            let size = (img.width() as f32, img.height() as f32);
            self.blocks.push(Block::Picture { scaled: img, size, fill, height: 0.0 });
            return true;
        }
        self.push_image_file(path, fill)
    }

    /// Formats a LaTeX source file. The tex module parses it into blocks and
    /// display equations; this sets them on the page the document class asks
    /// for — same paper, same margins, same column grid, scaled to the pane —
    /// so the result reads like the PDF the source compiles to.
    fn build_tex(&mut self, font_system: &mut FontSystem, path: &Path) -> Result<(), String> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let base_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let style = crate::tex::document_style(&source);
        // Where \includegraphics looks: the document's own folder first, then
        // whatever \graphicspath declares, then the usual figure folders.
        let mut search: Vec<std::path::PathBuf> = vec![base_dir.clone()];
        for dir in crate::tex::graphics_paths(&source) {
            search.push(base_dir.join(dir));
        }
        for dir in ["figures", "figs", "images", "img", "media"] {
            let candidate = base_dir.join(dir);
            if candidate.is_dir() && !search.contains(&candidate) {
                search.push(candidate);
            }
        }

        // The sheet fits the pane the way the PDF viewer fits a page, and
        // every length on it is the page's own length at that scale — except
        // in a pane too narrow to read at: there the page keeps a legible
        // body size and pans sideways instead, as a PDF page would.
        let fit = ((self.width_px - 2.0 * PAGE_GUTTER).max(160.0) / style.page_w)
            .max(MIN_BODY_PX / style.base_pt);
        let scale = fit * self.zoom;
        let sheet_w = (style.page_w * scale).min(MAX_DRAW_W);
        let scale = sheet_w / style.page_w;
        let geom = PageGeom {
            w: sheet_w,
            h: (style.page_h * scale).round(),
            margin_x: (style.margin_x * scale).round(),
            margin_y: (style.margin_y * scale).round(),
            columns: style.columns,
            gutter: (style.gutter * scale).round(),
        };
        let body_px = (style.base_pt * scale).max(5.0);
        self.paper = true;
        self.page_geom = Some(geom);
        self.font_px = body_px;
        self.margin = geom.margin_x;
        self.spacing = (body_px * 0.4).round();
        // TeX sets solid paragraphs: no space between them, one line of
        // leading, and an indented first line instead.
        self.leading = 1.22;

        let ink = Color::rgb(PAPER_INK[0], PAPER_INK[1], PAPER_INK[2]);
        let muted = Color::rgb(PAPER_MUTED[0], PAPER_MUTED[1], PAPER_MUTED[2]);
        let link = Color::rgb(PAPER_LINK[0], PAPER_LINK[1], PAPER_LINK[2]);
        let fam = self.font_family;
        let serif = Attrs::new().family(mathlayout::serif_family(font_system));

        let attrs_serif = serif.clone();
        let to_attrs = move |s: &crate::tex::Span| -> Attrs<'static> {
            let mut attrs = if s.mono { mono(fam) } else { attrs_serif.clone() };
            attrs = attrs.color(if s.link { link } else { ink });
            if s.bold {
                attrs = attrs.weight(Weight::BOLD);
            }
            if s.italic {
                attrs = attrs.style(Style::Italic);
            }
            attrs = attrs.metadata(match s.script {
                1 => SCRIPT_SUP,
                -1 => SCRIPT_SUB,
                _ => SCRIPT_NONE,
            });
            attrs
        };
        let leading = self.leading;
        let to_spans = |spans: &[crate::tex::Span], px: f32| -> Vec<(String, Attrs<'static>)> {
            spans
                .iter()
                .map(|s| {
                    let mut attrs = to_attrs(s);
                    // Scripts are set small, as TeX does, and shifted off the
                    // baseline by the painter; \large and friends scale the
                    // run outright.
                    let scale = if s.script != 0 { s.size * 0.72 } else { s.size };
                    if (scale - 1.0).abs() > 0.01 {
                        let sp = (px * scale).round().max(5.0);
                        let line = (px * s.size.max(1.0) * leading).round();
                        attrs = attrs.metrics(Metrics::new(sp, line));
                    }
                    (s.text.clone(), attrs)
                })
                .collect()
        };
        // The tallest run decides the block's own size, so a `\LARGE` title
        // inside a centered group is not cramped.
        let peak = |spans: &[crate::tex::Span]| spans.iter().map(|s| s.size).fold(1.0f32, f32::max);
        let align_of = |a: crate::tex::TexAlign| match a {
            crate::tex::TexAlign::Center => Some(Align::Center),
            crate::tex::TexAlign::Right => Some(Align::Right),
            crate::tex::TexAlign::Left => Some(Align::Left),
            crate::tex::TexAlign::Justify => Some(Align::Justified),
        };

        // A paragraph's first line is indented, except right after a heading
        // or a display, which is TeX's rule — unless the preamble set
        // \parindent to zero and separates paragraphs with space instead.
        let indent_em = style.parindent * scale;
        let parskip = style.parskip * scale;
        let mut fresh = true;
        // Space owed under the block just pushed, applied to the next one.
        let mut carry = 0.0f32;
        let em = body_px;

        for block in crate::tex::parse_with(&source, &style) {
            let start = self.blocks.len();
            let (before, after) = match &block {
                crate::tex::TexBlock::Heading { level, .. } => match level {
                    0 => (0.0, 0.35 * em),
                    1 => (1.5 * em, 0.55 * em),
                    2 => (1.1 * em, 0.4 * em),
                    3 => (0.9 * em, 0.3 * em),
                    5 => (1.2 * em, 0.45 * em),
                    _ => (0.7 * em, 0.25 * em),
                },
                crate::tex::TexBlock::Paragraph { .. } => (parskip, 0.0),
                crate::tex::TexBlock::Byline { small, .. } => {
                    if *small {
                        (1.0 * em, 0.8 * em)
                    } else {
                        (0.5 * em, 0.2 * em)
                    }
                }
                crate::tex::TexBlock::Math { .. } => (0.75 * em, 0.75 * em),
                crate::tex::TexBlock::ListItem { .. } => (0.15 * em, 0.15 * em),
                crate::tex::TexBlock::Table { .. } => (0.7 * em, 0.5 * em),
                crate::tex::TexBlock::Image { .. } => (0.9 * em, 0.4 * em),
                crate::tex::TexBlock::Caption { .. } => (0.35 * em, 0.9 * em),
                crate::tex::TexBlock::Code(_) => (0.6 * em, 0.6 * em),
                crate::tex::TexBlock::Rule => (0.5 * em, 0.5 * em),
            };
            match block {
                crate::tex::TexBlock::Heading { level, spans } => {
                    // Title and section heads take the class's own style: the
                    // two-column journals center a small-caps head, the
                    // article classes set a large bold one flush left.
                    let ieee = style.columns == 2;
                    let (scale, align, bold, italic, upper) = match level {
                        0 => (1.65, Some(Align::Center), false, false, false),
                        1 if ieee => (1.0, Some(Align::Center), false, false, true),
                        2 if ieee => (1.0, None, false, true, false),
                        3 if ieee => (1.0, None, false, true, false),
                        1 => (1.44, None, true, false, false),
                        2 => (1.2, None, true, false, false),
                        3 => (1.05, None, true, false, false),
                        // The "Abstract" label of an article, centered over
                        // the block it introduces.
                        5 => (0.98, Some(Align::Center), true, false, false),
                        _ => (1.0, None, true, false, false),
                    };
                    let px = (body_px * scale).round();
                    let styled: Vec<(String, Attrs<'static>)> = spans
                        .iter()
                        .map(|s| {
                            let mut b = s.clone();
                            b.bold |= bold;
                            b.italic |= italic;
                            if upper {
                                b.text = b.text.to_uppercase();
                            }
                            (b.text.clone(), to_attrs(&b))
                        })
                        .collect();
                    self.push_aligned(font_system, &styled, px, 0.0, align);
                    self.mark_keep(start);
                    if level == 0 {
                        // The title block runs across the whole page, above
                        // the columns, the way \maketitle sets it.
                        self.mark_spanning(start);
                    }
                    fresh = true;
                }
                crate::tex::TexBlock::Byline { spans, small } => {
                    let px = if small { (body_px * 0.8).round() } else { body_px };
                    let styled = to_spans(&spans, px);
                    let align = if small { Some(Align::Justified) } else { Some(Align::Center) };
                    self.push_aligned(font_system, &styled, px, 0.0, align);
                    self.mark_spanning(start);
                    fresh = true;
                }
                crate::tex::TexBlock::Paragraph { spans, align, inset } => {
                    let px = (body_px * if inset { 0.94 } else { 1.0 }).round();
                    let mut styled = to_spans(&spans, px);
                    let centered = align != crate::tex::TexAlign::Justify;
                    if !fresh && !centered && indent_em >= 1.0 {
                        // An em quad at the indent's size gives TeX's
                        // \parindent without touching the shaper.
                        styled.insert(0, ("\u{2003}".into(), serif.clone().color(ink).metrics(
                            Metrics::new(indent_em, (body_px * leading).round()),
                        )));
                    }
                    // Justified columns are the strongest visual cue that
                    // this is a typeset document rather than a text dump.
                    let pad = if inset { body_px * 2.2 } else { 0.0 };
                    let base = (px * peak(&spans)).round();
                    self.push_inset(font_system, &styled, base, pad, pad, align_of(align));
                    fresh = false;
                }
                crate::tex::TexBlock::Code(text) => {
                    let attrs = mono(fam).color(ink);
                    let px = (body_px * 0.88).round();
                    let mut buffer =
                        Buffer::new(font_system, Metrics::new(px, (px * 1.35).round()));
                    buffer.set_wrap(Wrap::WordOrGlyph);
                    buffer.set_text(&text, &attrs, Shaping::Advanced, None);
                    self.blocks.push(Block::Text {
                        buffer,
                        indent: 0.0,
                        inset_r: 0.0,
                        bg: Some(PAPER_CODE_BG),
                        height: 0.0,
                    });
                    fresh = true;
                }
                crate::tex::TexBlock::Math { node, number } => {
                    self.push_math(font_system, &node, number.as_deref());
                    fresh = true;
                }
                crate::tex::TexBlock::ListItem { indent, marker, spans } => {
                    let mut styled = Vec::with_capacity(spans.len() + 1);
                    if !marker.is_empty() {
                        styled.push((marker, serif.clone().color(ink)));
                    }
                    styled.extend(to_spans(&spans, body_px));
                    let pad = indent as f32 * 1.4 * body_px;
                    self.push_aligned(font_system, &styled, body_px, pad, Some(Align::Justified));
                    fresh = true;
                }
                crate::tex::TexBlock::Table { rows, float } => {
                    let px = (body_px * 0.86).round().max(6.0);
                    let cells: Vec<Vec<Vec<(String, Attrs<'static>)>>> = rows
                        .iter()
                        .enumerate()
                        .map(|(r, row)| {
                            row.iter()
                                .map(|cell| {
                                    let bolded: Vec<crate::tex::Span> = cell
                                        .iter()
                                        .map(|s| {
                                            // First row bolded as the header.
                                            let mut b = s.clone();
                                            b.bold |= r == 0;
                                            b
                                        })
                                        .collect();
                                    to_spans(&bolded, px)
                                })
                                .collect()
                        })
                        .collect();
                    self.push_table_sized(font_system, cells, px);
                    self.mark_float(start, float, style.columns);
                    fresh = true;
                }
                crate::tex::TexBlock::Image { path: name, width, float } => {
                    // \includegraphics usually omits the extension and lets
                    // the driver pick; try the formats we can decode, in each
                    // of the folders LaTeX would search.
                    let mut resolved = None;
                    'search: for dir in &search {
                        let direct = dir.join(&name);
                        if direct.is_file() {
                            resolved = Some(direct);
                            break;
                        }
                        for ext in ["pdf", "png", "jpg", "jpeg"] {
                            let candidate = dir.join(format!("{name}.{ext}"));
                            if candidate.is_file() {
                                resolved = Some(candidate);
                                break 'search;
                            }
                        }
                    }
                    let shown = resolved.is_some_and(|p| self.push_figure(&p, width));
                    if !shown {
                        let attrs = serif.clone().color(muted).style(Style::Italic);
                        let styled = vec![(format!("[figure: {name}]"), attrs)];
                        self.push_aligned(
                            font_system,
                            &styled,
                            body_px,
                            0.0,
                            Some(Align::Center),
                        );
                    }
                    self.mark_keep(start);
                    self.mark_float(start, float, style.columns);
                    fresh = true;
                }
                crate::tex::TexBlock::Caption { spans, float } => {
                    let px = (body_px * 0.86).round();
                    let styled = to_spans(&spans, px);
                    self.push_aligned(font_system, &styled, px, 0.0, Some(Align::Justified));
                    // A caption belongs with the float it names.
                    self.mark_keep(start);
                    self.mark_float(start, float, style.columns);
                    fresh = true;
                }
                crate::tex::TexBlock::Rule => {
                    self.blocks.push(Block::Rule);
                    fresh = true;
                }
            }
            self.finish_block(start, before, after, &mut carry);
        }
        if self.blocks.is_empty() {
            let attrs = serif.color(ink);
            self.push_plain(font_system, "(empty file)", attrs, Wrap::WordOrGlyph);
        }
        Ok(())
    }

    /// Records the space kept above the blocks a source block turned into,
    /// and carries the space owed under it to the next one. TeX quotes both
    /// halves for every construct; the larger of the two wins, as it does in
    /// TeX's own glue.
    fn finish_block(&mut self, start: usize, before: f32, after: f32, carry: &mut f32) {
        if self.blocks.len() <= start {
            return;
        }
        while self.gaps.len() < self.blocks.len() {
            self.gaps.push(self.spacing);
        }
        self.gaps[start] = before.max(*carry).round();
        *carry = after;
    }

    /// Marks the block at `idx` as one that must not be stranded at the foot
    /// of a column without what follows it.
    fn mark_keep(&mut self, idx: usize) {
        while self.keep_next.len() <= idx {
            self.keep_next.push(false);
        }
        self.keep_next[idx] = true;
    }

    /// Marks the block at `idx` as running across every column.
    fn mark_spanning(&mut self, idx: usize) {
        while self.spanning.len() <= idx {
            self.spanning.push(false);
        }
        self.spanning[idx] = true;
    }

    /// Records how the float a block came from may be set: spanning every
    /// column, and/or free to move to the top of a later column.
    fn mark_float(&mut self, idx: usize, float: crate::tex::FloatInfo, columns: usize) {
        if float.wide && columns > 1 {
            self.mark_spanning(idx);
        }
        if float.movable {
            while self.floating.len() <= idx {
                self.floating.push(false);
            }
            self.floating[idx] = true;
        }
    }

    /// Recomputes block sizes for the current width and zoom, then places
    /// every block: into the column grid of a paginated document, or one
    /// under the other for the views that simply flow.
    fn reflow(&mut self, font_system: &mut FontSystem) {
        let col_w = self.column_width();
        // A block that spans the page — the title block, a starred float —
        // is laid out for the whole text width instead of one column.
        let full_w = match &self.page_geom {
            Some(g) => g.w - 2.0 * g.margin_x,
            None => col_w,
        };
        let spanning = std::mem::take(&mut self.spanning);
        let zoom = self.zoom;
        let mut max_w = col_w;
        for (bi, block) in self.blocks.iter_mut().enumerate() {
            let text_w = if spanning.get(bi).copied().unwrap_or(false) { full_w } else { col_w };
            match block {
                Block::Text { buffer, indent, inset_r, bg, height } => {
                    buffer.set_size(Some((text_w - *indent - *inset_r).max(48.0)), None);
                    buffer.shape_until_scroll(font_system, false);
                    let mut h = 0.0f32;
                    for run in buffer.layout_runs() {
                        h = h.max(run.line_top + run.line_height);
                    }
                    *height = h + if bg.is_some() { self.font_px } else { 0.0 };
                }
                Block::Picture { size, fill, height, .. } => {
                    let (draw_w, draw_h) =
                        fit_draw(size.0, size.1, picture_target_w(*size, *fill, text_w, zoom));
                    *height = draw_h;
                    max_w = max_w.max(draw_w);
                }
                Block::Page { size, height, .. } => {
                    let (draw_w, draw_h) = fit_draw(size.0, size.1, text_w * zoom);
                    *height = draw_h;
                    max_w = max_w.max(draw_w);
                }
                Block::Rule => {}
                // Equations are laid out once, at build time: their size does
                // not depend on the column width.
                Block::Math { bx, .. } => {
                    max_w = max_w.max(bx.width);
                }
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
                }
            }
        }
        self.spanning = spanning;
        self.place(max_w);
        self.clamp_scroll();
    }

    /// Width one column of text is laid out for: a page's column in a
    /// paginated document, the whole pane minus margins otherwise.
    fn column_width(&self) -> f32 {
        match &self.page_geom {
            Some(g) => g.column_w(),
            None => self.text_width(),
        }
    }

    /// Left edge of the sheet within the pane. A page wider than the pane
    /// (zoomed in) starts at the gutter and pans horizontally instead.
    fn sheet_x(&self) -> f32 {
        let Some(g) = &self.page_geom else { return self.margin };
        ((self.width_px - g.w) / 2.0).max(PAGE_GUTTER)
    }

    /// Assigns every block a position. Paginated documents fill one column
    /// after another and start a new sheet when the last column is full,
    /// splitting a paragraph that runs off the bottom the way TeX would;
    /// everything else stacks in one flowing column.
    fn place(&mut self, max_w: f32) {
        self.places.clear();
        self.sheets.clear();
        let Some(g) = self.page_geom else {
            let text_w = self.text_width();
            let mut y = self.margin;
            for (i, block) in self.blocks.iter().enumerate() {
                let h = self.block_height(block);
                if i > 0 {
                    y += self.gap_before(i);
                }
                self.places.push(Frag {
                    block: i,
                    x: self.margin,
                    y,
                    w: text_w,
                    h,
                    from: 0.0,
                    to: f32::MAX,
                });
                y += h;
            }
            self.content_h = y + self.margin;
            self.content_w = max_w + 2.0 * self.margin;
            return;
        };

        let sheet_x = self.sheet_x();
        let full_w = g.w - 2.0 * g.margin_x;
        let col_w = g.column_w();
        let full_h = g.column_h();
        let mut sheet_top = PAGE_GAP;
        // Height taken by the full-width blocks at the top of the sheet — a
        // title block or a starred float — and what is left for the columns.
        let mut col_top = 0.0f32;
        let mut col_h = full_h;
        let mut col = 0usize;
        // Height used in the current column, and the deepest column on this
        // sheet: a block taller than a column grows the sheet rather than
        // spilling over its edge.
        let mut used = 0.0f32;
        let mut deepest = 0.0f32;
        let mut i = 0usize;
        // Where the current block starts drawing, for a paragraph continued
        // from the previous column.
        let mut from = 0.0f32;
        // Floats waiting for the top of a column (or, for the full-width
        // ones, a page), exactly as TeX holds a float over.
        let mut deferred: Vec<usize> = Vec::new();

        macro_rules! place_full {
            ($idx:expr) => {{
                let idx = $idx;
                let gap = if col_top > 0.0 { self.gap_before(idx) } else { 0.0 };
                let h = self.block_height(&self.blocks[idx]);
                self.places.push(Frag {
                    block: idx,
                    x: sheet_x + g.margin_x,
                    y: sheet_top + g.margin_y + col_top + gap,
                    w: full_w,
                    h,
                    from: 0.0,
                    to: f32::MAX,
                });
                col_top += gap + h;
                deepest = deepest.max(col_top);
            }};
        }
        macro_rules! finish_sheet {
            () => {{
                let sheet_h = g.h.max(deepest + 2.0 * g.margin_y);
                self.sheets.push((sheet_top, sheet_h));
                sheet_top += sheet_h + PAGE_GAP;
                col = 0;
                used = 0.0;
                deepest = 0.0;
                col_top = 0.0;
                col_h = full_h;
            }};
        }

        while i < self.blocks.len() {
            // A column starts by taking whatever floats are waiting, which is
            // where TeX puts them too.
            if used == 0.0 && !deferred.is_empty() {
                let mut k = 0;
                while k < deferred.len() {
                    let b = deferred[k];
                    if self.spanning.get(b).copied().unwrap_or(false) {
                        k += 1;
                        continue;
                    }
                    let h = self.block_height(&self.blocks[b]);
                    let gap = if used > 0.0 { self.gap_before(b) } else { 0.0 };
                    if used > 0.0 && gap + h > col_h - used {
                        break;
                    }
                    self.places.push(Frag {
                        block: b,
                        x: sheet_x + g.column_x(col),
                        y: sheet_top + g.margin_y + col_top + used + gap,
                        w: col_w,
                        h,
                        from: 0.0,
                        to: f32::MAX,
                    });
                    used += gap + h;
                    deepest = deepest.max(col_top + used);
                    deferred.remove(k);
                }
            }
            // The top of a sheet is where full-width material goes: the title
            // block, and any float that has been waiting for a page top.
            if col == 0 && used == 0.0 {
                while !deferred.is_empty() && col_top < full_h * 0.8 {
                    place_full!(deferred.remove(0));
                }
                while i < self.blocks.len()
                    && self.spanning.get(i).copied().unwrap_or(false)
                    && col_top < full_h * 0.8
                {
                    place_full!(i);
                    i += 1;
                }
                col_h = (full_h - col_top).max(full_h * 0.15);
                if i >= self.blocks.len() {
                    break;
                }
            }
            if self.spanning.get(i).copied().unwrap_or(false) {
                // Mid-page: hold it over for the next page rather than
                // squeezing it into a column.
                deferred.push(i);
                i += 1;
                continue;
            }
            // A float that will not fit here travels to the top of the next
            // column, and the text after it keeps flowing — which is what
            // leaves a LaTeX page full instead of half empty.
            if self.floating.get(i).copied().unwrap_or(false) && used > 0.0 {
                let (end, need) = self.float_group(i);
                if need > col_h - used {
                    for b in i..end {
                        deferred.push(b);
                    }
                    i = end;
                    continue;
                }
            }
            let gap = if used > 0.0 && from == 0.0 { self.gap_before(i) } else { 0.0 };
            let h = self.block_height(&self.blocks[i]) - from;
            let left = col_h - used - gap;
            // A paragraph splits so the column stays full; anything else moves
            // whole, and a block too tall for any column grows the sheet
            // instead of hanging off it. A heading keeps the block under it
            // company rather than sitting alone at the foot of a column.
            let keep = self.keep_next.get(i).copied().unwrap_or(false);
            // A heading or caption needs the head of the next block with it.
            let need = h + if keep { self.keep_room(i) } else { 0.0 };
            let cut = (h > left + 0.5 && !keep)
                .then(|| self.split_at(i, from, left))
                .flatten();
            if need > left + 0.5 && (used > 0.0 || cut.is_some()) {
                if let Some(cut) = cut {
                    self.places.push(Frag {
                        block: i,
                        x: sheet_x + g.column_x(col),
                        y: sheet_top + g.margin_y + col_top + used + gap,
                        w: col_w,
                        h: cut - from,
                        from,
                        to: cut,
                    });
                    deepest = deepest.max(col_top + used + gap + cut - from);
                    from = cut;
                }
                col += 1;
                used = 0.0;
                if col >= g.columns.max(1) {
                    finish_sheet!();
                }
                continue;
            }
            self.places.push(Frag {
                block: i,
                x: sheet_x + g.column_x(col),
                y: sheet_top + g.margin_y + col_top + used + gap,
                w: col_w,
                h,
                from,
                to: f32::MAX,
            });
            used += gap + h;
            deepest = deepest.max(col_top + used);
            from = 0.0;
            i += 1;
        }
        // Floats still in hand get pages of their own at the end, which is
        // where LaTeX flushes them too.
        while !deferred.is_empty() {
            if deepest > 0.0 {
                finish_sheet!();
            }
            let before = deferred.len();
            while !deferred.is_empty() && col_top < full_h * 0.8 {
                place_full!(deferred.remove(0));
            }
            if deferred.len() == before {
                break;
            }
        }
        if deepest > 0.0 || !self.blocks.is_empty() {
            let sheet_h = g.h.max(deepest + 2.0 * g.margin_y);
            self.sheets.push((sheet_top, sheet_h));
            sheet_top += sheet_h + PAGE_GAP;
        }
        self.content_h = sheet_top;
        self.content_w = (g.w + 2.0 * PAGE_GUTTER).max(self.width_px);
        // The trailing flush leaves the column cursors behind on purpose.
        let _ = (col, used, col_h);
    }

    /// The run of blocks a float contributes — its caption and its body —
    /// as (one past the last block, the height they need together).
    fn float_group(&self, start: usize) -> (usize, f32) {
        let mut end = start;
        let mut need = 0.0;
        while end < self.blocks.len() && self.floating.get(end).copied().unwrap_or(false) {
            if end > start {
                need += self.gap_before(end);
            }
            need += self.block_height(&self.blocks[end]);
            end += 1;
        }
        (end.max(start + 1), need)
    }

    /// Blank space to keep under block `i`: a heading (or a caption above its
    /// figure) needs the start of what follows on the same column, or it
    /// reads as a stranded line.
    fn keep_room(&self, i: usize) -> f32 {
        if !self.keep_next.get(i).copied().unwrap_or(false) {
            return 0.0;
        }
        match self.blocks.get(i + 1) {
            Some(next) => self.block_height(next).min(self.font_px * 3.0),
            None => 0.0,
        }
    }

    /// Space kept above block `i`. The LaTeX builder sets these per block —
    /// paragraphs run on with no gap at all, headings get air above them —
    /// and everything else falls back to the view's uniform spacing.
    fn gap_before(&self, i: usize) -> f32 {
        self.gaps.get(i).copied().unwrap_or(self.spacing)
    }

    /// The line boundary of a text block nearest under `budget` pixels from
    /// `from`, or None when the block cannot usefully be split there (it is
    /// not text, or one of the halves would be a single stranded line).
    fn split_at(&self, block: usize, from: f32, budget: f32) -> Option<f32> {
        let Some(Block::Text { buffer, bg, .. }) = self.blocks.get(block) else { return None };
        if bg.is_some() {
            // Code panels carry a background; splitting one would cut the box.
            return None;
        }
        let tops: Vec<f32> = buffer
            .layout_runs()
            .map(|r| r.line_top)
            .filter(|t| *t >= from - 0.5)
            .collect();
        // Two lines have to stay on each side of the break.
        if tops.len() < 4 {
            return None;
        }
        let cut = tops
            .iter()
            .copied()
            .skip(2)
            .take(tops.len() - 4 + 1)
            .filter(|t| *t - from <= budget)
            .next_back()?;
        Some(cut)
    }

    /// The laid-out height of a block, spacing excluded.
    fn block_height(&self, block: &Block) -> f32 {
        match block {
            Block::Rule => self.font_px,
            Block::Text { height, .. }
            | Block::Picture { height, .. }
            | Block::Page { height, .. }
            | Block::Math { height, .. }
            | Block::Table { height, .. } => *height,
        }
    }

    pub fn set_viewport(&mut self, font_system: &mut FontSystem, width_px: f32, _height_px: f32) {
        if (width_px - self.width_px).abs() > 1.0 {
            // A page's type size follows its width, so a resized pane
            // re-typesets the document rather than reflowing the old sizes —
            // but only once the width has really moved, since dragging the
            // split sends a stream of one-pixel changes and setting a long
            // paper again is not free.
            let step = (self.width_px * 0.015).max(2.0);
            let retypeset = self.kind == ViewKind::Tex && (width_px - self.width_px).abs() > step;
            self.width_px = width_px.max(64.0);
            if retypeset {
                self.rebuild_tex(font_system);
            } else {
                self.reflow(font_system);
            }
        }
    }

    /// Re-typesets a LaTeX document after the page scale changed (a resized
    /// pane, a zoom step), keeping the reader roughly where they were.
    fn rebuild_tex(&mut self, font_system: &mut FontSystem) {
        let anchor = if self.content_h > 1.0 { self.scroll / self.content_h } else { 0.0 };
        let path = self.path.clone();
        self.blocks.clear();
        self.places.clear();
        self.sheets.clear();
        self.gaps.clear();
        self.keep_next.clear();
        self.spanning.clear();
        self.sel_anchor = None;
        self.sel_head = None;
        // The image worker holds the originals and is keyed by block index,
        // which the rebuild reproduces exactly; the new picture blocks simply
        // ask it for their bitmaps again.
        self.img_paths.clear();
        self.pending_imgs.clear();
        if self.build_tex(font_system, &path).is_err() {
            return;
        }
        self.img_paths.clear();
        self.reflow(font_system);
        self.scroll = anchor * self.content_h;
        self.clamp_scroll();
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

    /// Mouse on the pane (physical px; kind 0=down 1=up 2=move 3=double-
    /// click). Consumes presses on the scrollbar and drags of its thumb, and
    /// drives text selection everywhere else; returns whether the event
    /// changed anything worth repainting.
    pub fn handle_mouse(&mut self, kind: i32, x: f32, y: f32, view_h: f32) -> bool {
        match kind {
            0 => {
                if let Some((bar_x, thumb_y, thumb_h)) = self.scrollbar(view_h) {
                    if x >= bar_x {
                        // Grabbing the thumb keeps the cursor's spot on it; a
                        // track click jumps there, continuing as a centered drag.
                        let grab = if y >= thumb_y && y < thumb_y + thumb_h {
                            y - thumb_y
                        } else {
                            thumb_h / 2.0
                        };
                        self.drag = Some(grab);
                        self.drag_to(y, grab, view_h);
                        return true;
                    }
                }
                // Elsewhere a press drops the old selection and anchors a new one.
                let had = self.clear_selection();
                if let Some(pos) = self.hit_text(x, y) {
                    self.sel_anchor = Some(pos);
                    self.sel_head = Some(pos);
                    self.selecting = true;
                } else if let Some(pos) = self.hit_page(x, y) {
                    self.page_sel_anchor = Some(pos);
                    self.page_sel_head = Some(pos);
                    self.selecting = true;
                }
                had
            }
            2 => {
                if let Some(grab) = self.drag {
                    self.drag_to(y, grab, view_h);
                    return true;
                }
                if self.selecting {
                    // Dragging past the pane edges scrolls the document along.
                    if y < 0.0 {
                        self.scroll += y * 0.3;
                    } else if y > view_h {
                        self.scroll += (y - view_h) * 0.3;
                    }
                    self.clamp_scroll();
                    if self.page_sel_anchor.is_some() {
                        // Keep the last good caret when the pointer wanders
                        // over a page whose text has not arrived yet.
                        if let Some(pos) = self.hit_page(x, y) {
                            self.page_sel_head = Some(pos);
                        }
                    } else {
                        self.sel_head = self.hit_text(x, y);
                    }
                    return true;
                }
                false
            }
            1 => {
                let selecting = std::mem::take(&mut self.selecting);
                self.drag.take().is_some() || selecting
            }
            3 => self.select_word_at(x, y) || self.select_page_word_at(x, y),
            _ => false,
        }
    }

    /// Paints the sheets a paper document sits on: one white rectangle per
    /// page, edged with a hairline so it still reads as paper against a light
    /// theme, and the page number centered in the bottom margin the way TeX
    /// prints it. Views that flow rather than paginate get one tall sheet.
    fn paint_sheets(
        &self,
        canvas: &mut Canvas,
        font_system: &mut FontSystem,
        swash_cache: &mut SwashCache,
        view_h: f32,
    ) {
        let Some(g) = self.page_geom else {
            let gutter = (self.margin * 0.35).round();
            let top = (-self.scroll).min(0.0).max(-1.0);
            let sheet_h = (self.content_h - self.scroll).min(view_h) - top;
            canvas.fill_rect(
                gutter as i32,
                top as i32,
                (self.width_px - 2.0 * gutter) as i32,
                sheet_h.max(0.0) as i32,
                PAPER_BG,
            );
            return;
        };
        let x = (self.sheet_x() - self.scroll_x) as i32;
        let edge = [214, 214, 218];
        let num_px = (self.font_px * 0.85).round().max(7.0);
        let attrs = Attrs::new()
            .family(mathlayout::serif_family(font_system))
            .color(Color::rgb(PAPER_MUTED[0], PAPER_MUTED[1], PAPER_MUTED[2]));
        for (i, (top, height)) in self.sheets.iter().enumerate() {
            let y = top - self.scroll;
            if y + height < 0.0 || y > view_h {
                continue;
            }
            canvas.fill_rect(x, y as i32, g.w as i32, *height as i32, PAPER_BG);
            let (w, h) = (g.w as i32, *height as i32);
            canvas.fill_rect(x - 1, y as i32 - 1, w + 2, 1, edge);
            canvas.fill_rect(x - 1, y as i32 + h, w + 2, 1, edge);
            canvas.fill_rect(x - 1, y as i32, 1, h, edge);
            canvas.fill_rect(x + w, y as i32, 1, h, edge);
            let mut buffer =
                Buffer::new(font_system, Metrics::new(num_px, (num_px * 1.3).round()));
            buffer.set_text(&(i + 1).to_string(), &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(font_system, false);
            let tw = buffer.layout_runs().map(|r| r.line_w).fold(0.0f32, f32::max);
            let nx = x + ((g.w - tw) / 2.0) as i32;
            let ny = (y + height - g.margin_y * 0.62) as i32;
            buffer.draw(font_system, swash_cache, attrs.color_opt.unwrap(), |px, py, pw, ph, color| {
                canvas.blend_rect(nx + px, ny + py, pw as i32, ph as i32, color);
            });
        }
    }

    /// Maps viewport coordinates to a text position. Points between or beyond
    /// text blocks clamp to the nearest one above (documents mix text with
    /// images and pages), so drags select contiguous ranges. None when the
    /// view has no text at all (plain images, page-rendered PDFs).
    fn hit_text(&self, x: f32, y: f32) -> Option<(usize, Cursor)> {
        let doc_y = y + self.scroll;
        let doc_x = x + self.scroll_x;
        let mut best: Option<(usize, Cursor)> = None;
        for frag in &self.places {
            let Block::Text { buffer, indent, bg, .. } = &self.blocks[frag.block] else { continue };
            let pad = if bg.is_some() { self.font_px / 2.0 } else { 0.0 };
            let cursor = buffer
                .hit(doc_x - frag.x - *indent, doc_y - frag.y - pad + frag.from)
                .unwrap_or_else(|| Cursor::new(0, 0));
            let inside = doc_y >= frag.y
                && doc_y < frag.y + frag.h
                && doc_x >= frag.x - self.spacing
                && doc_x < frag.x + frag.w + self.spacing;
            if inside {
                return Some((frag.block, cursor));
            }
            // Otherwise keep the last fragment that started above the
            // pointer, so a drag past the end of a column still selects.
            if doc_y >= frag.y || best.is_none() {
                best = Some((frag.block, cursor));
            }
        }
        best
    }

    /// Maps viewport coordinates to a caret in a rendered PDF page's text,
    /// as (page index, caret). None until that page's layer has arrived from
    /// the worker, or for views with no pages at all.
    fn hit_page(&self, x: f32, y: f32) -> Option<(usize, usize)> {
        let doc_y = y + self.scroll;
        // The page containing doc_y, or the last one above it.
        let mut found: Option<(usize, f32, f32)> = None;
        for frag in &self.places {
            if let Block::Page { index, size, .. } = &self.blocks[frag.block] {
                let draw_w = fit_draw(size.0, size.1, frag.w * self.zoom).0;
                found = Some((*index, frag.y, draw_w / size.0.max(1.0)));
                if doc_y < frag.y + frag.h {
                    break;
                }
            }
        }
        let (index, page_top, scale) = found?;
        let layer = self.page_text.get(&index)?;
        if layer.chars.is_empty() {
            return None;
        }
        let px = (x + self.scroll_x - self.margin) / scale;
        let py = (doc_y - page_top) / scale;
        Some((index, layer.caret_at(px, py)))
    }

    /// The ordered, non-empty PDF page selection, or None.
    fn page_sel_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let (a, b) = (self.page_sel_anchor?, self.page_sel_head?);
        if a == b {
            return None;
        }
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// The characters of `page` covered by the current selection.
    fn page_sel_slice(&self, page: usize) -> Option<&[PageChar]> {
        let ((start_page, start), (end_page, end)) = self.page_sel_range()?;
        if page < start_page || page > end_page {
            return None;
        }
        let layer = self.page_text.get(&page)?;
        let lo = if page == start_page { start } else { 0 };
        let hi = if page == end_page { end } else { layer.chars.len() };
        layer.chars.get(lo..hi.min(layer.chars.len()))
    }

    /// The selected text of a page-rendered PDF, with line breaks where the
    /// reading order moves to a new row.
    fn page_selected_text(&self) -> Option<String> {
        let ((start_page, _), (end_page, _)) = self.page_sel_range()?;
        let mut out = String::new();
        for page in start_page..=end_page {
            let Some(chars) = self.page_sel_slice(page) else { continue };
            if !out.is_empty() && !chars.is_empty() {
                out.push('\n');
            }
            let mut prev: Option<&PageChar> = None;
            for c in chars {
                match prev {
                    Some(p) if p.line != c.line => out.push('\n'),
                    // PDFs routinely draw words with no space glyph between
                    // them; a wide enough gap stands in for one.
                    Some(p) if c.x - (p.x + p.w) > c.h * 0.25 => out.push(' '),
                    _ => {}
                }
                out.push_str(&c.text);
                prev = Some(c);
            }
        }
        (!out.trim().is_empty()).then_some(out)
    }

    /// Double-click on a rendered page: selects the word under the pointer.
    fn select_page_word_at(&mut self, x: f32, y: f32) -> bool {
        let Some((page, caret)) = self.hit_page(x, y) else { return false };
        let Some(layer) = self.page_text.get(&page) else { return false };
        let i = caret.min(layer.chars.len().saturating_sub(1));
        let word = |c: &PageChar| c.text.chars().all(|ch| ch.is_alphanumeric() || ch == '_');
        if !word(&layer.chars[i]) {
            return false;
        }
        let line = layer.chars[i].line;
        let mut lo = i;
        while lo > 0 && layer.chars[lo - 1].line == line && word(&layer.chars[lo - 1]) {
            lo -= 1;
        }
        let mut hi = i + 1;
        while hi < layer.chars.len() && layer.chars[hi].line == line && word(&layer.chars[hi]) {
            hi += 1;
        }
        self.page_sel_anchor = Some((page, lo));
        self.page_sel_head = Some((page, hi));
        true
    }

    /// Double-click: selects the word under the pointer.
    fn select_word_at(&mut self, x: f32, y: f32) -> bool {
        let Some((block_i, cursor)) = self.hit_text(x, y) else { return false };
        let Some(Block::Text { buffer, .. }) = self.blocks.get(block_i) else { return false };
        let Some(line) = buffer.lines.get(cursor.line) else { return false };
        let text = line.text();
        let mut start = cursor.index.min(text.len());
        let mut end = start;
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        while let Some(c) = text[..start].chars().next_back() {
            if !is_word(c) {
                break;
            }
            start -= c.len_utf8();
        }
        while let Some(c) = text[end..].chars().next() {
            if !is_word(c) {
                break;
            }
            end += c.len_utf8();
        }
        if start == end {
            return false;
        }
        self.sel_anchor = Some((block_i, Cursor::new(cursor.line, start)));
        self.sel_head = Some((block_i, Cursor::new(cursor.line, end)));
        true
    }

    /// The ordered, non-empty selection, or None.
    fn sel_range(&self) -> Option<((usize, Cursor), (usize, Cursor))> {
        let (a, b) = (self.sel_anchor?, self.sel_head?);
        let key = |p: &(usize, Cursor)| (p.0, p.1.line, p.1.index);
        if key(&a) == key(&b) {
            return None;
        }
        Some(if key(&a) <= key(&b) { (a, b) } else { (b, a) })
    }

    /// Drops the selection; returns whether there was one to drop.
    pub fn clear_selection(&mut self) -> bool {
        self.selecting = false;
        let had = self.sel_range().is_some() || self.page_sel_range().is_some();
        self.sel_anchor = None;
        self.sel_head = None;
        self.page_sel_anchor = None;
        self.page_sel_head = None;
        had
    }

    /// Selects every text block; returns whether the view has any text.
    pub fn select_all(&mut self) -> bool {
        let mut first = None;
        let mut last = None;
        for (i, block) in self.blocks.iter().enumerate() {
            if let Block::Text { buffer, .. } | Block::Math { text: buffer, .. } = block {
                if first.is_none() {
                    first = Some((i, Cursor::new(0, 0)));
                }
                let line = buffer.lines.len().saturating_sub(1);
                let index = buffer.lines.last().map_or(0, |l| l.text().len());
                last = Some((i, Cursor::new(line, index)));
            }
        }
        self.selecting = false;
        self.sel_anchor = first;
        self.sel_head = last;
        first.is_some()
    }

    /// The selected text: a drag over flowed text, or over the glyphs of a
    /// rendered PDF page. None when nothing is selected.
    pub fn selected_text(&self) -> Option<String> {
        self.flowed_selected_text().or_else(|| self.page_selected_text())
    }

    /// The selection across text blocks, separated by blank lines to match
    /// their visual separation.
    fn flowed_selected_text(&self) -> Option<String> {
        let ((sb, sc), (eb, ec)) = self.sel_range()?;
        let mut parts: Vec<String> = Vec::new();
        for (i, block) in self.blocks.iter().enumerate().take(eb + 1).skip(sb) {
            let (Block::Text { buffer, .. } | Block::Math { text: buffer, .. }) = block else {
                continue;
            };
            let start = if i == sb { sc } else { Cursor::new(0, 0) };
            let end = if i == eb { ec } else { Cursor::new(usize::MAX, usize::MAX) };
            let last_line = buffer.lines.len().saturating_sub(1);
            let lo = start.line.min(last_line);
            let hi = end.line.min(last_line);
            let mut out = String::new();
            for li in lo..=hi {
                let text = buffer.lines[li].text();
                let s = if li == start.line { start.index.min(text.len()) } else { 0 };
                let e = if li == end.line { end.index.min(text.len()) } else { text.len() };
                if li > lo {
                    out.push('\n');
                }
                out.push_str(&text[s.min(e)..e]);
            }
            parts.push(out);
        }
        let joined = parts.join("\n\n");
        (!joined.trim().is_empty()).then_some(joined)
    }

    /// Whether Cmd+C / the right-click Copy would produce anything: a live
    /// selection, or a page-rendered PDF whose text can be extracted whole.
    pub fn can_copy(&self) -> bool {
        self.sel_range().is_some()
            || self.page_sel_range().is_some()
            || (self.kind == ViewKind::Pdf && self.worker.is_some())
    }

    /// Clipboard text for a page-rendered PDF, whose bitmaps carry no
    /// selectable glyphs: the whole document's extracted text, cached after
    /// the first request. None for other views or when nothing is extractable.
    pub fn whole_document_text(&mut self) -> Option<String> {
        if self.kind != ViewKind::Pdf || self.worker.is_none() {
            return None;
        }
        if self.pdf_text.is_none() {
            self.pdf_text = Some(pdf_extract::extract_text(&self.path).unwrap_or_default());
        }
        self.pdf_text.clone().filter(|t| !t.trim().is_empty())
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
        matches!(self.kind, ViewKind::Image | ViewKind::Tex) || self.worker.is_some()
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
        if self.kind == ViewKind::Tex {
            // A bigger page means bigger type, so the document is set again.
            self.rebuild_tex(font_system);
        } else {
            self.reflow(font_system);
            self.scroll *= ratio;
        }
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
        if self.kind == ViewKind::Tex {
            self.rebuild_tex(font_system);
        } else {
            self.reflow(font_system);
            self.scroll *= ratio;
        }
        self.clamp_scroll();
    }

    /// Collects finished page bitmaps from the worker, requests the pages at
    /// (and adjacent to) the viewport at the current width/zoom, and drops
    /// far-away cached pages to bound memory. Never blocks: a page draws as a
    /// placeholder until its bitmap arrives and the worker triggers a repaint.
    fn prepare_pages(&mut self, view_h: f32) {
        let Some(worker) = self.worker.as_ref() else { return };
        for res in worker.res_rx.try_iter() {
            match res {
                PdfRes::Page(index, width, img) => {
                    if self.pending.get(&index) == Some(&width) {
                        self.pending.remove(&index);
                    }
                    self.page_cache.insert(index, img);
                }
                PdfRes::Text(index, text) => {
                    self.pending_text.remove(&index);
                    self.page_text.insert(index, text);
                }
            }
        }
        let target_w = self.column_width() * self.zoom;
        // (page index, size, intersects viewport) in document order.
        let mut pages: Vec<(usize, (f32, f32), bool)> = Vec::new();
        for frag in &self.places {
            if let Block::Page { index, size, .. } = &self.blocks[frag.block] {
                let y = frag.y - self.scroll;
                pages.push((*index, *size, y + frag.h >= 0.0 && y <= view_h));
            }
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
            if worker.req_tx.send(PdfReq::Render(index, want)).is_ok() {
                self.pending.insert(index, want);
            }
        }
        // Selectable text for the same window, so a drag finds glyph boxes
        // already in hand. The worker only gets to these once no page is
        // waiting to be rasterized.
        for &index in &keep {
            if self.page_text.contains_key(&index) || self.pending_text.contains(&index) {
                continue;
            }
            if worker.req_tx.send(PdfReq::Text(index)).is_ok() {
                self.pending_text.insert(index);
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
        let zoom = self.zoom;
        let scroll = self.scroll;
        let places = std::mem::take(&mut self.places);
        for frag in &places {
            let idx = frag.block;
            let Block::Picture { scaled, size, fill, height } = &mut self.blocks[idx] else {
                continue;
            };
            if let Some(img) = done.remove(&idx) {
                *scaled = img;
            }
            let (dw, dh) = fit_draw(size.0, size.1, picture_target_w(*size, *fill, frag.w, zoom));
            let want = (dw as u32, dh as u32);
            let y = frag.y - scroll;
            let visible = y + *height >= 0.0 && y <= view_h;
            if visible
                && (scaled.width(), scaled.height()) != want
                && self.pending_imgs.get(&idx) != Some(&want)
                && worker.req_tx.send((idx, want.0, want.1)).is_ok()
            {
                self.pending_imgs.insert(idx, want);
            }
        }
        self.places = places;
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
        if self.paper {
            self.paint_sheets(&mut canvas, font_system, swash_cache, h as f32);
        }

        let scroll_x = self.scroll_x;
        let fg = self.fg();
        let default_color = Color::rgb(fg[0], fg[1], fg[2]);
        let page_cache = &self.page_cache;
        let page_text = &self.page_text;
        let sel = self.sel_range();
        let page_sel = self.page_sel_range();
        let sel_color = Color::rgba(self.accent[0], self.accent[1], self.accent[2], 80);
        let places = std::mem::take(&mut self.places);
        for frag in &places {
            let block_i = frag.block;
            let y = frag.y - self.scroll;
            let text_w = frag.w;
            if y + frag.h < 0.0 || y > h as f32 {
                continue;
            }
            let margin = frag.x - scroll_x;
            match &mut self.blocks[block_i] {
                Block::Text { buffer, indent, inset_r, bg: block_bg, .. } => {
                    let x0 = (margin + *indent) as i32;
                    let pad = if block_bg.is_some() { (self.font_px / 2.0) as i32 } else { 0 };
                    if let Some(color) = block_bg {
                        canvas.fill_rect(
                            x0 - pad,
                            y as i32,
                            (text_w - *indent - *inset_r) as i32 + 2 * pad,
                            frag.h as i32,
                            *color,
                        );
                    }
                    let oy = y as i32 + pad;
                    // Selection highlight behind the glyphs.
                    if let Some(((sb, sc), (eb, ec))) = sel {
                        if block_i >= sb && block_i <= eb {
                            let s = if block_i == sb { sc } else { Cursor::new(0, 0) };
                            let e = if block_i == eb {
                                ec
                            } else {
                                Cursor::new(usize::MAX, usize::MAX)
                            };
                            for run in buffer.layout_runs() {
                                if run.line_top < frag.from - 0.5 || run.line_top >= frag.to - 0.5 {
                                    continue;
                                }
                                for (hx, hw) in run.highlight(s, e) {
                                    canvas.blend_rect(
                                        x0 + hx as i32,
                                        oy + (run.line_top - frag.from) as i32,
                                        hw.ceil() as i32,
                                        run.line_height.ceil() as i32,
                                        sel_color,
                                    );
                                }
                            }
                        }
                    }
                    draw_buffer_slice(
                        buffer,
                        font_system,
                        swash_cache,
                        &mut canvas,
                        default_color,
                        x0,
                        oy,
                        frag.from,
                        frag.to,
                    );
                }
                Block::Picture { scaled, size, fill, height } => {
                    let draw_w = fit_draw(
                        size.0,
                        size.1,
                        picture_target_w(*size, *fill, text_w, self.zoom),
                    )
                    .0 as i32;
                    // A float is centered in its column on paper.
                    let indent =
                        if self.paper { ((text_w - draw_w as f32) / 2.0).max(0.0) } else { 0.0 };
                    let x0 = (margin + indent) as i32;
                    if scaled.width() == draw_w as u32 && scaled.height() == *height as u32 {
                        blit_image(&mut canvas, scaled, x0, y as i32);
                    } else if scaled.width() > 0 {
                        // Decode/rescale still in flight: nearest preview.
                        blit_scaled(&mut canvas, scaled, x0, y as i32, draw_w, *height as i32);
                    } else {
                        // Not decoded yet: a light frame on paper, so the
                        // page does not flash a dark hole while it loads.
                        let wait =
                            if self.paper { PAPER_CODE_BG } else { self.theme.ui.panel_hover };
                        canvas.fill_rect(x0, y as i32, draw_w, *height as i32, wait);
                    }
                }
                Block::Page { index, size, height } => {
                    {
                        let x0 = margin as i32;
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
                        // Selection over the page's glyphs.
                        if let Some(((start_page, start), (end_page, end))) = page_sel {
                            if *index >= start_page && *index <= end_page {
                                if let Some(layer) = page_text.get(index) {
                                    let lo = if *index == start_page { start } else { 0 };
                                    let hi = if *index == end_page { end } else { layer.chars.len() };
                                    let scale = draw_w as f32 / size.0.max(1.0);
                                    for c in layer.chars.get(lo..hi.min(layer.chars.len())).unwrap_or(&[])
                                    {
                                        canvas.blend_rect(
                                            x0 + (c.x * scale) as i32,
                                            y as i32 + (c.y * scale) as i32,
                                            (c.w * scale).ceil() as i32,
                                            (c.h * scale).ceil() as i32,
                                            sel_color,
                                        );
                                    }
                                }
                            }
                        }
                        // Hairline frame so white pages read as pages on
                        // light backgrounds too.
                        let ph = *height as i32;
                        canvas.fill_rect(x0 - 1, y as i32 - 1, draw_w + 2, 1, dim);
                        canvas.fill_rect(x0 - 1, y as i32 + ph, draw_w + 2, 1, dim);
                        canvas.fill_rect(x0 - 1, y as i32, 1, ph, dim);
                        canvas.fill_rect(x0 + draw_w, y as i32, 1, ph, dim);
                    }
                }
                Block::Rule => {
                    let ry = (y + self.font_px / 2.0) as i32;
                    let dim = if self.paper { PAPER_RULE } else { colors::base_palette(self.theme)[8] };
                    canvas.fill_rect(margin as i32, ry, text_w as i32, 1, dim);
                }
                Block::Math { bx, number, height, .. } => {
                    {
                        // Display equations are centered in the column, with
                        // the number set flush right like LaTeX does.
                        let x0 = margin + ((text_w - bx.width) / 2.0).max(0.0);
                        for item in &mut bx.items {
                            match item {
                                MathItem::Run { buffer, x, y: iy } => {
                                    let (ox, oy) = ((x0 + *x) as i32, (y + *iy) as i32);
                                    buffer.draw(
                                        font_system,
                                        swash_cache,
                                        default_color,
                                        |px, py, pw, ph, color| {
                                            canvas.blend_rect(px + ox, py + oy, pw as i32, ph as i32, color);
                                        },
                                    );
                                }
                                MathItem::Rule { x, y: iy, w, h: rh } => {
                                    canvas.fill_rect(
                                        (x0 + *x) as i32,
                                        (y + *iy) as i32,
                                        w.ceil() as i32,
                                        rh.ceil().max(1.0) as i32,
                                        fg,
                                    );
                                }
                            }
                        }
                        if let Some(num) = number {
                            let nx = margin + text_w - num.width;
                            let ny = y + (*height - num.height()) / 2.0;
                            for item in &mut num.items {
                                if let MathItem::Run { buffer, x, y: iy } = item {
                                    let (ox, oy) = ((nx + *x) as i32, (ny + *iy) as i32);
                                    buffer.draw(
                                        font_system,
                                        swash_cache,
                                        default_color,
                                        |px, py, pw, ph, color| {
                                            canvas.blend_rect(px + ox, py + oy, pw as i32, ph as i32, color);
                                        },
                                    );
                                }
                            }
                        }
                    }
                }
                Block::Table { rows, font_px, col_widths, row_heights, height } => {
                    if !col_widths.is_empty() {
                        let paper = self.paper;
                        let dim = if paper { PAPER_RULE } else { colors::base_palette(self.theme)[8] };
                        // On paper a table is set booktabs style: rules above
                        // the head, under it and at the foot, and nothing
                        // else — no grid, no shading.
                        let rule = if paper { PAPER_INK } else { dim };
                        let head_bg = if paper { PAPER_BG } else { self.theme.ui.panel_hover };
                        let pad_h = (*font_px * 0.5).round();
                        let pad_v = (*font_px * 0.3).round();
                        let table_w = (col_widths.iter().map(|w| w + 2.0 * pad_h).sum::<f32>()
                            + (col_widths.len() + 1) as f32) as i32;
                        let thick = if paper { (*font_px / 9.0).round().max(1.0) as i32 } else { 1 };
                        let nrows = rows.len();
                        canvas.fill_rect(margin as i32, y as i32, table_w, thick, rule);
                        let mut row_y = y + thick as f32;
                        for (ri, row) in rows.iter_mut().enumerate() {
                            let row_h = row_heights[ri];
                            if ri == 0 && !paper {
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
                                draw_buffer_slice(
                                    buffer,
                                    font_system,
                                    swash_cache,
                                    &mut canvas,
                                    default_color,
                                    ox,
                                    oy,
                                    0.0,
                                    f32::MAX,
                                );
                                cell_x += col_widths[ci] + 2.0 * pad_h + 1.0;
                            }
                            row_y += row_h;
                            let last = ri + 1 == nrows;
                            if !paper || ri == 0 || last {
                                let t = if paper && last { thick } else { 1 };
                                canvas.fill_rect(margin as i32, row_y as i32, table_w, t, rule);
                            }
                            row_y += 1.0;
                        }
                        if !paper {
                            let mut line_x = margin;
                            canvas.fill_rect(line_x as i32, y as i32, 1, *height as i32, dim);
                            for w in col_widths.iter() {
                                line_x += w + 2.0 * pad_h + 1.0;
                                canvas.fill_rect(line_x as i32, y as i32, 1, *height as i32, dim);
                            }
                        }
                    }
                }
            }
        }
        self.places = places;
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

/// Collects positioned characters from a page's content stream. Everything
/// but `draw_glyph` is ignored, so this walks the page far more cheaply than
/// rasterizing it — no paths, images or blending are ever evaluated.
struct TextExtractor {
    chars: Vec<PageChar>,
}

impl hayro::hayro_interpret::Device<'_> for TextExtractor {
    fn set_soft_mask(&mut self, _: Option<hayro::hayro_interpret::SoftMask<'_>>) {}

    fn set_blend_mode(&mut self, _: hayro::hayro_interpret::BlendMode) {}

    fn draw_path(
        &mut self,
        _: &kurbo::BezPath,
        _: kurbo::Affine,
        _: &hayro::hayro_interpret::Paint<'_>,
        _: &hayro::hayro_interpret::PathDrawMode,
    ) {
    }

    fn push_clip_path(&mut self, _: &hayro::hayro_interpret::ClipPath) {}

    fn push_transparency_group(
        &mut self,
        _: f32,
        _: Option<hayro::hayro_interpret::SoftMask<'_>>,
        _: hayro::hayro_interpret::BlendMode,
    ) {
    }

    fn draw_glyph(
        &mut self,
        glyph: &hayro::hayro_interpret::font::Glyph<'_>,
        transform: kurbo::Affine,
        glyph_transform: kurbo::Affine,
        _: &hayro::hayro_interpret::Paint<'_>,
        _: &hayro::hayro_interpret::GlyphDrawMode,
    ) {
        use hayro::hayro_interpret::font::Glyph;
        use hayro::hayro_interpret::hayro_cmap::BfString;

        let Some(unicode) = glyph.as_unicode() else { return };
        let text = match unicode {
            BfString::Char(c) => c.to_string(),
            BfString::String(s) => s,
        };
        if text.is_empty() {
            return;
        }
        // `transform` already carries the page's initial transform, so this
        // lands in the same top-left-origin space the renderer draws into.
        let to_page = transform * glyph_transform;
        // Outlines use a 1000-unit em, so the advance and the em top are
        // measured there and land in page points after the transform.
        let advance = match glyph {
            Glyph::Outline(outline) => outline.advance_width().unwrap_or(500.0) as f64,
            Glyph::Type3(_) => 500.0,
        };
        let origin = to_page * kurbo::Point::new(0.0, 0.0);
        let end = to_page * kurbo::Point::new(advance, 0.0);
        let em_top = to_page * kurbo::Point::new(0.0, 1000.0);
        let h = ((origin.y - em_top.y).abs() as f32).max(1.0);
        let w = (end.x - origin.x).abs() as f32;
        self.chars.push(PageChar {
            text,
            x: origin.x.min(end.x) as f32,
            // The em box sits mostly above the baseline; keeping a fifth of it
            // below leaves room for descenders inside the selection box.
            y: origin.y as f32 - h * 0.8,
            w: if w > 0.0 { w } else { h * 0.5 },
            h,
            baseline: origin.y as f32,
            line: 0,
        });
    }

    fn draw_image(&mut self, _: hayro::hayro_interpret::Image<'_, '_>, _: kurbo::Affine) {}

    fn pop_clip_path(&mut self) {}

    fn pop_transparency_group(&mut self) {}
}

/// Extracts one page's selectable text by interpreting its content stream
/// with a glyph-only device.
fn extract_page_text<'a>(
    pdf: &'a hayro::hayro_syntax::Pdf,
    index: usize,
    cache: &hayro::hayro_interpret::InterpreterCache<'a>,
) -> Option<PageText> {
    use hayro::hayro_interpret::TransformExt;

    let page = pdf.pages().get(index)?;
    let (page_w, page_h) = page.render_dimensions();
    // The very transform the renderer starts from, at scale 1: it folds in the
    // crop box and page rotation and flips y, so the boxes land in page points
    // with a top-left origin — exactly the space the page bitmap is drawn in.
    let mut context = hayro::hayro_interpret::Context::new(
        page.initial_transform(true).to_kurbo(),
        kurbo::Rect::new(0.0, 0.0, page_w as f64, page_h as f64),
        cache,
        pdf.xref(),
        hayro::hayro_interpret::InterpreterSettings::default(),
    );
    let mut extractor = TextExtractor { chars: Vec::new() };
    hayro::hayro_interpret::interpret_page(page, &mut context, &mut extractor);
    Some(PageText::from_chars(extractor.chars, page_w))
}

/// Renders one PDF page to a bitmap `draw_w` pixels wide. The white opaque
/// background makes hayro's premultiplied output plain RGBA.
/// Rasterizes the first page of a PDF used as a figure. LaTeX papers keep
/// their plots as vector PDFs, which the image crate cannot decode; hayro is
/// already here for the PDF viewer, so reuse it.
fn rasterize_pdf_figure(path: &Path, draw_w: u32) -> Option<(RgbaImage, (f32, f32))> {
    let bytes = std::fs::read(path).ok()?;
    let pdf = hayro::hayro_syntax::Pdf::new(bytes).ok()?;
    let pages = pdf.pages();
    let size = pages.first()?.render_dimensions();
    let cache = hayro::RenderCache::new();
    let img = rasterize_page(pages, 0, draw_w, &cache)?;
    Some((img, size))
}

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

    /// A letter-size page with two text columns sharing baselines, the layout
    /// that trips up a naive top-to-bottom sort.
    fn two_column_pdf() -> Vec<u8> {
        let mut stream = String::new();
        for row in 0..12 {
            let y = 700 - row * 20;
            stream.push_str(&format!("BT /F1 10 Tf 50 {y} Td (Left{row}) Tj ET\n"));
            stream.push_str(&format!("BT /F1 10 Tf 320 {y} Td (Right{row}) Tj ET\n"));
        }
        let objects = [
            "<</Type/Catalog/Pages 2 0 R>>".to_string(),
            "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>".to_string(),
            format!("<</Length {}>>stream\n{stream}\nendstream", stream.len() + 1),
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

    /// Text must come out column by column. Sorting purely by baseline would
    /// zig-zag across the gutter and interleave the two columns, which is what
    /// makes copied text from a paper unusable.
    #[test]
    fn two_column_pages_extract_one_column_at_a_time() {
        let pdf = hayro::hayro_syntax::Pdf::new(two_column_pdf()).expect("parse");
        let cache = hayro::hayro_interpret::InterpreterCache::new();
        let text = extract_page_text(&pdf, 0, &cache).expect("page 0");

        let mut lines: Vec<String> = Vec::new();
        for c in &text.chars {
            if lines.len() as u32 != c.line + 1 {
                lines.push(String::new());
            }
            lines.last_mut().unwrap().push_str(&c.text);
        }
        let expected: Vec<String> = (0..12)
            .map(|i| format!("Left{i}"))
            .chain((0..12).map(|i| format!("Right{i}")))
            .collect();
        assert_eq!(lines, expected, "left column should read out fully before the right");
    }

    #[test]
    fn tex_renders_formatted_blocks() {
        let path = std::env::temp_dir().join("tigriden-viewer-tex-test.tex");
        std::fs::write(
            &path,
            "\\documentclass{article}\n\\title{Sample Paper}\n\\author{Ada}\n\\begin{document}\n\\maketitle\n\\section{Intro}\nEnergy is $E = mc^2$ here.\n\\begin{itemize}\n\\item first point\n\\end{itemize}\n\\begin{equation}\n\\alpha + \\beta\n\\end{equation}\n\\end{document}\n",
        )
        .unwrap();

        let mut font_system = FontSystem::new();
        let theme = crate::theme::default_theme();
        let mut viewer = ViewerState::open(
            &mut font_system,
            &path,
            ViewKind::Tex,
            "Menlo",
            13.0,
            theme,
            [0, 0, 0],
            400.0,
            std::sync::Arc::new(|| {}),
        )
        .expect("viewer opens the tex file");

        assert!(viewer.select_all(), "tex view has selectable text");
        let all = viewer.selected_text().expect("select-all yields text");
        assert!(all.contains("Sample Paper"), "title from \\maketitle: {all}");
        assert!(all.contains("1  Intro"), "numbered section heading: {all}");
        assert!(all.contains("E = mc2"), "inline math, with the 2 as a script run: {all}");
        assert!(all.contains("first point"), "list item text: {all}");
        assert!(all.contains("α + β"), "display math as unicode: {all}");
        // The preamble itself must not leak into the formatted view.
        assert!(!all.contains("documentclass"), "preamble hidden: {all}");

        let _ = std::fs::remove_file(&path);
    }

    /// A LaTeX document is set on the page its class asks for: the sheet has
    /// the paper's proportions, a two-column class fills two columns per
    /// sheet, the title block spans them, and the text runs onto further
    /// sheets rather than one endless strip.
    #[test]
    fn tex_paginates_onto_the_class_page() {
        let path = std::env::temp_dir().join("tigriden-viewer-tex-page-test.tex");
        let body = "Filler sentence for the column. ".repeat(400);
        std::fs::write(
            &path,
            format!(
                "\\documentclass[journal]{{IEEEtran}}\n\\title{{Wide Title}}\n\\author{{Ada}}\n                 \\begin{{document}}\n\\maketitle\n\\section{{Intro}}\n{body}\n\\end{{document}}\n"
            ),
        )
        .unwrap();

        let mut font_system = FontSystem::new();
        let theme = crate::theme::default_theme();
        let viewer = ViewerState::open(
            &mut font_system,
            &path,
            ViewKind::Tex,
            "Menlo",
            13.0,
            theme,
            [0, 0, 0],
            1200.0,
            std::sync::Arc::new(|| {}),
        )
        .expect("viewer opens the tex file");

        let geom = viewer.page_geom.expect("a LaTeX view is paginated");
        assert_eq!(geom.columns, 2, "IEEEtran is a two-column class");
        let ratio = geom.h / geom.w;
        assert!((ratio - 11.0 / 8.5).abs() < 0.02, "letter proportions, got {ratio}");
        assert!(viewer.sheets.len() > 1, "the filler must run past one page");
        assert!(
            viewer.content_h > viewer.sheets.len() as f32 * geom.h,
            "every sheet is on the strip",
        );

        // The title spans the page; the body sits in two columns under it.
        let title = viewer.places[0];
        assert!(title.w > geom.column_w() * 1.9, "the title block spans both columns");
        let mut lefts: Vec<i32> = viewer.places.iter().map(|f| f.x as i32).collect();
        lefts.sort_unstable();
        lefts.dedup();
        assert_eq!(lefts.len(), 2, "one x for each column, got {lefts:?}");
        for frag in &viewer.places {
            let sheet = viewer
                .sheets
                .iter()
                .find(|(top, h)| frag.y >= *top && frag.y < top + h)
                .expect("every fragment sits on a sheet");
            assert!(
                frag.y + frag.h <= sheet.0 + sheet.1 + 1.0,
                "a fragment must not hang off the bottom of its sheet",
            );
        }

        let _ = std::fs::remove_file(&path);
    }

    /// Markdown math has to reach the same box layout the LaTeX view uses:
    /// display equations become their own `Block::Math`, `\tag{…}` supplies the
    /// number, and a `$` in a code span stays a dollar sign.
    #[test]
    fn markdown_math_is_typeset_like_latex() {
        let path = std::env::temp_dir().join("tigriden-viewer-mdmath-test.md");
        std::fs::write(
            &path,
            concat!(
                "# Metrics\n\n",
                "The overlap of $b$ and $b^{gt}$ is:\n\n",
                r"$$\mathrm{IoU}(b, b^{gt}) = \frac{|b \cap b^{gt}|}{|b \cup b^{gt}|} \tag{1}$$",
                "\n\n",
                "Run `echo $PATH` to check, and it costs $5 today.\n\n",
                "```math\n",
                r"\alpha + \beta",
                "\n```\n",
            ),
        )
        .unwrap();

        let mut font_system = FontSystem::new();
        let theme = crate::theme::default_theme();
        let mut viewer = ViewerState::open(
            &mut font_system,
            &path,
            ViewKind::Markdown,
            "Menlo",
            13.0,
            theme,
            [0, 0, 0],
            400.0,
            std::sync::Arc::new(|| {}),
        )
        .expect("viewer opens the markdown");

        let math: Vec<&Block> =
            viewer.blocks.iter().filter(|b| matches!(b, Block::Math { .. })).collect();
        assert_eq!(math.len(), 2, "both $$…$$ and ```math become laid-out blocks");
        // \tag{1} is set flush right as the equation number, not left in the body.
        let Block::Math { number, .. } = math[0] else { unreachable!() };
        assert!(number.is_some(), "\\tag{{1}} becomes the equation number");

        assert!(viewer.select_all(), "markdown view has selectable text");
        let all = viewer.selected_text().expect("select-all yields text");
        assert!(all.contains("IoU"), "the formula is selectable: {all}");
        assert!(!all.contains("\\frac"), "the formula is typeset, not dumped: {all}");
        assert!(!all.contains("\\tag"), "the tag is consumed, not printed: {all}");
        // A dollar inside a code span, and a lone dollar, are money not math.
        assert!(all.contains("echo $PATH"), "code span keeps its dollar: {all}");
        assert!(all.contains("costs $5 today"), "a lone dollar stays literal: {all}");
        assert!(all.contains("α + β"), "```math block is typeset: {all}");

        let _ = std::fs::remove_file(&path);
    }



    #[test]
    fn markdown_selection_copies_text() {
        let path = std::env::temp_dir().join("tigriden-viewer-sel-test.md");
        std::fs::write(
            &path,
            "# Title\n\nHello viewer selection world.\n\nSecond paragraph here.\n",
        )
        .unwrap();

        let mut font_system = FontSystem::new();
        let theme = crate::theme::default_theme();
        let mut viewer = ViewerState::open(
            &mut font_system,
            &path,
            ViewKind::Markdown,
            "Menlo",
            13.0,
            theme,
            [0, 0, 0],
            400.0,
            std::sync::Arc::new(|| {}),
        )
        .expect("viewer opens the markdown");

        // Select-all captures every block in document order.
        assert!(viewer.select_all(), "markdown view has selectable text");
        let all = viewer.selected_text().expect("select-all yields text");
        assert!(all.contains("Title"));
        assert!(all.contains("Hello viewer selection world."));
        assert!(all.contains("Second paragraph here."));
        assert!(all.find("Title").unwrap() < all.find("Second").unwrap());

        assert!(viewer.clear_selection(), "clear drops the selection");
        assert!(viewer.selected_text().is_none());

        // A drag from inside the heading to far past the end selects through
        // the last paragraph; the release ends the drag.
        viewer.handle_mouse(0, viewer.margin + 1.0, viewer.margin + 1.0, 300.0);
        assert!(viewer.handle_mouse(2, 10_000.0, 10_000.0, 300.0), "drag extends the selection");
        viewer.handle_mouse(1, 10_000.0, 10_000.0, 300.0);
        let dragged = viewer.selected_text().expect("dragged selection yields text");
        assert!(dragged.contains("Second paragraph here."));

        // Double-click selects the word under the pointer. The drag above ran
        // off the bottom of the viewport, which scrolled the page, so come back
        // to the top first — otherwise the click lands on the last paragraph.
        viewer.scroll = 0.0;
        assert!(viewer.handle_mouse(3, viewer.margin + 2.0, viewer.margin + 2.0, 300.0));
        assert_eq!(viewer.selected_text().as_deref(), Some("Title"));

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

        // Copy with nothing selected falls back to the whole document.
        assert!(viewer.selected_text().is_none());
        let text = viewer.whole_document_text().expect("pdf text extraction");
        assert!(text.contains("Hello PDF"), "extracted text: {text:?}");

        // The text layer arrives from the same worker as the bitmaps, so poll
        // for it, then drag across the page and copy just what was dragged.
        viewer.zoom_reset(&mut font_system);
        let mut glyphs: Vec<(String, f32, f32, f32, f32)> = Vec::new();
        for _ in 0..200 {
            viewer.render(&mut font_system, &mut swash_cache, 800, 600);
            if let Some(layer) = viewer.page_text.get(&0).filter(|t| !t.chars.is_empty()) {
                glyphs = layer
                    .chars
                    .iter()
                    .map(|c| (c.text.clone(), c.x, c.y, c.w, c.h))
                    .collect();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(!glyphs.is_empty(), "page text layer should arrive from the worker");
        let extracted: String = glyphs.iter().map(|(t, ..)| t.as_str()).collect();
        assert_eq!(extracted, "Hello PDF", "glyphs extract in reading order");
        // Every glyph must sit inside the page box, or highlights would be
        // drawn in the wrong place.
        let (page_w, page_h) = (200.0, 100.0);
        for (text, x, y, w, h) in &glyphs {
            assert!(
                *x >= 0.0 && x + w <= page_w && *y >= 0.0 && y + h <= page_h,
                "glyph {text:?} box ({x}, {y}, {w}, {h}) escapes the {page_w}x{page_h} page"
            );
        }

        // A drag across the whole line selects it; the text comes back with a
        // space where the PDF left a gap instead of a space glyph.
        viewer.handle_mouse(0, 0.0, 0.0, 600.0);
        viewer.handle_mouse(2, 800.0, 600.0, 600.0);
        viewer.handle_mouse(1, 800.0, 600.0, 600.0);
        assert_eq!(viewer.selected_text().as_deref(), Some("Hello PDF"));
        assert!(viewer.can_copy());

        // Double-click selects the word under the pointer, not the whole line.
        viewer.clear_selection();
        let (_, fx, fy, fw, fh) = glyphs[0];
        let scale = (800.0 - 2.0 * viewer.margin) / page_w;
        let (cx, cy) =
            (viewer.margin + (fx + fw / 2.0) * scale, viewer.margin + (fy + fh / 2.0) * scale);
        assert!(viewer.handle_mouse(3, cx, cy, 600.0), "double-click selects a word");
        assert_eq!(viewer.selected_text().as_deref(), Some("Hello"));

        let _ = std::fs::remove_file(&path);
    }

    /// Dev aid, not a regression test: renders a real `.tex` through the
    /// viewer and writes the scrolled sheet out as PNGs, so the LaTeX layout
    /// can be eyeballed without launching the app.
    ///
    ///   TEX_DUMP=/path/paper.tex TEX_DUMP_OUT=/tmp/tex cargo test tex_sheet_dump -- --nocapture
    #[test]
    fn tex_sheet_dump() {
        let Ok(src) = std::env::var("TEX_DUMP") else { return };
        let out = std::env::var("TEX_DUMP_OUT").unwrap_or_else(|_| "/tmp/texdump".into());
        let pages: usize = std::env::var("TEX_DUMP_PAGES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4);
        let w: u32 = std::env::var("TEX_DUMP_W").ok().and_then(|v| v.parse().ok()).unwrap_or(1700);
        let h = 2100u32;
        std::fs::create_dir_all(&out).unwrap();

        let mut font_system = FontSystem::new();
        let mut swash_cache = SwashCache::new();
        let theme = crate::theme::default_theme();
        let mut viewer = ViewerState::open(
            &mut font_system,
            Path::new(&src),
            ViewKind::Tex,
            "Menlo",
            46.0,
            theme,
            [70, 120, 200],
            w as f32,
            std::sync::Arc::new(|| {}),
        )
        .expect("viewer opens the tex");
        println!("blocks={} content_h={}", viewer.blocks.len(), viewer.content_h);
        let from: usize = std::env::var("TEX_DUMP_FROM").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        let sheets = viewer.sheets.clone();
        println!("sheets={}", sheets.len());
        for i in from..(from + pages).min(sheets.len().max(from + pages)) {
            let (top, sheet_h) = sheets.get(i).copied().unwrap_or((i as f32 * h as f32, h as f32));
            let h = (sheet_h + 2.0 * PAGE_GAP) as u32;
            viewer.scroll = top - PAGE_GAP;
            viewer.clamp_scroll();
            // Figures decode on the worker thread; give them a moment.
            for _ in 0..25 {
                let _ = viewer.render(&mut font_system, &mut swash_cache, w, h);
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            let frame = viewer.render(&mut font_system, &mut swash_cache, w, h);
            let mut img = RgbaImage::new(w, h);
            for (i, p) in frame.as_slice().iter().enumerate() {
                let (x, y) = ((i as u32) % w, (i as u32) / w);
                img.put_pixel(x, y, image::Rgba([p.r, p.g, p.b, 255]));
            }
            let path = std::path::Path::new(&out).join(format!("sheet{i}.png"));
            img.save(&path).unwrap();
            println!("wrote {}", path.display());
        }
    }
}
/// Metadata tags the LaTeX builder puts on spans so the painter can raise or
/// lower them off the baseline: a plain text buffer has no notion of scripts,
/// and inline math is full of them.
const SCRIPT_NONE: usize = 0;
const SCRIPT_SUB: usize = 1;
const SCRIPT_SUP: usize = 2;

/// Draws the part of `buffer` between the buffer-space heights `from` and
/// `to` at (`x`, `y`), which is how a paragraph continues in the next column.
/// Glyphs tagged as scripts are shifted off the baseline; everything else is
/// drawn exactly where the layout put it.
fn draw_buffer_slice(
    buffer: &mut Buffer,
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    canvas: &mut Canvas,
    color: Color,
    x: i32,
    y: i32,
    from: f32,
    to: f32,
) {
    buffer.shape_until_scroll(font_system, false);
    for run in buffer.layout_runs() {
        if run.line_top < from - 0.5 || run.line_top >= to - 0.5 {
            continue;
        }
        let line_y = run.line_y - from;
        for glyph in run.glyphs {
            let shift = match glyph.metadata {
                SCRIPT_SUB => glyph.font_size * 0.16,
                SCRIPT_SUP => -glyph.font_size * 0.34,
                _ => 0.0,
            };
            let physical = glyph.physical((0.0, line_y + shift), 1.0);
            let glyph_color = glyph.color_opt.unwrap_or(color);
            swash_cache.with_pixels(font_system, physical.cache_key, glyph_color, |px, py, c| {
                canvas.blend_rect(x + physical.x + px, y + physical.y + py, 1, 1, c);
            });
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
        "pdf" => Some(ViewKind::Pdf),
        "tex" | "latex" | "ltx" => Some(ViewKind::Tex),
        _ => None,
    }
}

