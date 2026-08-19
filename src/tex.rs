//! Lightweight LaTeX-to-blocks converter for the read-only viewer.
//!
//! This is a best-effort formatter, not a TeX engine: it understands the
//! document constructs that show up in everyday papers and notes (sectioning,
//! text styles, lists, tabular, verbatim, math with a Unicode translation)
//! and degrades gracefully on everything else by showing the plain text.

use std::collections::HashMap;

/// A run of styled text within a paragraph, heading, list item or table cell.
#[derive(Clone, PartialEq, Debug)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// Monospace (\texttt, \verb).
    pub mono: bool,
    /// Accent-colored (\href, \url, \cite, \ref).
    pub link: bool,
    /// 1 for a superscript, -1 for a subscript, 0 for text on the baseline.
    /// Inline math and \textsuperscript set it; the viewer draws those runs
    /// small and off the baseline.
    pub script: i8,
    /// Type size as a multiple of the body size: what `\large`, `\small` and
    /// the rest of TeX's size commands select. 1.0 is `\normalsize`.
    pub size: f32,
}

impl Default for Span {
    fn default() -> Self {
        Span {
            text: String::new(),
            bold: false,
            italic: false,
            mono: false,
            link: false,
            script: 0,
            size: 1.0,
        }
    }
}

/// What kind of float a block came out of, which decides how the page
/// builder may move it.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct FloatInfo {
    /// Inside a starred float: it spans every column.
    pub wide: bool,
    /// A float TeX is free to move to the top of a later page — anything but
    /// the `[H]` of the float package, which pins it where it is written.
    pub movable: bool,
}

/// How a paragraph sets in its column.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum TexAlign {
    #[default]
    Justify,
    Center,
    Left,
    Right,
}

#[derive(PartialEq, Debug)]
pub enum TexBlock {
    /// A paragraph of running text. `inset` marks the quoted blocks — an
    /// abstract, a `quote` — that TeX pulls in from both margins.
    Paragraph { spans: Vec<Span>, align: TexAlign, inset: bool },
    /// level 0 = \part/\chapter, 1 = \section, 2 = \subsection, 3 = deeper.
    Heading { level: u8, spans: Vec<Span> },
    /// Verbatim / lstlisting / minted content, monospace on a panel.
    Code(String),
    /// Display math as a layout tree, with the equation number TeX would
    /// print at the right margin (None for starred/unnumbered forms).
    Math { node: MathNode, number: Option<String> },
    /// One \item; indent counts nested list environments (1-based).
    ListItem { indent: usize, marker: String, spans: Vec<Span> },
    /// rows -> cells -> spans; from tabular. No header semantics in LaTeX,
    /// but \hline after the first row is the common convention.
    Table { rows: Vec<Vec<Vec<Span>>>, float: FloatInfo },
    /// \includegraphics: the path as written, the requested width as a
    /// fraction of the text column (None = the image's natural size).
    Image { path: String, width: Option<f32>, float: FloatInfo },
    /// \caption text, shown under figures/tables.
    Caption { spans: Vec<Span>, float: FloatInfo },
    /// A line of the title block: the authors, the date, or — with `small`
    /// set — the \thanks note that goes with them.
    Byline { spans: Vec<Span>, small: bool },
    Rule,
}

/// What a `\label` was attached to, so `\ref` prints the right number and
/// `\autoref` can name the thing.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RefKind {
    Section,
    Figure,
    Table,
    Equation,
}

impl RefKind {
    fn name(self) -> &'static str {
        match self {
            RefKind::Section => "Section",
            RefKind::Figure => "Figure",
            RefKind::Table => "Table",
            RefKind::Equation => "Equation",
        }
    }
}

#[derive(Clone, Debug)]
struct RefTarget {
    kind: RefKind,
    number: String,
}

#[derive(Clone, Copy)]
struct StyleFlags {
    bold: bool,
    italic: bool,
    mono: bool,
    link: bool,
    script: i8,
    size: f32,
}

impl Default for StyleFlags {
    fn default() -> Self {
        StyleFlags {
            bold: false,
            italic: false,
            mono: false,
            link: false,
            script: 0,
            size: 1.0,
        }
    }
}

struct ListCtx {
    /// Some(next number) for enumerate, None for itemize/description.
    counter: Option<u64>,
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    blocks: Vec<TexBlock>,
    spans: Vec<Span>,
    style: StyleFlags,
    /// One entry per open `{` group: the style and the alignment depth to
    /// restore on `}`, since `{\centering …}` is scoped to its group.
    groups: Vec<(StyleFlags, usize)>,
    /// Alignment depth and text style at each open environment, restored by
    /// its `\end`: an environment is a group, so a `\scriptsize` or
    /// `\centering` inside one must not leak past it.
    env_marks: Vec<(usize, StyleFlags)>,
    lists: Vec<ListCtx>,
    /// Marker for the \item currently being accumulated, if any.
    item: Option<String>,
    /// \section counters for the auto numbering: [section, sub, subsub].
    counters: [u64; 3],
    /// Equation counter for numbered display math.
    eq_counter: u64,
    /// Float counters, incremented at \begin{figure} / \begin{table}.
    fig_counter: u64,
    tab_counter: u64,
    /// The most recently numbered thing, which is what a `\label` binds to.
    last_numbered: Option<RefTarget>,
    /// Open floats, so `\caption` knows its own number.
    floats: Vec<RefTarget>,
    /// One entry per open float environment, describing how it may be set.
    open_floats: Vec<FloatInfo>,
    /// Width of each open `minipage`/`subfigure` as a fraction of the text
    /// column: the figures inside one are that much narrower.
    boxes: Vec<f32>,
    /// Labels found in this pass (filled on the scanning pass).
    labels: HashMap<String, RefTarget>,
    /// Labels from the scanning pass, used to resolve `\ref` in the real one.
    resolved: HashMap<String, RefTarget>,
    /// Set when everything pushed into the current paragraph so far is a
    /// single formula, so it can be promoted to a display equation.
    solo_math: Option<MathNode>,
    /// A label the next paragraph runs into, as the journal classes set
    /// "Abstract—" and "Index Terms—".
    run_in: Option<Vec<Span>>,
    /// Alignment in force, one entry per open environment or group that set
    /// one (`center`, `\centering`, `raggedleft`).
    aligns: Vec<TexAlign>,
    /// Depth of the environments TeX sets inset from both margins: an
    /// `abstract` in an article, a `quote`.
    inset: usize,
    meta: Meta,
}

/// The page the document class asks for, in TeX points: everything the
/// viewer needs to lay the text out on the same grid the compiler would.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocStyle {
    pub page_w: f32,
    pub page_h: f32,
    pub margin_x: f32,
    pub margin_y: f32,
    /// 1 for a normal document, 2 for the two-column classes and options.
    pub columns: usize,
    /// Gap between columns.
    pub gutter: f32,
    /// Body type size.
    pub base_pt: f32,
    /// Deepest section level that gets a number (`secnumdepth`): 1 numbers
    /// sections, 0 numbers nothing.
    pub secnumdepth: i32,
    /// First-line indent of a paragraph, in points.
    pub parindent: f32,
    /// Space between paragraphs, in points.
    pub parskip: f32,
    /// Whether captions print a "Figure 1:" label (`labelformat=empty` in a
    /// `\captionsetup` turns it off).
    pub caption_labels: bool,
}

impl Default for DocStyle {
    fn default() -> Self {
        // article, 10pt, letterpaper: 345pt of text centered on the page.
        DocStyle {
            page_w: 612.0,
            page_h: 792.0,
            margin_x: 133.5,
            margin_y: 108.0,
            columns: 1,
            gutter: 10.0,
            base_pt: 10.0,
            secnumdepth: 3,
            parindent: 15.0,
            parskip: 0.0,
            caption_labels: true,
        }
    }
}

/// Reads `\documentclass` (and a `geometry` package call, when there is one)
/// to work out the page the source would compile to. Everything it cannot
/// recognize keeps the article defaults, which is what most sources are.
pub fn document_style(source: &str) -> DocStyle {
    let clean = strip_comments(source);
    let mut style = DocStyle::default();
    let (opts, class) = match clean.find("\\documentclass") {
        Some(i) => {
            let after = &clean[i + "\\documentclass".len()..];
            let opts = bracket_arg(after).unwrap_or_default();
            let rest = match after.find('{') {
                Some(j) => &after[j..],
                None => "",
            };
            (opts, brace_arg(rest).unwrap_or_default())
        }
        None => (String::new(), String::new()),
    };
    let opts: Vec<String> =
        opts.split(',').map(|o| o.trim().to_lowercase()).filter(|o| !o.is_empty()).collect();
    let class = class.trim().to_lowercase();
    let has = |name: &str| opts.iter().any(|o| o == name);

    // Paper size. A4 is the common non-US case; everything else stays letter.
    if has("a4paper") || has("a4") {
        style.page_w = 595.0;
        style.page_h = 842.0;
    } else if has("a5paper") {
        style.page_w = 420.0;
        style.page_h = 595.0;
    } else if has("b5paper") {
        style.page_w = 499.0;
        style.page_h = 709.0;
    }
    for pt in [9.0, 10.0, 11.0, 12.0] {
        if has(&format!("{pt:.0}pt")) {
            style.base_pt = pt;
        }
    }
    // Two-column classes and the twocolumn option. IEEE and the ACM/SIG
    // classes are two-column unless they are told otherwise.
    let two_by_class = class.starts_with("ieee")
        || class.starts_with("sig-")
        || class.starts_with("sigchi")
        || class == "acmart"
        || class.starts_with("revtex");
    if (two_by_class && !has("onecolumn")) || has("twocolumn") {
        style.columns = 2;
    }
    if two_by_class {
        // The IEEE grid: narrow margins, two 3.5in columns, a 1/6in gutter.
        style.margin_x = 0.62 * 72.0;
        style.margin_y = 0.68 * 72.0;
        style.gutter = 0.17 * 72.0;
        style.base_pt = if has("9pt") { 9.0 } else { 10.0 };
    } else if style.columns == 2 {
        style.margin_x = 72.0;
        style.margin_y = 90.0;
        style.gutter = 14.0;
    } else {
        // Single-column article: the classic wide-margin measure, which grows
        // a little with the type size.
        let text_w = match style.base_pt {
            p if p >= 12.0 => 390.0,
            p if p >= 11.0 => 360.0,
            _ => 345.0,
        };
        style.margin_x = ((style.page_w - text_w) / 2.0).max(54.0);
        style.margin_y = 108.0;
    }

    // Paragraph shape and numbering depth, as the preamble set them.
    style.parindent = 1.5 * style.base_pt;
    if let Some(v) = set_length(&clean, "parindent") {
        style.parindent = v;
    }
    if let Some(v) = set_length(&clean, "parskip") {
        style.parskip = v;
    }
    if let Some(v) = set_counter(&clean, "secnumdepth") {
        style.secnumdepth = v;
    }
    if let Some(i) = clean.find("\\captionsetup") {
        let arg = brace_arg(&clean[i + "\\captionsetup".len()..]).unwrap_or_default();
        if arg.contains("labelformat=empty") {
            style.caption_labels = false;
        }
    }

    // An explicit geometry call wins over the class defaults.
    if let Some(i) = clean.find("{geometry}") {
        let head = &clean[..i];
        if let Some(j) = head.rfind("\\usepackage") {
            apply_geometry(&mut style, &bracket_arg(&head[j + "\\usepackage".len()..]).unwrap_or_default());
        }
    }
    if let Some(i) = clean.find("\\geometry") {
        let arg = brace_arg(&clean[i + "\\geometry".len()..]).unwrap_or_default();
        apply_geometry(&mut style, &arg);
    }
    style
}

/// Applies the `geometry` keys the viewer can act on: uniform margins, the
/// per-side ones, and an explicit text width or height.
fn apply_geometry(style: &mut DocStyle, opts: &str) {
    let mut left = None;
    let mut right = None;
    for opt in opts.split(',') {
        let (key, value) = match opt.split_once('=') {
            Some((k, v)) => (k.trim().to_lowercase(), v.trim()),
            None => {
                match opt.trim().to_lowercase().as_str() {
                    "a4paper" => {
                        style.page_w = 595.0;
                        style.page_h = 842.0;
                    }
                    "letterpaper" => {
                        style.page_w = 612.0;
                        style.page_h = 792.0;
                    }
                    "twocolumn" => style.columns = 2,
                    _ => {}
                }
                continue;
            }
        };
        let Some(pt) = length_pt(value) else { continue };
        match key.as_str() {
            "margin" => {
                style.margin_x = pt;
                style.margin_y = pt;
            }
            "hmargin" => style.margin_x = pt,
            "vmargin" | "tmargin" | "top" | "bmargin" | "bottom" => style.margin_y = pt,
            "left" | "lmargin" | "inner" => left = Some(pt),
            "right" | "rmargin" | "outer" => right = Some(pt),
            "textwidth" => style.margin_x = ((style.page_w - pt) / 2.0).max(18.0),
            "textheight" => style.margin_y = ((style.page_h - pt) / 2.0).max(18.0),
            "paperwidth" => style.page_w = pt,
            "paperheight" => style.page_h = pt,
            "columnsep" => style.gutter = pt,
            _ => {}
        }
    }
    if let (Some(l), Some(r)) = (left, right) {
        style.margin_x = (l + r) / 2.0;
    } else if let Some(m) = left.or(right) {
        style.margin_x = m;
    }
}

/// The value of a `\setlength{\name}{…}` in the preamble, in points.
fn set_length(source: &str, name: &str) -> Option<f32> {
    let needle = format!("\\setlength{{\\{name}}}");
    let at = source.find(&needle)? + needle.len();
    let raw = brace_arg(&source[at..])?;
    // em and ex are relative; a paragraph indent of "1.5em" is common.
    let raw = raw.trim();
    if let Some(n) = raw.strip_suffix("em").and_then(|n| n.trim().parse::<f32>().ok()) {
        return Some(n * 12.0);
    }
    if let Some(n) = raw.strip_suffix("ex").and_then(|n| n.trim().parse::<f32>().ok()) {
        return Some(n * 5.0);
    }
    if raw.starts_with('0') && raw.trim_start_matches(['0', '.', ' ']).is_empty() {
        return Some(0.0);
    }
    length_pt(raw)
}

/// The value of a `\setcounter{name}{…}` in the preamble.
fn set_counter(source: &str, name: &str) -> Option<i32> {
    let needle = format!("\\setcounter{{{name}}}");
    let at = source.find(&needle)? + needle.len();
    brace_arg(&source[at..])?.trim().parse().ok()
}

/// The directories a `\graphicspath{{figures/}{plots/}}` declares, in the
/// order LaTeX would search them.
pub fn graphics_paths(source: &str) -> Vec<String> {
    let clean = strip_comments(source);
    // Balanced, since the argument is a group of groups.
    let Some(arg) = find_raw_arg(&clean, "\\graphicspath") else { return Vec::new() };
    // The argument is a list of brace groups: {{figures/}{plots/}}.
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    for c in arg.chars() {
        match c {
            '{' => {
                depth += 1;
                if depth == 1 {
                    cur.clear();
                }
            }
            '}' => {
                if depth == 1 && !cur.trim().is_empty() {
                    out.push(cur.trim().to_string());
                }
                depth = depth.saturating_sub(1);
            }
            _ if depth >= 1 => cur.push(c),
            _ => {}
        }
    }
    if out.is_empty() && !arg.trim().is_empty() {
        out.push(arg.trim().to_string());
    }
    out
}

/// A TeX length in points, for the units that show up in class options.
fn length_pt(value: &str) -> Option<f32> {
    let value = value.trim();
    let split = value.find(|c: char| c.is_alphabetic())?;
    let n: f32 = value[..split].trim().parse().ok()?;
    let unit = value[split..].trim();
    Some(match unit {
        "in" => n * 72.0,
        "cm" => n * 28.3465,
        "mm" => n * 2.83465,
        "pt" | "bp" => n,
        "pc" => n * 12.0,
        _ => return None,
    })
}

/// The contents of a leading `[...]`, if the text starts with one.
fn bracket_arg(after: &str) -> Option<String> {
    let after = after.trim_start();
    let rest = after.strip_prefix('[')?;
    let end = rest.find(']')?;
    Some(rest[..end].to_string())
}

/// The contents of a leading `{...}`, if the text starts with one.
fn brace_arg(after: &str) -> Option<String> {
    let after = after.trim_start();
    let rest = after.strip_prefix('{')?;
    let end = rest.find('}')?;
    Some(rest[..end].to_string())
}

/// Parses LaTeX source into a flat block list with the default (article)
/// page style. Never fails: unknown input passes through as text.
#[cfg(test)]
pub fn parse(source: &str) -> Vec<TexBlock> {
    parse_with(source, &DocStyle::default())
}

/// Parses with the document's own style, which decides how sections are
/// numbered: the two-column journal classes use roman numerals and letters
/// where the article classes count 1, 1.1, 1.1.1.
pub fn parse_with(source: &str, style: &DocStyle) -> Vec<TexBlock> {
    let clean = strip_comments(source);
    // \thanks inside \author is the affiliation footnote, not part of the
    // byline; pull it out before the names are flattened.
    let raw_author = find_raw_arg(&clean, "\\author").unwrap_or_default();
    let (author_src, notes) = split_thanks(&raw_author);
    // Kept raw: the title block is parsed as inline content, so an
    // affiliation marker set with \textsuperscript stays a superscript.
    let non_empty = |s: String| (!s.trim().is_empty()).then_some(s);
    let meta = Meta {
        title: find_raw_arg(&clean, "\\title").and_then(non_empty),
        author: non_empty(author_src),
        date: find_raw_arg(&clean, "\\date").and_then(non_empty),
        notes,
        style: *style,
        cites: bib_numbers(&clean),
    };
    // Format the body only; the preamble is setup noise.
    let body = match clean.find("\\begin{document}") {
        Some(i) => {
            let after = &clean[i + "\\begin{document}".len()..];
            match after.find("\\end{document}") {
                Some(j) => &after[..j],
                None => after,
            }
        }
        None => clean.as_str(),
    };
    // A first pass collects every \label and the number it belongs to, so a
    // \ref pointing forward — the common case — still resolves on the second.
    let mut scan = Parser::new(body, Meta { style: *style, ..Meta::default() }, HashMap::new());
    scan.run();
    let resolved = std::mem::take(&mut scan.labels);

    let mut p = Parser::new(body, meta, resolved);
    p.run();
    p.flush_paragraph();
    p.blocks
}

/// The front matter a `\maketitle` prints, plus the document-wide facts the
/// body parse needs.
#[derive(Clone, Default)]
struct Meta {
    title: Option<String>,
    author: Option<String>,
    date: Option<String>,
    /// \thanks notes lifted out of the author list.
    notes: Vec<String>,
    style: DocStyle,
    /// Citation key -> the number \bibitem gives it.
    cites: HashMap<String, usize>,
}

/// Numbers the `\bibitem` keys in the order they are declared, so `\cite`
/// prints the number a reader sees in the reference list rather than the key.
fn bib_numbers(source: &str) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    let mut rest = source;
    while let Some(i) = rest.find("\\bibitem") {
        rest = &rest[i + "\\bibitem".len()..];
        // An optional [label] comes before the key.
        let mut head = rest.trim_start();
        if let Some(stripped) = head.strip_prefix('[') {
            match stripped.find(']') {
                Some(j) => head = stripped[j + 1..].trim_start(),
                None => continue,
            }
        }
        if let Some(key) = brace_arg(head) {
            let next = out.len() + 1;
            out.entry(key.trim().to_string()).or_insert(next);
        }
    }
    out
}

/// Splits `\thanks{...}` groups out of a raw argument, returning the text
/// without them and the notes themselves.
fn split_thanks(raw: &str) -> (String, Vec<String>) {
    let mut out = String::with_capacity(raw.len());
    let mut notes = Vec::new();
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let tail: String = chars[i..].iter().take(9).collect();
        if tail.starts_with("\\thanks") || tail.starts_with("\\footnote") {
            let skip = if tail.starts_with("\\thanks") { 7 } else { 9 };
            let mut j = i + skip;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if chars.get(j) == Some(&'{') {
                let mut depth = 0usize;
                let mut note = String::new();
                while j < chars.len() {
                    match chars[j] {
                        '{' => {
                            depth += 1;
                            if depth > 1 {
                                note.push('{');
                            }
                        }
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                j += 1;
                                break;
                            }
                            note.push('}');
                        }
                        c => note.push(c),
                    }
                    j += 1;
                }
                notes.push(note);
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    (out, notes)
}

/// The raw (unflattened) `{...}` argument of the first `\name{`.
fn find_raw_arg(source: &str, name: &str) -> Option<String> {
    let mut search = 0;
    loop {
        let i = source[search..].find(name)? + search;
        let after = &source[i + name.len()..];
        if !after.starts_with('{') {
            search = i + name.len();
            continue;
        }
        let mut depth = 0usize;
        let mut arg = String::new();
        for c in after.chars() {
            match c {
                '{' => {
                    if depth > 0 {
                        arg.push(c);
                    }
                    depth += 1;
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(arg);
                    }
                    arg.push(c);
                }
                _ => {
                    if depth > 0 {
                        arg.push(c);
                    }
                }
            }
        }
        return None;
    }
}

/// Cuts every line at its first unescaped `%`.
fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        let mut prev_backslash = false;
        let mut cut = None;
        for (i, c) in line.char_indices() {
            if c == '%' && !prev_backslash {
                cut = Some(i);
                break;
            }
            prev_backslash = c == '\\' && !prev_backslash;
        }
        match cut {
            // Keep the newline so paragraph breaks survive.
            Some(i) => {
                out.push_str(&line[..i]);
                if line.ends_with('\n') {
                    out.push('\n');
                }
            }
            None => out.push_str(line),
        }
    }
    out
}

/// Returns the `{...}` argument of the first `\name{` occurrence, flattened
/// of nested commands (best effort, used for \title/\author/\date).
fn find_arg(source: &str, name: &str) -> Option<String> {
    let mut search = 0;
    loop {
        let i = source[search..].find(name)? + search;
        let after = &source[i + name.len()..];
        // Reject longer commands sharing the prefix (\titleformat...).
        let mut it = after.chars();
        match it.next() {
            Some('{') => {
                let mut depth = 0usize;
                let mut arg = String::new();
                for c in after.chars() {
                    match c {
                        '{' => {
                            if depth > 0 {
                                arg.push(c);
                            }
                            depth += 1;
                        }
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                let flat: Vec<Span> = parse_inline(&arg);
                                let text: String =
                                    flat.iter().map(|s| s.text.as_str()).collect();
                                let text = text.trim().to_string();
                                return if text.is_empty() { None } else { Some(text) };
                            }
                            arg.push(c);
                        }
                        _ => {
                            if depth > 0 {
                                arg.push(c);
                            }
                        }
                    }
                }
                return None;
            }
            _ => search = i + name.len(),
        }
    }
}

/// Parses a fragment as inline content only (no block structure), returning
/// its styled spans. Used for command arguments like headings and captions.
fn parse_inline(fragment: &str) -> Vec<Span> {
    parse_inline_with(fragment, &HashMap::new())
}

/// Inline parse that can still resolve cross-references.
fn parse_inline_with(fragment: &str, resolved: &HashMap<String, RefTarget>) -> Vec<Span> {
    let mut p = Parser::new(fragment, Meta::default(), resolved.clone());
    p.run();
    // A fragment can contain something that ends a paragraph — a table cell
    // set with `\RaggedRight`, a stray `\par` — so take back whatever was
    // flushed into blocks rather than losing it.
    let mut out: Vec<Span> = Vec::new();
    for block in &p.blocks {
        match block {
            TexBlock::Paragraph { spans, .. }
            | TexBlock::ListItem { spans, .. }
            | TexBlock::Heading { spans, .. } => out.extend(spans.iter().cloned()),
            _ => {}
        }
    }
    out.append(&mut p.spans);
    out
}

const CODE_ENVS: [&str; 5] = ["verbatim", "verbatim*", "lstlisting", "minted", "Verbatim"];
const MATH_ENVS: [&str; 10] = [
    "equation", "equation*", "align", "align*", "gather", "gather*", "displaymath", "eqnarray",
    "eqnarray*", "multline",
];
const TABLE_ENVS: [&str; 6] =
    ["tabular", "tabular*", "tabularx", "tabulary", "longtable", "array"];
const LIST_ENVS: [&str; 3] = ["itemize", "enumerate", "description"];

/// Splits an `align`-family body into the rows the `\\` separate, ignoring
/// the breaks nested inside a group or inside an inner environment such as a
/// `matrix` or `cases`. Without this the parser stopped at the first break and
/// every row after it was dropped.
fn split_math_rows(raw: &str) -> Vec<String> {
    let chars: Vec<char> = raw.chars().collect();
    let (mut rows, mut start, mut depth, mut env) = (Vec::new(), 0usize, 0i32, 0i32);
    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            '{' => depth += 1,
            '}' => depth -= 1,
            '\\' if chars.get(i + 1) == Some(&'\\') && depth <= 0 && env <= 0 => {
                rows.push(chars[start..i].iter().collect());
                i += 2;
                // A row break may carry a spacing argument, e.g. `\\[2pt]`.
                if chars.get(i) == Some(&'[') {
                    while i < chars.len() && chars[i] != ']' {
                        i += 1;
                    }
                    i += 1;
                }
                start = i;
                continue;
            }
            '\\' => {
                let rest: String = chars[i..].iter().take(6).collect();
                if rest.starts_with("\\begin") {
                    env += 1;
                } else if rest.starts_with("\\end") {
                    env -= 1;
                }
                // Skip the escaped character so `\\{` does not count as a group.
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    rows.push(chars[start.min(chars.len())..].iter().collect());
    rows.retain(|r: &String| !r.trim().is_empty());
    rows
}
/// Environments whose begin/end are ignored and content flows through.
const SKIP_ENVS: [&str; 11] = [
    "document",
    "center",
    "figure",
    "figure*",
    "table",
    "table*",
    "flushleft",
    "flushright",
    "quote",
    "quotation",
    "adjustwidth",
];

impl Parser {
    fn new(body: &str, meta: Meta, resolved: HashMap<String, RefTarget>) -> Self {
        Parser {
            chars: body.chars().collect(),
            pos: 0,
            blocks: Vec::new(),
            spans: Vec::new(),
            style: StyleFlags::default(),
            groups: Vec::new(),
            lists: Vec::new(),
            item: None,
            counters: [0; 3],
            eq_counter: 0,
            fig_counter: 0,
            tab_counter: 0,
            last_numbered: None,
            floats: Vec::new(),
            open_floats: Vec::new(),
            boxes: Vec::new(),
            labels: HashMap::new(),
            resolved,
            solo_math: None,
            run_in: None,
            aligns: Vec::new(),
            env_marks: Vec::new(),
            inset: 0,
            meta,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !text.trim().is_empty() {
            self.solo_math = None;
        }
        let s = self.style;
        if let Some(last) = self.spans.last_mut() {
            if last.bold == s.bold
                && last.italic == s.italic
                && last.mono == s.mono
                && last.link == s.link
                && last.script == s.script
                && last.size == s.size
            {
                last.text.push_str(text);
                return;
            }
        }
        self.spans.push(Span {
            text: text.to_string(),
            bold: s.bold,
            italic: s.italic,
            mono: s.mono,
            link: s.link,
            script: s.script,
            size: s.size,
        });
    }

    fn flush_paragraph(&mut self) {
        let mut spans = std::mem::take(&mut self.spans);
        if let Some(first) = spans.first_mut() {
            first.text = first.text.trim_start().to_string();
        }
        if let Some(last) = spans.last_mut() {
            last.text = last.text.trim_end().to_string();
        }
        spans.retain(|s| !s.text.is_empty());
        if spans.iter().all(|s| s.text.trim().is_empty()) {
            self.solo_math = None;
            return;
        }
        // A paragraph that is nothing but a formula is a display equation,
        // however the source spelled it.
        if let Some(node) = self.solo_math.take() {
            let bare_item = self.item.as_ref().is_none_or(String::is_empty);
            if bare_item {
                self.blocks.push(TexBlock::Math { node, number: None });
                return;
            }
        }
        if let Some(mut label) = self.run_in.take() {
            label.append(&mut spans);
            spans = label;
        }
        if let Some(marker) = self.item.take() {
            self.blocks.push(TexBlock::ListItem {
                indent: self.lists.len().max(1),
                marker,
                spans,
            });
            // Later text in the same \item flows as further list lines.
            self.item = Some(String::new());
        } else {
            self.blocks.push(TexBlock::Paragraph {
                spans,
                align: self.align(),
                inset: self.inset > 0,
            });
        }
    }

    /// How the float being filled may be set, if there is one.
    fn float_info(&self) -> FloatInfo {
        self.open_floats.last().copied().unwrap_or_default()
    }

    /// The alignment in force for the paragraph being flushed.
    fn align(&self) -> TexAlign {
        self.aligns.last().copied().unwrap_or_default()
    }

    /// Pushes inline math, italicizing only the variables.
    fn push_inline_math(&mut self, raw: &str) {
        // pandoc writes display formulas as `\(…\)` sitting alone in a
        // paragraph; note that so flush can promote them to real display math.
        let alone = self.spans.iter().all(|s| s.text.trim().is_empty());
        let node = parse_math(raw);
        let saved = self.style;
        for span in math_spans(&node) {
            self.style.italic = span.italic;
            self.style.script = span.script;
            self.push_text(&span.text);
        }
        self.style = saved;
        self.solo_math = alone.then_some(node);
    }

    /// Pushes one display equation, numbering it the way TeX would.
    fn push_display_math(&mut self, raw: &str, numbered: bool) {
        if raw.trim().is_empty() {
            return;
        }
        let node = parse_math(raw);
        let number = numbered.then(|| {
            self.eq_counter += 1;
            self.last_numbered = Some(RefTarget {
                kind: RefKind::Equation,
                number: self.eq_counter.to_string(),
            });
            format!("({})", self.eq_counter)
        });
        // An equation's \label sits inside the environment body, which the
        // math parser consumes whole, so bind it here instead.
        if let (Some(key), Some(target)) =
            (find_arg(raw, "\\label"), self.last_numbered.clone())
        {
            if target.kind == RefKind::Equation {
                self.labels.insert(key, target);
            }
        }
        self.blocks.push(TexBlock::Math { node, number });
    }

    /// Reads a balanced `{...}` group, assuming `{` is next (skips leading
    /// whitespace). Returns None when the next token is not a group.
    fn read_group(&mut self) -> Option<String> {
        let save = self.pos;
        while self.peek().is_some_and(|c| c == ' ' || c == '\t') {
            self.pos += 1;
        }
        if self.peek() != Some('{') {
            self.pos = save;
            return None;
        }
        self.pos += 1;
        let mut depth = 1usize;
        let mut out = String::new();
        while let Some(c) = self.bump() {
            match c {
                '{' => {
                    depth += 1;
                    out.push(c);
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(out);
                    }
                    out.push(c);
                }
                '\\' => {
                    out.push(c);
                    // Keep escaped braces from closing the group.
                    if let Some(n) = self.bump() {
                        out.push(n);
                    }
                }
                _ => out.push(c),
            }
        }
        Some(out)
    }

    /// Consumes every consecutive optional argument. natbib's `\citep[][]{k}`
    /// and friends take more than one, and a leftover `[]` would otherwise
    /// fall through as body text.
    fn skip_opts(&mut self) {
        let mut before = self.pos;
        loop {
            self.skip_opt();
            if self.pos == before {
                return;
            }
            before = self.pos;
        }
    }

    /// Consumes an optional `[...]` argument if present.
    fn skip_opt(&mut self) {
        let _ = self.read_opt();
    }

    /// Reads an optional `[...]` argument, returning its contents.
    fn read_opt(&mut self) -> Option<String> {
        let save = self.pos;
        while self.peek().is_some_and(|c| c == ' ' || c == '\t') {
            self.pos += 1;
        }
        if self.peek() != Some('[') {
            self.pos = save;
            return None;
        }
        self.pos += 1;
        let mut depth = 1usize;
        let mut out = String::new();
        while let Some(c) = self.bump() {
            match c {
                '[' => {
                    depth += 1;
                    out.push(c);
                }
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(out);
                    }
                    out.push(c);
                }
                // Keep a braced group's brackets from ending the argument.
                '{' => {
                    let mut inner = 1usize;
                    out.push(c);
                    while let Some(n) = self.bump() {
                        out.push(n);
                        match n {
                            '{' => inner += 1,
                            '}' => {
                                inner -= 1;
                                if inner == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => out.push(c),
            }
        }
        Some(out)
    }

    /// Reads raw text until `\end{env}`, consuming the terminator.
    fn read_until_end(&mut self, env: &str) -> String {
        let needle: Vec<char> = format!("\\end{{{env}}}").chars().collect();
        let mut out = String::new();
        while self.pos < self.chars.len() {
            if self.chars[self.pos..].starts_with(&needle[..]) {
                self.pos += needle.len();
                return out;
            }
            out.push(self.chars[self.pos]);
            self.pos += 1;
        }
        out
    }

    /// Styled-group commands: consume `{`, apply the style until the match.
    fn styled_group(&mut self, apply: impl Fn(&mut StyleFlags)) {
        while self.peek().is_some_and(|c| c == ' ' || c == '\t') {
            self.pos += 1;
        }
        if self.peek() == Some('{') {
            self.pos += 1;
            self.groups.push((self.style, self.aligns.len()));
            apply(&mut self.style);
        }
    }

    fn heading(&mut self, level: u8, starred: bool) {
        self.skip_opt();
        let Some(arg) = self.read_group() else { return };
        self.flush_paragraph();
        let mut spans = parse_inline_with(&arg, &self.resolved);
        let numbered = !starred
            && (1..=3).contains(&level)
            && i32::from(level) <= self.meta.style.secnumdepth;
        if numbered {
            let idx = (level - 1) as usize;
            self.counters[idx] += 1;
            for c in &mut self.counters[idx + 1..] {
                *c = 0;
            }
            let (number, label) = self.section_number(level);
            self.last_numbered = Some(RefTarget { kind: RefKind::Section, number });
            spans.insert(0, Span { text: label, ..Span::default() });
        }
        self.blocks.push(TexBlock::Heading { level, spans });
    }

    /// The number a section heading prints, as (the number a `\ref` shows,
    /// the label in front of the title). The two-column journal classes count
    /// I, A, 1) down the levels; everything else counts 1, 1.1, 1.1.1.
    fn section_number(&self, level: u8) -> (String, String) {
        let idx = (level - 1) as usize;
        let n = self.counters[idx];
        if self.meta.style.columns == 2 {
            let number = match level {
                1 => roman(n),
                2 => letter(n),
                _ => n.to_string(),
            };
            let label = match level {
                3 => format!("{number}) "),
                _ => format!("{number}. "),
            };
            return (number, label);
        }
        let number = self.counters[..=idx]
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".");
        let label = format!("{number}  ");
        (number, label)
    }

    fn begin_env(&mut self, env: &str) {
        // An environment is a group: what it changes lasts until its `\end`.
        // Only the ones that let their content flow get a mark — a tabular or
        // a display swallows its own `\end`, so no `end_env` would pop it.
        let consumes_end = CODE_ENVS.contains(&env)
            || MATH_ENVS.contains(&env)
            || TABLE_ENVS.contains(&env);
        if !consumes_end {
            self.env_marks.push((self.aligns.len(), self.style));
        }
        if LIST_ENVS.contains(&env) {
            self.flush_paragraph();
            // enumitem's key=value list, e.g. [leftmargin=1.4em,itemsep=1pt].
            self.skip_opts();
            self.item = None;
            self.lists.push(ListCtx {
                counter: (env == "enumerate").then_some(1),
            });
        } else if CODE_ENVS.contains(&env) {
            self.flush_paragraph();
            // minted/lstlisting take language/option arguments.
            self.skip_opt();
            if env == "minted" {
                let _ = self.read_group();
            }
            let raw = self.read_until_end(env);
            let text = raw.trim_matches('\n').trim_end().to_string();
            if !text.is_empty() {
                self.blocks.push(TexBlock::Code(text));
            }
        } else if MATH_ENVS.contains(&env) {
            self.flush_paragraph();
            let raw = self.read_until_end(env);
            let numbered = !env.ends_with('*') && env != "displaymath";
            match env.trim_end_matches('*') {
                // These stack one equation per row, each with its own number.
                "align" | "gather" | "eqnarray" | "alignat" | "flalign" => {
                    for row in split_math_rows(&raw) {
                        let tagged = numbered
                            && !row.contains("\\nonumber")
                            && !row.contains("\\notag");
                        self.push_display_math(&row, tagged);
                    }
                }
                // multline breaks one equation over several lines and numbers
                // the last of them.
                "multline" => {
                    let rows = split_math_rows(&raw);
                    let last = rows.len().saturating_sub(1);
                    for (i, row) in rows.into_iter().enumerate() {
                        self.push_display_math(&row, numbered && i == last);
                    }
                }
                _ => self.push_display_math(&raw, numbered),
            }
        } else if TABLE_ENVS.contains(&env) {
            self.flush_paragraph();
            self.skip_opts(); // vertical placement, e.g. [t]
            if matches!(env, "tabular*" | "tabularx" | "tabulary") {
                let _ = self.read_group(); // target width
            }
            let _ = self.read_group(); // column spec
            let raw = self.read_until_end(env);
            let rows = parse_tabular(&raw, &self.resolved);
            if !rows.is_empty() {
                self.blocks.push(TexBlock::Table { rows, float: self.float_info() });
            }
        } else if env == "abstract" || env.eq_ignore_ascii_case("ieeekeywords") || env == "keywords" {
            self.flush_paragraph();
            let label = if env == "abstract" { "Abstract" } else { "Index Terms" };
            if self.meta.style.columns == 2 {
                // A journal sets the abstract as one bold paragraph that the
                // label runs into.
                self.run_in = Some(vec![Span {
                    text: format!("{label}—"),
                    bold: true,
                    italic: true,
                    ..Span::default()
                }]);
                self.style.bold = true;
            } else {
                // An article sets the label centered over a block pulled in
                // from both margins.
                self.blocks.push(TexBlock::Heading {
                    level: 5,
                    spans: vec![Span { text: label.into(), bold: true, ..Span::default() }],
                });
                self.inset += 1;
            }
        } else if env == "thebibliography" {
            let _ = self.read_group();
            self.flush_paragraph();
            self.blocks.push(TexBlock::Heading {
                level: 1,
                spans: vec![Span { text: "References".into(), bold: true, ..Span::default() }],
            });
            self.lists.push(ListCtx { counter: Some(1) });
        } else if matches!(env, "minipage" | "subfigure" | "subcaptionblock" | "wrapfigure") {
            // The figures inside a half-width box are half-width themselves.
            self.skip_opts();
            if env == "wrapfigure" {
                let _ = self.read_group(); // placement
            }
            let raw = self.read_group().unwrap_or_default();
            self.boxes.push(column_fraction(&raw).unwrap_or(1.0));
        } else if SKIP_ENVS.contains(&env) {
            self.flush_paragraph();
            match env {
                "center" => self.aligns.push(TexAlign::Center),
                "flushleft" => self.aligns.push(TexAlign::Left),
                "flushright" => self.aligns.push(TexAlign::Right),
                "quote" | "quotation" => self.inset += 1,
                _ => {}
            }
            let placement = self.read_opt().unwrap_or_default(); // e.g. [htbp]
            self.skip_opts();
            if matches!(env.trim_end_matches('*'), "figure" | "table") {
                self.open_floats.push(FloatInfo {
                    wide: env.ends_with('*'),
                    // The float package's [H] pins a float where it stands;
                    // everything else may travel to a page top.
                    movable: !placement.contains('H'),
                });
            }
            for _ in 0..env_args(env) {
                let _ = self.read_group();
            }
            let kind = match env.trim_end_matches('*') {
                "figure" => Some(RefKind::Figure),
                "table" => Some(RefKind::Table),
                _ => None,
            };
            if let Some(kind) = kind {
                let counter = match kind {
                    RefKind::Figure => &mut self.fig_counter,
                    _ => &mut self.tab_counter,
                };
                *counter += 1;
                let target = RefTarget { kind, number: counter.to_string() };
                self.last_numbered = Some(target.clone());
                self.floats.push(target);
            }
        } else {
            // Unknown environment: drop the arguments it declares, then let
            // its content flow through.
            self.skip_opts();
            for _ in 0..env_args(env) {
                let _ = self.read_group();
            }
        }
    }

    fn end_env(&mut self, env: &str) {
        if matches!(env, "minipage" | "subfigure" | "subcaptionblock" | "wrapfigure") {
            self.boxes.pop();
        }
        if let Some((depth, style)) = self.env_marks.pop() {
            self.flush_paragraph();
            self.aligns.truncate(depth);
            self.style = style;
        }
        if LIST_ENVS.contains(&env) || env == "thebibliography" {
            self.flush_paragraph();
            self.item = None;
            self.lists.pop();
        } else if env == "abstract"
            || env.eq_ignore_ascii_case("ieeekeywords")
            || env == "keywords"
            || SKIP_ENVS.contains(&env)
        {
            self.flush_paragraph();
            match env {
                "center" | "flushleft" | "flushright" => {
                    self.aligns.pop();
                }
                "quote" | "quotation" => self.inset = self.inset.saturating_sub(1),
                _ => {}
            }
            if matches!(env, "abstract" | "keywords") || env.eq_ignore_ascii_case("ieeekeywords") {
                self.run_in = None;
                self.style.bold = false;
                self.inset = self.inset.saturating_sub(1);
            }
            if matches!(env.trim_end_matches('*'), "figure" | "table") {
                self.floats.pop();
                self.open_floats.pop();
            }
        }
    }

    fn command(&mut self) {
        // The backslash is consumed; read the command name.
        let Some(first) = self.peek() else { return };
        if !first.is_ascii_alphabetic() {
            self.pos += 1;
            match first {
                '\\' => {
                    self.skip_opt(); // \\[2pt]
                    self.push_text("\n");
                }
                '%' | '&' | '$' | '#' | '_' | '{' | '}' => {
                    self.push_text(&first.to_string());
                }
                ',' | ';' | ' ' | '!' => self.push_text(" "),
                '[' => {
                    // Display math \[ ... \]
                    let mut raw = String::new();
                    while self.pos < self.chars.len() {
                        if self.chars[self.pos] == '\\'
                            && self.chars.get(self.pos + 1) == Some(&']')
                        {
                            self.pos += 2;
                            break;
                        }
                        raw.push(self.chars[self.pos]);
                        self.pos += 1;
                    }
                    self.flush_paragraph();
                    self.push_display_math(&raw, false);
                }
                '(' => {
                    // Inline math \( ... \)
                    let mut raw = String::new();
                    while self.pos < self.chars.len() {
                        if self.chars[self.pos] == '\\'
                            && self.chars.get(self.pos + 1) == Some(&')')
                        {
                            self.pos += 2;
                            break;
                        }
                        raw.push(self.chars[self.pos]);
                        self.pos += 1;
                    }
                    self.push_inline_math(&raw);
                }
                _ => {}
            }
            return;
        }
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        let name: String = self.chars[start..self.pos].iter().collect();
        let starred = self.peek() == Some('*');
        if starred {
            self.pos += 1;
        }
        match name.as_str() {
            "part" | "chapter" => self.heading(0, true),
            "section" => self.heading(1, starred),
            "subsection" => self.heading(2, starred),
            "subsubsection" => self.heading(3, starred),
            "paragraph" | "subparagraph" => {
                self.skip_opts();
                if let Some(arg) = self.read_group() {
                    self.flush_paragraph();
                    let mut spans = parse_inline_with(&arg, &self.resolved);
                    for s in &mut spans {
                        s.bold = true;
                    }
                    self.blocks.push(TexBlock::Heading { level: 4, spans });
                }
            }
            "begin" => {
                if let Some(env) = self.read_group() {
                    self.begin_env(env.trim());
                }
            }
            "end" => {
                if let Some(env) = self.read_group() {
                    self.end_env(env.trim());
                }
            }
            "item" => {
                self.skip_opt();
                self.flush_paragraph();
                let marker = match self.lists.last_mut() {
                    Some(ListCtx { counter: Some(n) }) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "•  ".to_string(),
                };
                self.item = Some(marker);
            }
            "bibitem" => {
                self.skip_opts();
                let _ = self.read_group();
                self.flush_paragraph();
                let marker = match self.lists.last_mut() {
                    Some(ListCtx { counter: Some(n) }) => {
                        let m = format!("[{n}] ");
                        *n += 1;
                        m
                    }
                    _ => "•  ".to_string(),
                };
                self.item = Some(marker);
            }
            "textbf" => self.styled_group(|s| s.bold = true),
            "textit" | "emph" | "textsl" => self.styled_group(|s| s.italic = true),
            "texttt" => self.styled_group(|s| s.mono = true),
            "underline" | "uline" | "textsc" | "textrm" | "textsf" | "textnormal" | "textup"
            | "mbox" | "text" | "textmd" => self.styled_group(|_| {}),
            "bfseries" | "bf" => self.style.bold = true,
            "itshape" | "em" | "it" | "sl" => self.style.italic = true,
            "ttfamily" | "tt" => self.style.mono = true,
            "rmfamily" | "sffamily" | "normalfont" | "mdseries" | "upshape" => {
                self.style = StyleFlags {
                    link: self.style.link,
                    size: self.style.size,
                    ..StyleFlags::default()
                };
            }
            "verb" => {
                // \verb<delim>...<delim>, raw.
                if let Some(delim) = self.bump() {
                    let mut out = String::new();
                    while let Some(c) = self.bump() {
                        if c == delim {
                            break;
                        }
                        out.push(c);
                    }
                    let saved = self.style;
                    self.style.mono = true;
                    self.push_text(&out);
                    self.style = saved;
                }
            }
            "includegraphics" => {
                let opts = self.read_opt();
                self.skip_opts();
                if let Some(path) = self.read_group() {
                    self.flush_paragraph();
                    // A width is relative to the box the picture sits in.
                    let scale: f32 = self.boxes.iter().product();
                    let width = opts
                        .as_deref()
                        .and_then(graphics_width)
                        .map(|w| (w * scale).clamp(0.05, 1.0));
                    self.blocks.push(TexBlock::Image {
                        path: path.trim().to_string(),
                        width,
                        float: self.float_info(),
                    });
                }
            }
            "caption" => {
                self.skip_opts();
                if let Some(arg) = self.read_group() {
                    self.flush_paragraph();
                    let mut spans = parse_inline_with(&arg, &self.resolved);
                    if let Some(float) = self.floats.last().filter(|_| self.meta.style.caption_labels) {
                        // The journal classes label floats "Fig. 1." and
                        // "TABLE I"; the article classes spell them out.
                        let label = if self.meta.style.columns == 2 {
                            match float.kind {
                                RefKind::Figure => format!("Fig. {}. ", float.number),
                                _ => format!(
                                    "TABLE {}. ",
                                    float
                                        .number
                                        .parse::<u64>()
                                        .map(roman)
                                        .unwrap_or_else(|_| float.number.clone())
                                ),
                            }
                        } else {
                            format!("{} {}: ", float.kind.name(), float.number)
                        };
                        spans.insert(0, Span { text: label, bold: true, ..Span::default() });
                    }
                    self.blocks.push(TexBlock::Caption { spans, float: self.float_info() });
                }
            }
            "maketitle" => {
                self.flush_paragraph();
                if let Some(t) = self.meta.title.clone() {
                    self.blocks.push(TexBlock::Heading {
                        level: 0,
                        spans: parse_inline_with(&t, &self.resolved),
                    });
                }
                for line in [self.meta.author.clone(), self.meta.date.clone()].into_iter().flatten() {
                    self.blocks.push(TexBlock::Byline {
                        spans: parse_inline_with(&line, &self.resolved),
                        small: false,
                    });
                }
                // The \thanks notes print as the footnote they are.
                let notes = std::mem::take(&mut self.meta.notes);
                if !notes.is_empty() {
                    self.blocks.push(TexBlock::Byline {
                        spans: parse_inline_with(&notes.join(" "), &self.resolved),
                        small: true,
                    });
                }
            }
            "label" => {
                if let Some(key) = self.read_group() {
                    if let Some(target) = self.last_numbered.clone() {
                        self.labels.insert(key.trim().to_string(), target);
                    }
                }
            }
            "title" | "author" | "date" | "vspace" | "hspace" | "input" | "include"
            | "bibliography" | "bibliographystyle" | "usepackage" | "documentclass"
            | "pagestyle" | "thispagestyle" | "graphicspath" => {
                self.skip_opts();
                let _ = self.read_group();
            }
            "ref" | "eqref" | "autoref" | "cref" | "Cref" | "pageref" | "nameref" => {
                self.skip_opts();
                if let Some(arg) = self.read_group() {
                    let key = arg.trim();
                    let saved = self.style;
                    self.style.link = true;
                    match self.resolved.get(key) {
                        Some(target) => {
                            // \ref prints the bare number; \eqref parenthesizes
                            // it; the \autoref family names the thing too.
                            let text = match name.as_str() {
                                "eqref" => format!("({})", target.number),
                                "autoref" | "cref" | "Cref" => {
                                    let noun = target.kind.name();
                                    let noun = if name == "cref" {
                                        noun.to_lowercase()
                                    } else {
                                        noun.to_string()
                                    };
                                    format!("{noun} {}", target.number)
                                }
                                _ => target.number.clone(),
                            };
                            self.push_text(&text);
                        }
                        // Undefined here (often defined in an \input'd file):
                        // show the key rather than losing the reference.
                        None => self.push_text(&format!("[{key}]")),
                    }
                    self.style = saved;
                }
            }
            "cite" | "citep" | "citet" | "citealp" | "citeauthor" | "citeyear" => {
                self.skip_opts();
                if let Some(arg) = self.read_group() {
                    // Print the numbers the reference list will show, the way
                    // a compiled bibliography does; keys with no \bibitem
                    // fall back to the key itself.
                    let keys: Vec<String> = arg
                        .split(',')
                        .map(|k| {
                            let k = k.trim();
                            match self.meta.cites.get(k) {
                                Some(n) => n.to_string(),
                                None => k.to_string(),
                            }
                        })
                        .collect();
                    let saved = self.style;
                    self.style.link = true;
                    self.push_text(&format!("[{}]", keys.join(", ")));
                    self.style = saved;
                }
            }
            "href" => {
                let url = self.read_group();
                match self.read_group() {
                    Some(text) => {
                        let saved = self.style;
                        self.style.link = true;
                        for s in parse_inline_with(&text, &self.resolved) {
                            self.push_text(&s.text);
                        }
                        self.style = saved;
                    }
                    None => {
                        if let Some(u) = url {
                            let saved = self.style;
                            self.style.link = true;
                            self.push_text(&u);
                            self.style = saved;
                        }
                    }
                }
            }
            "url" => {
                if let Some(u) = self.read_group() {
                    let saved = self.style;
                    self.style.link = true;
                    self.style.mono = true;
                    self.push_text(u.trim());
                    self.style = saved;
                }
            }
            "footnote" => {
                if let Some(arg) = self.read_group() {
                    let saved = self.style;
                    self.style.italic = true;
                    self.push_text(" (");
                    for s in parse_inline_with(&arg, &self.resolved) {
                        self.push_text(&s.text);
                    }
                    self.push_text(")");
                    self.style = saved;
                }
            }
            "thanks" => {
                // Outside the title block a \thanks is still a footnote; keep
                // it out of the running text.
                if let Some(arg) = self.read_group() {
                    self.meta.notes.push(arg);
                }
            }
            "textsuperscript" | "textsubscript" => {
                let up = name == "textsuperscript";
                self.styled_group(move |st| st.script = if up { 1 } else { -1 });
            }
            "color" => {
                let _ = self.read_group();
            }
            "newcommand" | "renewcommand" | "providecommand" => {
                let _ = self.read_group();
                self.skip_opts();
                let _ = self.read_group();
            }
            "def" => {
                // \def\name{body}: the name is a bare control sequence, not a
                // braced group, so consume it before the body.
                if self.peek() == Some('\\') {
                    self.pos += 1;
                    while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                        self.pos += 1;
                    }
                }
                self.skip_opts();
                let _ = self.read_group();
            }
            "hrule" | "hrulefill" => {
                self.flush_paragraph();
                self.blocks.push(TexBlock::Rule);
            }
            "newline" | "linebreak" | "smallbreak" | "medbreak" | "bigbreak" => {
                self.push_text("\n")
            }
            "par" => self.flush_paragraph(),
            "centering" | "raggedright" | "raggedleft" | "RaggedRight" | "RaggedLeft" => {
                // Scoped to the group or environment it appears in.
                self.flush_paragraph();
                self.aligns.push(match name.as_str() {
                    "centering" => TexAlign::Center,
                    "raggedleft" | "RaggedLeft" => TexAlign::Right,
                    _ => TexAlign::Left,
                });
            }
            "tiny" | "scriptsize" | "footnotesize" | "small" | "normalsize" | "large"
            | "Large" | "LARGE" | "huge" | "Huge" => {
                self.style.size = type_size(&name);
            }
            "newpage" | "clearpage" | "pagebreak" | "tableofcontents" | "listoffigures"
            | "listoftables" | "noindent" | "indent" | "smallskip" | "medskip" | "bigskip"
            | "hfill" | "vfill" | "frontmatter" | "mainmatter" | "backmatter" | "appendix"
            | "printbibliography" | "selectfont" | "flushbottom" | "onehalfspacing"
            | "doublespacing" | "singlespacing" => {}
            "ldots" | "dots" | "dotsc" | "dotso" => self.push_text("…"),
            "textbackslash" => self.push_text("\\"),
            "textasciitilde" => self.push_text("~"),
            "LaTeX" => self.push_text("LaTeX"),
            "TeX" => self.push_text("TeX"),
            "and" => self.push_text(", "),
            _ => {
                self.skip_opts();
                // Commands whose leading arguments are setup (lengths, counters,
                // colours, column spans): drop exactly those, and let anything
                // after them flow as content.
                for _ in 0..setup_args(&name) {
                    self.skip_opts();
                    if self.read_group().is_none() {
                        break;
                    }
                }
            }
        }
    }

    fn run(&mut self) {
        let mut pending = String::new();
        macro_rules! commit {
            () => {
                if !pending.is_empty() {
                    let text = std::mem::take(&mut pending);
                    self.push_text(&text);
                }
            };
        }
        while let Some(c) = self.peek() {
            match c {
                '\\' => {
                    commit!();
                    self.pos += 1;
                    self.command();
                }
                '{' => {
                    commit!();
                    self.pos += 1;
                    self.groups.push((self.style, self.aligns.len()));
                }
                '}' => {
                    commit!();
                    self.pos += 1;
                    if let Some((style, aligns)) = self.groups.pop() {
                        self.style = style;
                        self.aligns.truncate(aligns);
                    }
                }
                '$' => {
                    commit!();
                    self.pos += 1;
                    let display = self.peek() == Some('$');
                    if display {
                        self.pos += 1;
                    }
                    let mut raw = String::new();
                    while let Some(m) = self.bump() {
                        if m == '\\' {
                            raw.push(m);
                            if let Some(n) = self.bump() {
                                raw.push(n);
                            }
                            continue;
                        }
                        if m == '$' {
                            if display && self.peek() == Some('$') {
                                self.pos += 1;
                            }
                            break;
                        }
                        raw.push(m);
                    }
                    if display {
                        self.flush_paragraph();
                        self.push_display_math(&raw, false);
                    } else {
                        self.push_inline_math(&raw);
                    }
                }
                '\n' => {
                    self.pos += 1;
                    // A blank line ends the paragraph; a single newline is a
                    // space, as in TeX.
                    let mut newlines = 1;
                    while let Some(n) = self.peek() {
                        match n {
                            '\n' => {
                                newlines += 1;
                                self.pos += 1;
                            }
                            ' ' | '\t' | '\r' => {
                                self.pos += 1;
                            }
                            _ => break,
                        }
                    }
                    commit!();
                    if newlines > 1 {
                        self.flush_paragraph();
                        self.item = self.item.take().filter(|m| !m.is_empty());
                    } else if self
                        .spans
                        .last()
                        .is_some_and(|s| !s.text.ends_with(char::is_whitespace))
                    {
                        self.push_text(" ");
                    }
                }
                '~' => {
                    self.pos += 1;
                    pending.push(' ');
                }
                '`' => {
                    self.pos += 1;
                    if self.peek() == Some('`') {
                        self.pos += 1;
                        pending.push('“');
                    } else {
                        pending.push('‘');
                    }
                }
                '\'' => {
                    self.pos += 1;
                    if self.peek() == Some('\'') {
                        self.pos += 1;
                        pending.push('”');
                    } else {
                        pending.push('\'');
                    }
                }
                '-' => {
                    self.pos += 1;
                    if self.peek() == Some('-') {
                        self.pos += 1;
                        if self.peek() == Some('-') {
                            self.pos += 1;
                            pending.push('—');
                        } else {
                            pending.push('–');
                        }
                    } else {
                        pending.push('-');
                    }
                }
                _ => {
                    self.pos += 1;
                    pending.push(c);
                }
            }
        }
        commit!();
    }
}

/// A length written as a fraction of the column (`0.47\linewidth`), if that
/// is what it is.
fn column_fraction(raw: &str) -> Option<f32> {
    let raw = raw.trim();
    let at = raw.find('\\')?;
    let unit = raw[at + 1..].trim();
    let named = unit.starts_with("linewidth")
        || unit.starts_with("textwidth")
        || unit.starts_with("columnwidth")
        || unit.starts_with("hsize");
    if !named {
        return None;
    }
    let head = raw[..at].trim();
    if head.is_empty() {
        return Some(1.0);
    }
    head.parse::<f32>().ok().map(|f| f.clamp(0.05, 1.0))
}

/// Reads the width an \includegraphics asks for, as a fraction of the text
/// column. Only column-relative widths are meaningful here — the viewer has
/// no page geometry to turn centimetres into pixels.
fn graphics_width(opts: &str) -> Option<f32> {
    let at = opts.find("width")?;
    let rest = opts[at + "width".len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    // Either `\linewidth` or a multiplier in front of it.
    let (factor, unit) = match rest.find('\\') {
        Some(0) => (1.0f32, rest),
        Some(i) => (rest[..i].trim().parse::<f32>().ok()?, &rest[i..]),
        None => return None,
    };
    let unit = unit.trim_start_matches('\\');
    let named = unit.starts_with("linewidth")
        || unit.starts_with("textwidth")
        || unit.starts_with("columnwidth")
        || unit.starts_with("hsize");
    named.then(|| factor.clamp(0.05, 1.0))
}

/// Splits tabular content into rows (`\\`) and cells (`&`), respecting
/// braces and inline math, and dropping rules like \hline / \toprule.
fn parse_tabular(raw: &str, resolved: &HashMap<String, RefTarget>) -> Vec<Vec<Vec<Span>>> {
    let chars: Vec<char> = raw.chars().collect();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '{' => {
                depth += 1;
                cell.push(c);
                i += 1;
            }
            '}' => {
                depth = depth.saturating_sub(1);
                cell.push(c);
                i += 1;
            }
            '&' if depth == 0 => {
                row.push(std::mem::take(&mut cell));
                i += 1;
            }
            '\\' => {
                if chars.get(i + 1) == Some(&'\\') && depth == 0 {
                    row.push(std::mem::take(&mut cell));
                    rows.push(std::mem::take(&mut row));
                    i += 2;
                    // Optional [2pt] spacing after \\.
                    if chars.get(i) == Some(&'[') {
                        while i < chars.len() && chars[i] != ']' {
                            i += 1;
                        }
                        i += 1;
                    }
                } else {
                    cell.push(c);
                    if let Some(&n) = chars.get(i + 1) {
                        cell.push(n);
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            _ => {
                cell.push(c);
                i += 1;
            }
        }
    }
    if !cell.trim().is_empty() {
        row.push(cell);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    let strip_rules = |s: &str| -> String {
        let mut out = s.to_string();
        for rule in [
            "\\hline", "\\toprule", "\\midrule", "\\bottomrule", "\\centering", "\\arraybackslash",
        ] {
            out = out.replace(rule, " ");
        }
        // \cline{2-3} and \cmidrule(lr){2-3}
        for rule in ["\\cline", "\\cmidrule"] {
            while let Some(p) = out.find(rule) {
                let end = out[p..].find('}').map(|e| p + e + 1).unwrap_or(out.len());
                out.replace_range(p..end, " ");
            }
        }
        out
    };
    rows.into_iter()
        .map(|r| {
            r.into_iter()
                .map(|c| {
                    let cleaned = strip_rules(&c);
                    let mut spans = parse_inline_with(cleaned.trim(), resolved);
                    // Trim outer whitespace the columns padded with.
                    if let Some(first) = spans.first_mut() {
                        first.text = first.text.trim_start().to_string();
                    }
                    if let Some(last) = spans.last_mut() {
                        last.text = last.text.trim_end().to_string();
                    }
                    spans.retain(|s| !s.text.is_empty());
                    spans
                })
                .collect()
        })
        .filter(|r: &Vec<Vec<Span>>| r.iter().any(|c| !c.is_empty()))
        .collect()
}

const SUPERSCRIPTS: [(char, char); 38] = [
    ('0', '⁰'), ('1', '¹'), ('2', '²'), ('3', '³'), ('4', '⁴'), ('5', '⁵'), ('6', '⁶'),
    ('7', '⁷'), ('8', '⁸'), ('9', '⁹'), ('+', '⁺'), ('-', '⁻'), ('=', '⁼'), ('(', '⁽'),
    (')', '⁾'), ('a', 'ᵃ'), ('b', 'ᵇ'), ('c', 'ᶜ'), ('d', 'ᵈ'), ('e', 'ᵉ'), ('f', 'ᶠ'),
    ('g', 'ᵍ'), ('h', 'ʰ'), ('i', 'ⁱ'), ('j', 'ʲ'), ('k', 'ᵏ'), ('l', 'ˡ'), ('m', 'ᵐ'),
    ('n', 'ⁿ'), ('o', 'ᵒ'), ('p', 'ᵖ'), ('r', 'ʳ'), ('s', 'ˢ'), ('t', 'ᵗ'), ('u', 'ᵘ'),
    ('v', 'ᵛ'), ('w', 'ʷ'), ('x', 'ˣ'),
];
const SUBSCRIPTS: [(char, char); 32] = [
    ('0', '₀'), ('1', '₁'), ('2', '₂'), ('3', '₃'), ('4', '₄'), ('5', '₅'), ('6', '₆'),
    ('7', '₇'), ('8', '₈'), ('9', '₉'), ('+', '₊'), ('-', '₋'), ('=', '₌'), ('(', '₍'),
    (')', '₎'), ('a', 'ₐ'), ('e', 'ₑ'), ('h', 'ₕ'), ('i', 'ᵢ'), ('j', 'ⱼ'), ('k', 'ₖ'),
    ('l', 'ₗ'), ('m', 'ₘ'), ('n', 'ₙ'), ('o', 'ₒ'), ('p', 'ₚ'), ('r', 'ᵣ'), ('s', 'ₛ'),
    ('t', 'ₜ'), ('u', 'ᵤ'), ('v', 'ᵥ'), ('x', 'ₓ'),
];

/// Best-effort LaTeX-math to Unicode: named symbols, greek letters,
/// single-token super/subscripts, \frac and \sqrt.
/// One node of a math expression, shaped for box layout: the viewer measures
/// and positions these the way TeX stacks numerators, scripts and radicals,
/// instead of flattening everything onto one line.
#[derive(Clone, PartialEq, Debug)]
pub enum MathNode {
    /// A run of symbols. `italic` marks variables, which TeX sets in italic.
    Sym { text: String, italic: bool },
    Row(Vec<MathNode>),
    Frac { num: Box<MathNode>, den: Box<MathNode> },
    Sqrt { index: Option<Box<MathNode>>, arg: Box<MathNode> },
    /// A base carrying a superscript and/or subscript.
    Script { base: Box<MathNode>, sup: Option<Box<MathNode>>, sub: Option<Box<MathNode>> },
    /// A large operator (\sum, \int, \prod) whose limits stack under and over.
    BigOp { sym: String, under: Option<Box<MathNode>>, over: Option<Box<MathNode>> },
    /// \left( … \right): the delimiters stretch to the body's height.
    Fenced { open: String, close: String, body: Box<MathNode> },
    /// Rows of cells: matrices, cases and aligned blocks.
    Matrix { rows: Vec<Vec<MathNode>>, open: String, close: String },
    /// An accent drawn over the base (\hat, \bar, \vec, \tilde, \dot).
    Accent { base: Box<MathNode>, mark: char },
    /// Horizontal space in em. `literal` marks whitespace that was merely
    /// typed in the source, which TeX ignores but the inline flattener keeps.
    Space { em: f32, literal: bool },
}

impl MathNode {
    fn row(mut items: Vec<MathNode>) -> MathNode {
        if items.len() == 1 {
            items.pop().unwrap()
        } else {
            MathNode::Row(items)
        }
    }

    /// True for an empty row, so callers can drop absent optional parts.
    fn is_empty(&self) -> bool {
        match self {
            MathNode::Row(items) => items.is_empty(),
            MathNode::Sym { text, .. } => text.is_empty(),
            _ => false,
        }
    }
}

/// Named symbols shared by the tree parser and the inline flattener.
const SYMBOLS: [(&str, &str); 116] = [
    ("alpha", "α"), ("beta", "β"), ("gamma", "γ"), ("delta", "δ"), ("epsilon", "ε"),
    ("varepsilon", "ε"), ("zeta", "ζ"), ("eta", "η"), ("theta", "θ"), ("vartheta", "ϑ"),
    ("iota", "ι"), ("kappa", "κ"), ("lambda", "λ"), ("mu", "μ"), ("nu", "ν"), ("xi", "ξ"),
    ("pi", "π"), ("rho", "ρ"), ("sigma", "σ"), ("tau", "τ"), ("upsilon", "υ"), ("phi", "φ"),
    ("varphi", "φ"), ("chi", "χ"), ("psi", "ψ"), ("omega", "ω"), ("Gamma", "Γ"),
    ("Delta", "Δ"), ("Theta", "Θ"), ("Lambda", "Λ"), ("Xi", "Ξ"), ("Pi", "Π"),
    ("Sigma", "Σ"), ("Upsilon", "Υ"), ("Phi", "Φ"), ("Psi", "Ψ"), ("Omega", "Ω"),
    ("times", "×"), ("cdot", "·"), ("pm", "±"), ("mp", "∓"), ("div", "÷"), ("leq", "≤"),
    ("le", "≤"), ("geq", "≥"), ("ge", "≥"), ("neq", "≠"), ("ne", "≠"), ("approx", "≈"),
    ("sim", "∼"), ("simeq", "≃"), ("equiv", "≡"), ("propto", "∝"), ("infty", "∞"),
    ("partial", "∂"), ("nabla", "∇"), ("sum", "∑"), ("prod", "∏"), ("int", "∫"),
    ("oint", "∮"), ("rightarrow", "→"), ("to", "→"), ("leftarrow", "←"),
    ("Rightarrow", "⇒"), ("Leftarrow", "⇐"), ("leftrightarrow", "↔"),
    ("Leftrightarrow", "⇔"), ("mapsto", "↦"), ("in", "∈"), ("notin", "∉"), ("subset", "⊂"),
    ("supset", "⊃"), ("subseteq", "⊆"), ("cup", "∪"), ("cap", "∩"), ("forall", "∀"),
    ("exists", "∃"), ("emptyset", "∅"), ("circ", "∘"), ("degree", "°"), ("ell", "ℓ"),
    ("hbar", "ℏ"), ("Re", "ℜ"), ("Im", "ℑ"), ("angle", "∠"), ("perp", "⊥"),
    ("parallel", "∥"), ("wedge", "∧"), ("vee", "∨"), ("neg", "¬"), ("oplus", "⊕"),
    ("otimes", "⊗"), ("star", "⋆"), ("ast", "∗"), ("prime", "′"), ("ldots", "…"),
    ("top", "⊤"), ("bot", "⊥"), ("mid", "∣"), ("setminus", "∖"), ("cdots", "⋯"),
    ("dots", "…"), ("vdots", "⋮"), ("odot", "⊙"), ("bullet", "∙"), ("varsigma", "ς"),
    ("varrho", "ϱ"), ("varpi", "ϖ"), ("aleph", "ℵ"), ("supseteq", "⊇"), ("gg", "≫"),
    ("ll", "≪"), ("langle", "⟨"), ("rangle", "⟩"), ("lVert", "‖"), ("rVert", "‖"),
];

/// Operators that TeX sets upright and names, e.g. \sin, \log, \max.
const OP_NAMES: [&str; 24] = [
    "sin", "cos", "tan", "cot", "sec", "csc", "arcsin", "arccos", "arctan", "sinh", "cosh",
    "tanh", "log", "ln", "exp", "min", "max", "arg", "det", "dim", "gcd", "lim", "sup", "inf",
];

/// Large operators whose limits stack above and below in display style.
const BIG_OPS: [&str; 8] = ["sum", "prod", "int", "oint", "coprod", "bigcup", "bigcap", "lim"];

/// How many braced arguments a command takes that are pure setup. Anything
/// after them flows as normal content, which is what `\multicolumn{2}{c}{Hi}`
/// and friends want. Getting this wrong is what put stray `3pt` and `0.9cm`
/// into the rendered page.
fn setup_args(name: &str) -> usize {
    match name {
        "definecolor" => 3,
        "setlength" | "addtolength" | "setcounter" | "addtocounter" | "settowidth"
        | "newunicodechar" | "DeclareUnicodeCharacter" | "multicolumn" | "multirow"
        | "resizebox" | "rule" => 2,
        "parbox" | "raisebox" | "scalebox" | "textcolor" | "colorbox" | "fcolorbox"
        | "captionsetup" | "hypersetup" | "lstset" | "newlength" | "newcounter" => 1,
        _ => 0,
    }
}

/// TeX's size commands as multiples of the body size.
fn type_size(name: &str) -> f32 {
    match name {
        "tiny" => 0.6,
        "scriptsize" => 0.72,
        "footnotesize" => 0.84,
        "small" => 0.92,
        "large" => 1.2,
        "Large" => 1.44,
        "LARGE" => 1.73,
        "huge" => 2.07,
        "Huge" => 2.49,
        _ => 1.0,
    }
}

/// Upper-case roman numerals, for the section numbering of the journal
/// classes (I, II, III, ...).
fn roman(mut n: u64) -> String {
    const TABLE: [(u64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (value, sym) in TABLE {
        while n >= value {
            out.push_str(sym);
            n -= value;
        }
    }
    if out.is_empty() {
        out.push('0');
    }
    out
}

/// A, B, ... Z, AA, ... for subsection numbering.
fn letter(n: u64) -> String {
    if n == 0 {
        return "0".into();
    }
    let mut n = n - 1;
    let mut out = Vec::new();
    loop {
        out.push((b'A' + (n % 26) as u8) as char);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    out.iter().rev().collect()
}

/// Braced arguments an environment takes before its content.
fn env_args(env: &str) -> usize {
    match env {
        "adjustwidth" | "tabularx" | "wrapfigure" => 2,
        "minipage" | "multicols" | "subfigure" | "savenotes" | "IEEEeqnarray" => 1,
        _ => 0,
    }
}

/// Delimiters that `\left`/`\right` may stretch.
fn delim_char(name: &str) -> Option<&'static str> {
    Some(match name {
        "(" => "(",
        ")" => ")",
        "[" => "[",
        "]" => "]",
        "\\{" | "{" | "lbrace" => "{",
        "\\}" | "}" | "rbrace" => "}",
        "|" | "vert" => "|",
        "\\|" | "Vert" => "‖",
        "langle" => "⟨",
        "rangle" => "⟩",
        "lceil" => "⌈",
        "rceil" => "⌉",
        "lfloor" => "⌊",
        "rfloor" => "⌋",
        "." => "",
        _ => return None,
    })
}

/// Matrix-like environments and the delimiters they imply.
fn matrix_delims(env: &str) -> Option<(&'static str, &'static str)> {
    Some(match env {
        "matrix" | "aligned" | "align" | "align*" | "gathered" | "array" | "smallmatrix" => ("", ""),
        "pmatrix" => ("(", ")"),
        "bmatrix" => ("[", "]"),
        "Bmatrix" => ("{", "}"),
        "vmatrix" => ("|", "|"),
        "Vmatrix" => ("‖", "‖"),
        "cases" => ("{", ""),
        _ => return None,
    })
}

struct MathParser {
    chars: Vec<char>,
    pos: usize,
    /// Delimiter from the `\right` that ended the current row.
    pending_close: Option<String>,
    /// Set when the row ended at `\\`, so matrices know to start a new row.
    took_row_break: bool,
}

/// Parses LaTeX math source into a layout tree. Unknown commands survive as
/// their own name so no information is silently dropped.
pub fn parse_math(src: &str) -> MathNode {
    let mut p = MathParser {
        chars: src.chars().collect(),
        pos: 0,
        pending_close: None,
        took_row_break: false,
    };
    let mut items = p.parse_row(&[]);
    // A `\\` inside a construct that is not set as rows of its own — an
    // amsmath `split`, say — would otherwise truncate the formula there. Keep
    // reading and set the rest on the same line rather than losing it.
    while p.pos < p.chars.len() {
        let at = p.pos;
        let mut more = p.parse_row(&[]);
        if p.pos == at {
            p.pos += 1;
        }
        if !more.is_empty() {
            items.push(MathNode::Space { em: 0.6, literal: false });
            items.append(&mut more);
        }
    }
    MathNode::row(items)
}

impl MathParser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    /// Reads a command name at `pos`, assuming the backslash was consumed.
    fn read_name(&mut self) -> String {
        match self.peek() {
            Some(c) if c.is_ascii_alphabetic() => {
                let start = self.pos;
                while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                    self.pos += 1;
                }
                self.chars[start..self.pos].iter().collect()
            }
            // A single non-letter escape: \{, \|, \, and friends.
            Some(c) => {
                self.pos += 1;
                c.to_string()
            }
            None => String::new(),
        }
    }

    /// Parses one argument: a braced group, or a single atom.
    fn parse_arg(&mut self) -> MathNode {
        while self.peek() == Some(' ') {
            self.pos += 1;
        }
        if self.peek() == Some('{') {
            self.pos += 1;
            let items = self.parse_row(&['}']);
            if self.peek() == Some('}') {
                self.pos += 1;
            }
            return MathNode::row(items);
        }
        match self.peek() {
            Some('\\') => {
                self.pos += 1;
                let name = self.read_name();
                self.command(&name)
            }
            Some(c) => {
                self.pos += 1;
                self.char_atom(c)
            }
            None => MathNode::Row(Vec::new()),
        }
    }

    /// An optional `[...]` argument, used by \sqrt.
    fn parse_opt(&mut self) -> Option<MathNode> {
        if self.peek() != Some('[') {
            return None;
        }
        self.pos += 1;
        let items = self.parse_row(&[']']);
        if self.peek() == Some(']') {
            self.pos += 1;
        }
        Some(MathNode::row(items))
    }

    fn char_atom(&self, c: char) -> MathNode {
        // TeX sets the binary operator, not the hyphen the keyboard offers.
        let text = if c == '-' { '\u{2212}' } else { c };
        MathNode::Sym { text: text.to_string(), italic: c.is_ascii_alphabetic() }
    }

    /// Expands one command into a node.
    fn command(&mut self, name: &str) -> MathNode {
        match name {
            "frac" | "dfrac" | "tfrac" | "cfrac" => {
                let num = self.parse_arg();
                let den = self.parse_arg();
                MathNode::Frac { num: Box::new(num), den: Box::new(den) }
            }
            "sqrt" => {
                let index = self.parse_opt().map(Box::new);
                let arg = self.parse_arg();
                MathNode::Sqrt { index, arg: Box::new(arg) }
            }
            "left" => {
                let open = self.read_delim();
                let items = self.parse_row(&[]);
                let close = self.pending_close.take().unwrap_or_default();
                MathNode::Fenced { open, close, body: Box::new(MathNode::row(items)) }
            }
            "begin" => {
                let env = self.read_brace_word();
                match matrix_delims(&env) {
                    Some((open, close)) => {
                        let rows = self.parse_matrix(&env);
                        MathNode::Matrix {
                            rows,
                            open: open.to_string(),
                            close: close.to_string(),
                        }
                    }
                    None => {
                        // Unknown environment: parse its body inline.
                        let items = self.parse_row(&[]);
                        MathNode::row(items)
                    }
                }
            }
            "text" | "mathrm" | "operatorname" | "mathsf" | "mathtt" | "textrm" | "mbox" => {
                let arg = self.parse_arg();
                upright(arg)
            }
            // Blackboard bold and calligraphic letters have their own
            // characters; an italic R where the source said \mathbb{R} reads
            // as a variable rather than as the reals.
            "mathbb" => letterlike(self.parse_arg(), BLACKBOARD),
            "mathcal" | "mathscr" => letterlike(self.parse_arg(), CALLIGRAPHIC),
            "mathbf" | "boldsymbol" | "bm" | "mathit" => self.parse_arg(),
            // Spacing accents, not combining ones: a lone combining mark
            // shapes to nothing, so \widehat{E} came out as a bare E.
            "hat" | "widehat" => self.accent('\u{02C6}'),
            "bar" | "overline" => self.accent('\u{00AF}'),
            "vec" => self.accent('\u{2192}'),
            "tilde" | "widetilde" => self.accent('\u{02DC}'),
            "dot" => self.accent('\u{02D9}'),
            "ddot" => self.accent('\u{00A8}'),
            "quad" => MathNode::Space { em: 1.0, literal: false },
            "qquad" => MathNode::Space { em: 2.0, literal: false },
            "," | ":" | ";" | "thinspace" => MathNode::Space { em: 0.22, literal: false },
            "!" => MathNode::Space { em: -0.17, literal: false },
            " " => MathNode::Space { em: 0.33, literal: false },
            "label" | "tag" | "nonumber" | "notag" | "limits" | "nolimits" | "displaystyle"
            | "textstyle" | "scriptstyle" | "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr"
            | "Bigl" | "Bigr" => {
                if self.peek() == Some('{') {
                    let _ = self.parse_arg();
                }
                MathNode::Row(Vec::new())
            }
            "\\" => MathNode::Row(Vec::new()),
            _ => {
                if let Some((_, sym)) = SYMBOLS.iter().find(|(k, _)| *k == name) {
                    return MathNode::Sym { text: (*sym).to_string(), italic: false };
                }
                if OP_NAMES.contains(&name) {
                    return MathNode::Sym { text: name.to_string(), italic: false };
                }
                // An escaped literal (\%, \_, \{) or an unknown command name.
                MathNode::Sym { text: name.to_string(), italic: false }
            }
        }
    }

    fn accent(&mut self, mark: char) -> MathNode {
        let base = self.parse_arg();
        MathNode::Accent { base: Box::new(base), mark }
    }

    /// Reads the delimiter following \left or \right.
    fn read_delim(&mut self) -> String {
        while self.peek() == Some(' ') {
            self.pos += 1;
        }
        match self.peek() {
            Some('\\') => {
                self.pos += 1;
                let name = self.read_name();
                delim_char(&format!("\\{name}"))
                    .or_else(|| delim_char(&name))
                    .unwrap_or("")
                    .to_string()
            }
            Some(c) => {
                self.pos += 1;
                delim_char(&c.to_string()).unwrap_or("").to_string()
            }
            None => String::new(),
        }
    }

    /// Reads a `{word}` argument as plain text, for \begin{env}.
    fn read_brace_word(&mut self) -> String {
        if self.peek() != Some('{') {
            return String::new();
        }
        self.pos += 1;
        let start = self.pos;
        while self.peek().is_some_and(|c| c != '}') {
            self.pos += 1;
        }
        let word: String = self.chars[start..self.pos].iter().collect();
        if self.peek() == Some('}') {
            self.pos += 1;
        }
        word
    }

    /// Parses matrix rows until \end{env}, splitting on `&` and `\\`.
    fn parse_matrix(&mut self, env: &str) -> Vec<Vec<MathNode>> {
        let mut rows: Vec<Vec<MathNode>> = Vec::new();
        let mut row: Vec<MathNode> = Vec::new();
        loop {
            let items = self.parse_row(&['&']);
            row.push(MathNode::row(items));
            match self.peek() {
                Some('&') => {
                    self.pos += 1;
                }
                _ => {
                    // parse_row stopped at \\ or \end.
                    if self.took_row_break {
                        self.took_row_break = false;
                        rows.push(std::mem::take(&mut row));
                        continue;
                    }
                    let _ = env;
                    if !row.iter().all(MathNode::is_empty) {
                        rows.push(std::mem::take(&mut row));
                    }
                    return rows;
                }
            }
        }
    }

    /// Parses a sequence of atoms until one of `stop` (or a structural end).
    fn parse_row(&mut self, stop: &[char]) -> Vec<MathNode> {
        let mut items: Vec<MathNode> = Vec::new();
        while let Some(c) = self.peek() {
            if stop.contains(&c) {
                break;
            }
            match c {
                // `]` and `&` end a row only where the caller asked for it
                // (a \sqrt index, a matrix cell); elsewhere they are ordinary
                // characters and dropping them truncates the formula.
                '}' => break,
                '{' => {
                    self.pos += 1;
                    let inner = self.parse_row(&['}']);
                    if self.peek() == Some('}') {
                        self.pos += 1;
                    }
                    items.push(MathNode::row(inner));
                }
                '^' | '_' => {
                    self.pos += 1;
                    let script = self.parse_arg();
                    let base = items.pop().unwrap_or(MathNode::Row(Vec::new()));
                    let (base, mut sup, mut sub) = match base {
                        MathNode::Script { base, sup, sub } => (*base, sup, sub),
                        other => (other, None, None),
                    };
                    if c == '^' {
                        sup = Some(Box::new(script));
                    } else {
                        sub = Some(Box::new(script));
                    }
                    items.push(match base {
                        // Limits belong under/over a big operator.
                        MathNode::Sym { ref text, .. }
                            if BIG_OPS
                                .iter()
                                .any(|k| SYMBOLS.iter().any(|(n, s)| n == k && s == text))
                                || text == "lim" =>
                        {
                            MathNode::BigOp { sym: text.clone(), under: sub, over: sup }
                        }
                        base => MathNode::Script { base: Box::new(base), sup, sub },
                    });
                }
                '\\' => {
                    self.pos += 1;
                    let name = self.read_name();
                    if name == "end" {
                        let _ = self.read_brace_word();
                        break;
                    }
                    if name == "right" {
                        self.pending_close = Some(self.read_delim());
                        break;
                    }
                    if name == "\\" {
                        self.took_row_break = true;
                        // Drop an optional [2pt] spacing argument.
                        if self.peek() == Some('[') {
                            while self.peek().is_some_and(|c| c != ']') {
                                self.pos += 1;
                            }
                            self.pos += 1;
                        }
                        break;
                    }
                    let node = self.command(&name);
                    if !node.is_empty() {
                        items.push(node);
                    }
                }
                ' ' | '\t' | '\n' | '\r' => {
                    self.pos += 1;
                    items.push(MathNode::Space { em: 0.0, literal: true });
                }
                '&' => {
                    // An alignment tab outside a matrix marks the column the
                    // rows line up on. Each row is set on its own here, so the
                    // tab itself contributes nothing: the relation that
                    // follows it already carries TeX's spacing.
                    self.pos += 1;
                }
                _ => {
                    self.pos += 1;
                    items.push(self.char_atom(c));
                }
            }
        }
        items
    }
}

/// Blackboard-bold and calligraphic letters, for \mathbb and \mathcal. Only
/// the letters that have a plain character of their own are mapped; the rest
/// fall back to upright roman, which still reads as a set rather than as a
/// variable.
const BLACKBOARD: &[(char, char)] = &[
    ('C', '\u{2102}'), ('H', '\u{210D}'), ('N', '\u{2115}'), ('P', '\u{2119}'),
    ('Q', '\u{211A}'), ('R', '\u{211D}'), ('Z', '\u{2124}'),
];
const CALLIGRAPHIC: &[(char, char)] = &[
    ('B', '\u{212C}'), ('E', '\u{2130}'), ('F', '\u{2131}'), ('H', '\u{210B}'),
    ('I', '\u{2110}'), ('L', '\u{2112}'), ('M', '\u{2133}'), ('R', '\u{211B}'),
];

/// Substitutes a letterlike alphabet into a node, leaving anything the
/// alphabet has no character for upright.
fn letterlike(node: MathNode, table: &[(char, char)]) -> MathNode {
    match node {
        MathNode::Sym { text, .. } => MathNode::Sym {
            text: text
                .chars()
                .map(|c| table.iter().find(|(k, _)| *k == c).map_or(c, |(_, v)| *v))
                .collect(),
            italic: false,
        },
        MathNode::Row(items) => {
            MathNode::Row(items.into_iter().map(|n| letterlike(n, table)).collect())
        }
        other => upright(other),
    }
}

/// Renders a node with every letter upright (\text, \mathrm).
fn upright(node: MathNode) -> MathNode {
    match node {
        MathNode::Sym { text, .. } => MathNode::Sym { text, italic: false },
        MathNode::Row(items) => MathNode::Row(items.into_iter().map(upright).collect()),
        other => other,
    }
}

/// Flattens a formula into runs tagged with whether they are variables.
/// TeX italicizes variables but sets numbers, operators and function names
/// like `min` or `tanh` upright; one italic blob gets that visibly wrong.
pub fn math_spans(node: &MathNode) -> Vec<MathSpan> {
    let mut out: Vec<MathSpan> = Vec::new();
    collect_spans(node, &mut out, 0);
    // Merge neighbours that share a style, and drop doubled spaces.
    let mut merged: Vec<MathSpan> = Vec::new();
    for span in out {
        if span.text.is_empty() {
            continue;
        }
        match merged.last_mut() {
            Some(prev) if prev.italic == span.italic && prev.script == span.script => {
                prev.text.push_str(&span.text)
            }
            _ => merged.push(span),
        }
    }
    merged
}

/// A run of a formula flattened for inline setting: the text, whether TeX
/// would slant it, and whether it sits above or below the baseline.
pub struct MathSpan {
    pub text: String,
    pub italic: bool,
    /// Where the run sits relative to the baseline: see [`nested_script`].
    pub script: i8,
}

/// The level a script nested inside another script is set at.
///
/// Inline math is set as runs of ordinary text that the painter raises and
/// lowers, so the levels are a small fixed set rather than a depth: 0 on the
/// baseline, ±1 for a plain superscript or subscript, and four more for the
/// second level, which is what `\mathbb{R}^{d_{model}}` needs. Deeper than
/// that a script keeps its parent's level; the alternative was spelling the
/// nested script out as `d_(model)`, which is what this replaces.
pub fn nested_script(parent: i8, sup: bool) -> i8 {
    match (parent, sup) {
        (0, true) => 1,
        (0, false) => -1,
        (1, true) => 2,   // x^{y^z}
        (1, false) => 3,  // x^{y_z}
        (-1, true) => -3, // x_{y^z}
        (-1, false) => -2,
        (deeper, _) => deeper,
    }
}

fn collect_spans(node: &MathNode, out: &mut Vec<MathSpan>, script: i8) {
    let mut push = |text: String, italic: bool| out.push(MathSpan { text, italic, script });
    match node {
        MathNode::Sym { text, italic } => push(text.clone(), *italic),
        MathNode::Row(items) => items.iter().for_each(|i| collect_spans(i, out, script)),
        MathNode::Space { .. } => push(flatten_math(node), false),
        // Descend so a scripted variable keeps its slant: the `x` of `x^2`
        // is still a variable, even though the exponent is not.
        MathNode::Script { base, sup, sub } => {
            collect_spans(base, out, script);
            if let Some(sb) = sub {
                collect_spans(sb, out, nested_script(script, false));
            }
            if let Some(sp) = sup {
                collect_spans(sp, out, nested_script(script, true));
            }
        }
        MathNode::Accent { base, .. } => collect_spans(base, out, script),
        MathNode::Fenced { open, close, body } => {
            out.push(MathSpan { text: open.clone(), italic: false, script });
            collect_spans(body, out, script);
            out.push(MathSpan { text: close.clone(), italic: false, script });
        }
        // Everything else keeps the flattened form; its internals are
        // already positional rather than stylistic.
        other => push(flatten_math(other), false),
    }
}

/// The flattened Unicode form of an already-parsed formula, for copying.
pub fn math_text(node: &MathNode) -> String {
    let out = flatten_math(node);
    let mut tidy = String::with_capacity(out.len());
    let mut prev_space = false;
    for ch in out.trim().chars() {
        let space = ch == ' ';
        if !(space && prev_space) {
            tidy.push(ch);
        }
        prev_space = space;
    }
    tidy
}

/// Flattens a math tree to a single Unicode line.
fn flatten_math(node: &MathNode) -> String {
    match node {
        MathNode::Sym { text, .. } => text.clone(),
        MathNode::Row(items) => items.iter().map(flatten_math).collect(),
        MathNode::Space { em, literal } => {
            if *literal {
                " ".to_string()
            } else if *em <= 0.0 {
                String::new()
            } else {
                " ".repeat((em.round() as usize).max(1))
            }
        }
        MathNode::Frac { num, den } => {
            let wrap = |s: String| if s.chars().count() > 1 { format!("({s})") } else { s };
            format!("{}/{}", wrap(flatten_math(num)), wrap(flatten_math(den)))
        }
        MathNode::Sqrt { arg, .. } => format!("√({})", flatten_math(arg)),
        MathNode::Accent { base, .. } => flatten_math(base),
        MathNode::Fenced { open, close, body } => {
            format!("{open}{}{close}", flatten_math(body))
        }
        MathNode::Matrix { rows, open, close } => {
            let body = rows
                .iter()
                .map(|r| r.iter().map(flatten_math).collect::<Vec<_>>().join(", "))
                .collect::<Vec<_>>()
                .join("; ");
            format!("{open}{body}{close}")
        }
        MathNode::BigOp { sym, under, over } => {
            let mut out = sym.clone();
            if let Some(u) = under {
                out.push_str(&script_text(&flatten_math(u), false));
            }
            if let Some(o) = over {
                out.push_str(&script_text(&flatten_math(o), true));
            }
            out
        }
        MathNode::Script { base, sup, sub } => {
            let mut out = flatten_math(base);
            if let Some(sb) = sub {
                out.push_str(&script_text(&flatten_math(sb), false));
            }
            if let Some(sp) = sup {
                out.push_str(&script_text(&flatten_math(sp), true));
            }
            out
        }
    }
}

/// Maps a script to Unicode super/subscript glyphs, falling back to `^x` or
/// `^(xy)` when the characters have no such form.
fn script_text(text: &str, sup: bool) -> String {
    let table: &[(char, char)] = if sup { &SUPERSCRIPTS } else { &SUBSCRIPTS };
    let mapped: Option<String> = text
        .chars()
        .map(|t| table.iter().find(|(k, _)| *k == t).map(|(_, v)| *v))
        .collect();
    match mapped {
        Some(m) if !m.is_empty() => m,
        // A degree sign already sits raised: x^\circ.
        _ if sup && text == "°" => "°".to_string(),
        _ => {
            let marker = if sup { '^' } else { '_' };
            if text.chars().count() > 1 {
                format!("{marker}({text})")
            } else {
                format!("{marker}{text}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn sections_and_styles() {
        let blocks = parse(
            "\\documentclass{article}\n\\begin{document}\n\\section{Intro}\nHello \\textbf{bold} and \\emph{it}.\n\\end{document}\n",
        );
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            TexBlock::Heading { level, spans } => {
                assert_eq!(*level, 1);
                assert_eq!(text_of(spans), "1  Intro");
            }
            other => panic!("expected heading, got {other:?}"),
        }
        match &blocks[1] {
            TexBlock::Paragraph { spans, .. } => {
                assert_eq!(text_of(spans), "Hello bold and it.");
                assert!(spans.iter().any(|s| s.bold && s.text == "bold"));
                assert!(spans.iter().any(|s| s.italic && s.text == "it"));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn lists_and_verbatim() {
        let blocks = parse(
            "\\begin{itemize}\n\\item one\n\\item two\n\\end{itemize}\n\\begin{verbatim}\nlet x = 1;\n\\end{verbatim}\n",
        );
        let items: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                TexBlock::ListItem { marker, spans, .. } => {
                    Some(format!("{marker}{}", text_of(spans)))
                }
                _ => None,
            })
            .collect();
        assert_eq!(items, ["•  one", "•  two"]);
        assert!(blocks.iter().any(|b| matches!(b, TexBlock::Code(c) if c == "let x = 1;")));
    }

    #[test]
    fn math_translation() {
        let flat = |src: &str| math_text(&parse_math(src));
        assert_eq!(flat("E = mc^2"), "E = mc²");
        assert_eq!(flat("\\alpha + \\beta \\to \\infty"), "α + β → ∞");
        assert_eq!(flat("\\frac{a+b}{2}"), "(a+b)/2");
        assert_eq!(flat("x_i \\leq y^{10}"), "xᵢ ≤ y¹⁰");
        assert_eq!(flat("\\sqrt{2}"), "√(2)");
    }

    #[test]
    fn comments_stripped_and_escapes_kept() {
        let blocks = parse("50\\% done % a comment\nnext line\n");
        match &blocks[0] {
            TexBlock::Paragraph { spans, .. } => assert_eq!(text_of(spans), "50% done next line"),
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn tabular_rows_and_cells() {
        let blocks = parse(
            "\\begin{tabular}{ll}\n\\hline\nA & B \\\\\n1 & 2 \\\\\n\\hline\n\\end{tabular}\n",
        );
        match &blocks[0] {
            TexBlock::Table { rows, .. } => {
                assert_eq!(rows.len(), 2);
                assert_eq!(text_of(&rows[0][0]), "A");
                assert_eq!(text_of(&rows[0][1]), "B");
                assert_eq!(text_of(&rows[1][1]), "2");
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn inline_and_display_math() {
        let blocks = parse("Given $x^2$ we have\n\\[ y = \\alpha x \\]\ndone.\n");
        match &blocks[0] {
            TexBlock::Paragraph { spans, .. } => {
                assert!(text_of(spans).contains("x2"), "got {}", text_of(spans));
                // The variable slants; the exponent is a raised, upright run.
                assert!(spans.iter().any(|s| s.italic && s.script == 0 && s.text.contains('x')));
                assert!(spans.iter().any(|s| s.script == 1 && s.text.contains('2')));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
        assert!(blocks
            .iter()
            .any(|b| matches!(b, TexBlock::Math { node, .. } if math_text(node) == "y = α x")));
    }

    /// The constructs that leaked raw LaTeX into the rendered page: enumitem
    /// key-value lists, float placement, and natbib's two optional arguments.
    #[test]
    fn optional_arguments_never_leak_into_the_text() {
        let blocks = parse(
            "\\begin{figure}[htbp]\n\\end{figure}\n\\begin{itemize}[leftmargin=1.4em,itemsep=1pt]\n\\item one\n\\end{itemize}\nSee \\citep[][]{sdfc,mff} and \\begin{tabular}[t]{ll}a & b \\\\\\end{tabular}\n",
        );
        let all: String = blocks
            .iter()
            .map(|b| match b {
                TexBlock::Paragraph { spans: s, .. } | TexBlock::ListItem { spans: s, .. } => text_of(s),
                TexBlock::Table { rows, .. } => {
                    rows.iter().flatten().map(|c| text_of(c)).collect::<String>()
                }
                _ => String::new(),
            })
            .collect();
        assert!(!all.contains("leftmargin"), "enumitem options leaked: {all}");
        assert!(!all.contains("htbp"), "float placement leaked: {all}");
        assert!(!all.contains("]sdfc"), "empty cite options leaked: {all}");
        assert!(all.contains("[sdfc, mff]"), "citation keys missing: {all}");
        assert!(all.contains("one"), "list item text missing: {all}");
    }

    /// A bracketed group inside inline math must survive: `[0,1]^{...}` used
    /// to lose everything from the closing bracket onward.
    #[test]
    fn brackets_and_scripts_survive_inline_math() {
        let blocks = parse("Given $x \\in [0,1]^{B \\times C}$ done.\n");
        let TexBlock::Paragraph { spans, .. } = &blocks[0] else { panic!("expected paragraph") };
        let text = text_of(spans);
        assert!(text.contains("[0,1]"), "bracket group lost: {text}");
        assert!(text.contains('B') && text.contains('C'), "script lost: {text}");
        assert!(text.contains("done."), "text after math lost: {text}");
    }

    /// Variables slant; numbers, operators and function names do not.
    #[test]
    fn inline_math_italicizes_only_variables() {
        let blocks = parse("Let $g = \\min(8, H)$.\n");
        let TexBlock::Paragraph { spans, .. } = &blocks[0] else { panic!("expected paragraph") };
        let italic_of = |needle: &str| {
            spans.iter().find(|s| s.text.contains(needle)).map(|s| s.italic)
        };
        assert_eq!(italic_of("min"), Some(false), "function name must be upright");
        assert_eq!(italic_of("g"), Some(true), "variable must be italic");
    }

    /// Display math becomes a layout tree, numbered like LaTeX numbers it.
    #[test]
    fn display_math_is_a_tree_and_numbered() {
        let blocks = parse(
            "\\begin{equation}\nx = \\frac{a}{b}\n\\end{equation}\n\\begin{equation*}\ny = 1\n\\end{equation*}\n\\begin{equation}\nz = 2\n\\end{equation}\n",
        );
        let math: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                TexBlock::Math { node, number } => Some((node, number.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(math.len(), 3);
        assert_eq!(math[0].1.as_deref(), Some("(1)"));
        assert_eq!(math[1].1, None, "starred form is unnumbered");
        assert_eq!(math[2].1.as_deref(), Some("(2)"), "numbering skips starred forms");
        assert!(
            matches!(math[0].0, MathNode::Row(_)),
            "fraction should parse into a tree, not a flat string"
        );
        assert_eq!(math_text(math[0].0), "x = a/b");
    }

    /// Every row of an `align` is its own numbered equation. The parser used
    /// to stop at the first `\\`, so the rest of the environment — and any
    /// \label in it — was silently dropped.
    #[test]
    fn align_sets_every_row_as_its_own_equation() {
        let blocks = parse(concat!(
            "\\begin{align}\n",
            "a &= 1 \\label{eq:a}\\\\\n",
            "b &= 2 \\label{eq:b}\\\\\n",
            "c &= 3 \\nonumber\n",
            "\\end{align}\n",
            "See Eq.~\\eqref{eq:b}.\n",
        ));
        let math: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                TexBlock::Math { node, number } => Some((math_text(node), number.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(math.len(), 3, "rows after the first were dropped: {math:?}");
        assert_eq!(math[0].1.as_deref(), Some("(1)"));
        assert_eq!(math[1].1.as_deref(), Some("(2)"));
        assert_eq!(math[2].1, None, "\\nonumber row must not be numbered");
        assert_eq!(math[1].0, "b = 2");
        let TexBlock::Paragraph { spans, .. } = blocks.last().unwrap() else {
            panic!("expected a paragraph")
        };
        assert!(text_of(spans).contains("(2)"), "label in a later row never bound");
    }

    /// A row break inside a matrix belongs to the matrix, not to the align.
    #[test]
    fn a_row_break_inside_a_nested_environment_is_not_a_row() {
        let blocks = parse(concat!(
            "\\begin{align}\n",
            "M &= \\begin{pmatrix} 1 & 0 \\\\ 0 & 1 \\end{pmatrix}\\\\\n",
            "N &= 0\n",
            "\\end{align}\n",
        ));
        let rows = blocks.iter().filter(|b| matches!(b, TexBlock::Math { .. })).count();
        assert_eq!(rows, 2, "the matrix's own break split the align");
    }

    /// A script inside a script keeps a level of its own. Spelling it out as
    /// `d_(model)` is what this replaced.
    #[test]
    fn a_script_inside_a_script_is_set_as_one() {
        let node = parse_math("R^{d_{model} \\times 1}");
        let spans = math_spans(&node);
        let all: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert!(!all.contains("_("), "nested script fell back to plain text: {all}");
        assert!(all.contains("model"), "nested script lost: {all}");
        let level = |needle: &str| {
            spans.iter().find(|s| s.text.contains(needle)).map(|s| s.script)
        };
        assert_eq!(level("d"), Some(1), "exponent should be a superscript");
        assert_eq!(level("model"), Some(3), "its index should be a second level");
    }

    /// \mathbb and \mathcal have characters of their own; an italic R where
    /// the source said \mathbb{R} reads as a variable, not as the reals.
    #[test]
    fn blackboard_and_calligraphic_letters_get_their_own_characters() {
        assert_eq!(math_text(&parse_math("\\mathbb{R}^{n}")), "ℝⁿ");
        assert_eq!(math_text(&parse_math("\\mathcal{L}")), "ℒ");
        // No character of its own: upright, rather than slanted like a variable.
        let node = parse_math("\\mathbb{S}");
        assert!(matches!(&node, MathNode::Sym { text, italic: false } if text == "S"));
    }

    /// Cross-references resolve to numbers, including forward ones — a paper
    /// almost always cites a figure before the float that defines it.
    #[test]
    fn references_resolve_to_numbers() {
        let blocks = parse(concat!(
            "\\section{Intro}\\label{sec:intro}\n",
            "See \\ref{fig:arch} and Table~\\ref{tab:layers}, also \\autoref{fig:arch}, ",
            "\\eqref{eq:main} and \\ref{sec:intro}.\n",
            "\\begin{equation}\\label{eq:main}\nx = 1\n\\end{equation}\n",
            "\\begin{figure}\\caption{Arch}\\label{fig:arch}\\end{figure}\n",
            "\\begin{table}\\caption{Layers}\\label{tab:layers}\\end{table}\n",
        ));
        let para = blocks
            .iter()
            .find_map(|b| match b {
                TexBlock::Paragraph { spans: s, .. } => Some(text_of(s)),
                _ => None,
            })
            .expect("paragraph");
        assert!(!para.contains("fig:arch"), "raw label key still shown: {para}");
        assert!(!para.contains("tab:layers"), "raw label key still shown: {para}");
        // `~` is a non-breaking space, so it renders as an ordinary one.
        assert!(para.contains("See 1 and Table 1"), "bare \\ref numbers: {para}");
        assert!(para.contains("Figure 1"), "\\autoref should name the float: {para}");
        assert!(para.contains("(1)"), "\\eqref should parenthesize: {para}");

        // Captions carry the float's own number.
        let caps: Vec<String> = blocks
            .iter()
            .filter_map(|b| match b {
                TexBlock::Caption { spans, .. } => Some(text_of(spans)),
                _ => None,
            })
            .collect();
        assert_eq!(caps, ["Figure 1: Arch", "Table 1: Layers"]);
    }

    /// An unknown key keeps showing, rather than vanishing: it is usually
    /// defined in a file we were not given.
    #[test]
    fn unresolved_reference_keeps_its_key() {
        let blocks = parse("See \\ref{fig:elsewhere}.\n");
        let TexBlock::Paragraph { spans, .. } = &blocks[0] else { panic!("expected paragraph") };
        assert!(text_of(spans).contains("[fig:elsewhere]"));
    }

    /// Figures record the column fraction they asked for, so a
    /// `width=\linewidth` plot fills the column instead of sitting tiny.
    #[test]
    fn includegraphics_width_is_a_column_fraction() {
        let widths: Vec<Option<f32>> = parse(concat!(
            "\\includegraphics[width=\\linewidth]{a}\n\n",
            "\\includegraphics[width=0.8\\textwidth]{b}\n\n",
            "\\includegraphics[scale=0.5]{c}\n\n",
            "\\includegraphics{d}\n\n",
        ))
        .iter()
        .filter_map(|b| match b {
            TexBlock::Image { width, .. } => Some(*width),
            _ => None,
        })
        .collect();
        assert_eq!(widths, [Some(1.0), Some(0.8), None, None]);
    }

    /// Setup commands take more than one braced argument; reading only the
    /// first dropped stray `3pt` / `0.9cm` lengths into the rendered page.
    #[test]
    fn multi_argument_setup_commands_leak_nothing() {
        let blocks = parse(concat!(
            "{\\footnotesize\\setlength{\\tabcolsep}{3pt}\n",
            "\\setcounter{topnumber}{3}\n",
            "\\definecolor{mine}{rgb}{1,0,0}\n",
            "\\def\\labelenumi{\\arabic{enumi}.}\n",
            "\\begin{minipage}{0.9cm}Kept text.\\end{minipage}}\n",
        ));
        let all: String = blocks
            .iter()
            .filter_map(|b| match b {
                TexBlock::Paragraph { spans: s, .. } => Some(text_of(s)),
                _ => None,
            })
            .collect();
        for stray in ["3pt", "0.9cm", "topnumber", "enumi", "rgb"] {
            assert!(!all.contains(stray), "{stray} leaked into the page: {all:?}");
        }
        assert!(all.contains("Kept text."), "real content was dropped: {all:?}");
    }

    /// `\multicolumn{2}{c}{Header}` must drop the span and column spec but
    /// keep the cell's text.
    #[test]
    fn multicolumn_keeps_its_content() {
        let blocks = parse("\\begin{tabular}{ll}\\multicolumn{2}{c}{Header} \\\\ a & b \\\\\\end{tabular}\n");
        let TexBlock::Table { rows, .. } = &blocks[0] else { panic!("expected table, got {blocks:?}") };
        let first = text_of(&rows[0][0]);
        assert!(first.contains("Header"), "content lost: {first:?}");
        assert!(!first.contains('c') || !first.contains('2'), "spec leaked: {first:?}");
    }

    /// pandoc writes a display formula as `\(…\)` alone in a paragraph; that
    /// should be typeset as a display equation, not squeezed into a text line.
    #[test]
    fn a_paragraph_that_is_only_a_formula_becomes_display_math() {
        let blocks = parse(concat!(
            "Then\n\n",
            "\\(\\mathbf{y} = \\mathrm{clip}\\left(x^{\\gamma}\\cdot\\alpha,\\; 0,\\; 1\\right)\\)\n\n",
            "and inline \\(x^2\\) stays inline.\n",
        ));
        let math: Vec<String> = blocks
            .iter()
            .filter_map(|b| match b {
                TexBlock::Math { node, .. } => Some(math_text(node)),
                _ => None,
            })
            .collect();
        assert_eq!(math.len(), 1, "exactly the standalone formula: {blocks:?}");
        assert!(math[0].contains("clip"), "{:?}", math[0]);
        let last = blocks.last().expect("trailing paragraph");
        assert!(
            matches!(last, TexBlock::Paragraph { spans: s, .. } if text_of(s).contains("stays inline")),
            "inline math must not be promoted: {last:?}"
        );
    }

    #[test]
    fn maketitle_uses_preamble() {
        let blocks = parse(
            "\\title{My Paper}\n\\author{Ada}\n\\begin{document}\n\\maketitle\nBody.\n\\end{document}\n",
        );
        match &blocks[0] {
            TexBlock::Heading { level: 0, spans } => assert_eq!(text_of(spans), "My Paper"),
            other => panic!("expected title heading, got {other:?}"),
        }
        match &blocks[1] {
            TexBlock::Byline { spans, small } => {
                assert_eq!(text_of(spans), "Ada");
                assert!(!small);
            }
            other => panic!("expected byline, got {other:?}"),
        }
    }

    /// \thanks inside \author is the affiliation footnote: it leaves the
    /// byline and comes back as the small line under it.
    #[test]
    fn thanks_becomes_a_footnote_line() {
        let blocks = parse(
            "\\title{T}\\author{Ada\\thanks{Dept of Maths}}\\begin{document}\\maketitle\\end{document}",
        );
        let byline = blocks
            .iter()
            .find_map(|b| match b {
                TexBlock::Byline { spans, small: false } => Some(text_of(spans)),
                _ => None,
            })
            .expect("byline");
        assert_eq!(byline.trim(), "Ada");
        let note = blocks
            .iter()
            .find_map(|b| match b {
                TexBlock::Byline { spans, small: true } => Some(text_of(spans)),
                _ => None,
            })
            .expect("thanks note");
        assert!(note.contains("Dept of Maths"), "note lost: {note}");
    }

    /// A two-column journal class changes the page, the numbering and the
    /// float labels; an article keeps 1, 1.1 and "Figure 1:".
    #[test]
    fn journal_classes_use_their_own_page_and_numbering() {
        let ieee = document_style("\\documentclass[journal,10pt]{IEEEtran}");
        assert_eq!(ieee.columns, 2);
        assert!(ieee.margin_x < 60.0, "journal margins are narrow: {ieee:?}");

        let source = concat!(
            "\\documentclass{IEEEtran}\\begin{document}\\section{First}\\subsection{Part}",
            "\\begin{figure*}\\caption{A wide one}\\end{figure*}\\end{document}",
        );
        let blocks = parse_with(source, &ieee);
        let headings: Vec<String> = blocks
            .iter()
            .filter_map(|b| match b {
                TexBlock::Heading { spans, .. } => Some(text_of(spans)),
                _ => None,
            })
            .collect();
        assert_eq!(headings, vec!["I. First".to_string(), "A. Part".to_string()]);
        let caption = blocks
            .iter()
            .find_map(|b| match b {
                TexBlock::Caption { spans, float } => Some((text_of(spans), float.wide)),
                _ => None,
            })
            .expect("caption");
        assert_eq!(caption.0, "Fig. 1. A wide one");
        assert!(caption.1, "a starred float spans both columns");

        let article = parse("\\section{First}\\begin{figure}\\caption{Plain}\\end{figure}");
        let text: String = article
            .iter()
            .map(|b| match b {
                TexBlock::Heading { spans, .. } | TexBlock::Caption { spans, .. } => text_of(spans),
                _ => String::new(),
            })
            .collect();
        assert!(text.contains("1  First"), "article numbering: {text}");
        assert!(text.contains("Figure 1: Plain"), "article caption: {text}");
    }

    /// An environment is a group: a size or an alignment set inside a float
    /// must not leak into the text that follows it, and a cell that asks for
    /// ragged setting must keep its text.
    #[test]
    fn environments_scope_what_they_change() {
        let blocks = parse(concat!(
            "\\begin{table}[t]\n\\centering\n\\scriptsize\n",
            "\\begin{tabular}{ll}\n>{\\RaggedRight}A & \\RaggedRight B \\\\\n\\end{tabular}\n",
            "\\end{table}\nPlain body text.\n",
        ));
        let TexBlock::Table { rows, .. } = &blocks[0] else {
            panic!("expected a table, got {blocks:?}")
        };
        assert_eq!(text_of(&rows[0][1]).trim(), "B", "a ragged cell keeps its text");
        let body = blocks
            .iter()
            .find_map(|b| match b {
                TexBlock::Paragraph { spans, align, .. } => Some((text_of(spans), *align)),
                _ => None,
            })
            .expect("body paragraph");
        assert_eq!(body.0.trim(), "Plain body text.");
        assert_eq!(body.1, TexAlign::Justify, "the float's \\centering stopped at \\end");
        let sizes: Vec<f32> = blocks
            .iter()
            .filter_map(|b| match b {
                TexBlock::Paragraph { spans, .. } => spans.first().map(|s| s.size),
                _ => None,
            })
            .collect();
        assert!(sizes.iter().all(|s| (*s - 1.0).abs() < 0.01), "\\scriptsize leaked: {sizes:?}");
    }

    /// The preamble decides the page's paragraph shape and how deep the
    /// section numbering goes.
    #[test]
    fn preamble_sets_paragraph_shape_and_numbering() {
        let source = concat!(
            "\\documentclass[11pt]{article}\n",
            "\\usepackage[letterpaper,margin=1in]{geometry}\n",
            "\\setcounter{secnumdepth}{0}\n\\setlength{\\parindent}{0pt}\n",
            "\\setlength{\\parskip}{0.5em}\n\\graphicspath{{figures/}{plots/}}\n",
            "\\begin{document}\n\\begin{center}{\\LARGE A Title}\\end{center}\n",
            "\\section{1. Introduction}\nBody.\n\\end{document}\n",
        );
        let style = document_style(source);
        assert_eq!(style.margin_x, 72.0, "geometry margin wins: {style:?}");
        assert_eq!(style.secnumdepth, 0);
        assert_eq!(style.parindent, 0.0);
        assert!(style.parskip > 0.0);
        assert_eq!(graphics_paths(source), vec!["figures/".to_string(), "plots/".to_string()]);

        let blocks = parse_with(source, &style);
        let heading = blocks
            .iter()
            .find_map(|b| match b {
                TexBlock::Heading { spans, level: 1 } => Some(text_of(spans)),
                _ => None,
            })
            .expect("section heading");
        assert_eq!(heading, "1. Introduction", "secnumdepth 0 adds no number");
        let title = blocks
            .iter()
            .find_map(|b| match b {
                TexBlock::Paragraph { spans, align: TexAlign::Center, .. } => Some(spans.clone()),
                _ => None,
            })
            .expect("centered title");
        assert!(title[0].size > 1.5, "\\LARGE sets the title big: {:?}", title[0]);
    }

    /// \cite prints the number the reference list gives the key.
    #[test]
    fn citations_print_bibliography_numbers() {
        let blocks = parse(concat!(
            "See \\cite{b} and \\cite{a,b}.\\begin{thebibliography}{9}",
            "\\bibitem{a} First.\\bibitem{b} Second.\\end{thebibliography}",
        ));
        let TexBlock::Paragraph { spans, .. } = &blocks[0] else { panic!("expected paragraph") };
        assert_eq!(text_of(spans), "See [2] and [1, 2].");
    }
}

