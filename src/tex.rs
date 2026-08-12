//! Lightweight LaTeX-to-blocks converter for the read-only viewer.
//!
//! This is a best-effort formatter, not a TeX engine: it understands the
//! document constructs that show up in everyday papers and notes (sectioning,
//! text styles, lists, tabular, verbatim, math with a Unicode translation)
//! and degrades gracefully on everything else by showing the plain text.

use std::collections::HashMap;

/// A run of styled text within a paragraph, heading, list item or table cell.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// Monospace (\texttt, \verb).
    pub mono: bool,
    /// Accent-colored (\href, \url, \cite, \ref).
    pub link: bool,
}

#[derive(PartialEq, Debug)]
pub enum TexBlock {
    Paragraph(Vec<Span>),
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
    Table(Vec<Vec<Vec<Span>>>),
    /// \includegraphics: the path as written, plus the requested width as a
    /// fraction of the text column (None = the image's natural size).
    Image { path: String, width: Option<f32> },
    /// \caption text, shown italic under figures/tables.
    Caption(Vec<Span>),
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

#[derive(Clone, Copy, Default)]
struct StyleFlags {
    bold: bool,
    italic: bool,
    mono: bool,
    link: bool,
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
    /// One entry per open `{` group, saving the style to restore on `}`.
    groups: Vec<StyleFlags>,
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
    /// Labels found in this pass (filled on the scanning pass).
    labels: HashMap<String, RefTarget>,
    /// Labels from the scanning pass, used to resolve `\ref` in the real one.
    resolved: HashMap<String, RefTarget>,
    /// Set when everything pushed into the current paragraph so far is a
    /// single formula, so it can be promoted to a display equation.
    solo_math: Option<MathNode>,
    title: Option<String>,
    author: Option<String>,
    date: Option<String>,
}

/// Parses LaTeX source into a flat block list. Never fails: unknown input
/// passes through as text.
pub fn parse(source: &str) -> Vec<TexBlock> {
    let clean = strip_comments(source);
    let title = find_arg(&clean, "\\title");
    let author = find_arg(&clean, "\\author");
    let date = find_arg(&clean, "\\date");
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
    let mut scan = Parser::new(body, None, None, None, HashMap::new());
    scan.run();
    let resolved = std::mem::take(&mut scan.labels);

    let mut p = Parser::new(body, title, author, date, resolved);
    p.run();
    p.flush_paragraph();
    p.blocks
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
    let mut p = Parser::new(fragment, None, None, None, resolved.clone());
    p.run();
    p.spans
}

const CODE_ENVS: [&str; 5] = ["verbatim", "verbatim*", "lstlisting", "minted", "Verbatim"];
const MATH_ENVS: [&str; 10] = [
    "equation", "equation*", "align", "align*", "gather", "gather*", "displaymath", "eqnarray",
    "eqnarray*", "multline",
];
const TABLE_ENVS: [&str; 4] = ["tabular", "tabular*", "longtable", "array"];
const LIST_ENVS: [&str; 3] = ["itemize", "enumerate", "description"];
/// Environments whose begin/end are ignored and content flows through.
const SKIP_ENVS: [&str; 9] =
    ["document", "center", "figure", "figure*", "table", "table*", "flushleft", "flushright", "quote"];

impl Parser {
    fn new(
        body: &str,
        title: Option<String>,
        author: Option<String>,
        date: Option<String>,
        resolved: HashMap<String, RefTarget>,
    ) -> Self {
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
            labels: HashMap::new(),
            resolved,
            solo_math: None,
            title,
            author,
            date,
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
            if last.bold == s.bold && last.italic == s.italic && last.mono == s.mono && last.link == s.link
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
        if let Some(marker) = self.item.take() {
            self.blocks.push(TexBlock::ListItem {
                indent: self.lists.len().max(1),
                marker,
                spans,
            });
            // Later text in the same \item flows as further list lines.
            self.item = Some(String::new());
        } else {
            self.blocks.push(TexBlock::Paragraph(spans));
        }
    }

    /// Pushes inline math, italicizing only the variables.
    fn push_inline_math(&mut self, raw: &str) {
        // pandoc writes display formulas as `\(…\)` sitting alone in a
        // paragraph; note that so flush can promote them to real display math.
        let alone = self.spans.iter().all(|s| s.text.trim().is_empty());
        let node = parse_math(raw);
        let saved = self.style;
        for (text, italic) in math_spans(&node) {
            self.style.italic = italic;
            self.push_text(&text);
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
            self.groups.push(self.style);
            apply(&mut self.style);
        }
    }

    fn heading(&mut self, level: u8, starred: bool) {
        self.skip_opt();
        let Some(arg) = self.read_group() else { return };
        self.flush_paragraph();
        let mut spans = parse_inline_with(&arg, &self.resolved);
        if !starred && (1..=3).contains(&level) {
            let idx = (level - 1) as usize;
            self.counters[idx] += 1;
            for c in &mut self.counters[idx + 1..] {
                *c = 0;
            }
            let number = self.counters[..=idx]
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(".");
            self.last_numbered =
                Some(RefTarget { kind: RefKind::Section, number: number.clone() });
            spans.insert(0, Span { text: format!("{number}  "), ..Span::default() });
        }
        self.blocks.push(TexBlock::Heading { level, spans });
    }

    fn begin_env(&mut self, env: &str) {
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
            self.push_display_math(&raw, numbered);
        } else if TABLE_ENVS.contains(&env) {
            self.flush_paragraph();
            self.skip_opts(); // vertical placement, e.g. [t]
            if env == "tabular*" {
                let _ = self.read_group(); // width
            }
            let _ = self.read_group(); // column spec
            let raw = self.read_until_end(env);
            let table = parse_tabular(&raw, &self.resolved);
            if !table.is_empty() {
                self.blocks.push(TexBlock::Table(table));
            }
        } else if env == "abstract" {
            self.flush_paragraph();
            self.blocks.push(TexBlock::Heading {
                level: 2,
                spans: vec![Span { text: "Abstract".into(), bold: true, ..Span::default() }],
            });
        } else if env == "thebibliography" {
            let _ = self.read_group();
            self.flush_paragraph();
            self.blocks.push(TexBlock::Heading {
                level: 1,
                spans: vec![Span { text: "References".into(), bold: true, ..Span::default() }],
            });
            self.lists.push(ListCtx { counter: Some(1) });
        } else if SKIP_ENVS.contains(&env) {
            self.flush_paragraph();
            self.skip_opts(); // float placement, e.g. [htbp]
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
        if LIST_ENVS.contains(&env) || env == "thebibliography" {
            self.flush_paragraph();
            self.item = None;
            self.lists.pop();
        } else if env == "abstract" || SKIP_ENVS.contains(&env) {
            self.flush_paragraph();
            if matches!(env.trim_end_matches('*'), "figure" | "table") {
                self.floats.pop();
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
                self.style = StyleFlags { link: self.style.link, ..StyleFlags::default() };
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
                    let width = opts.as_deref().and_then(graphics_width);
                    self.blocks
                        .push(TexBlock::Image { path: path.trim().to_string(), width });
                }
            }
            "caption" => {
                self.skip_opts();
                if let Some(arg) = self.read_group() {
                    self.flush_paragraph();
                    let mut spans = parse_inline_with(&arg, &self.resolved);
                    if let Some(float) = self.floats.last() {
                        spans.insert(
                            0,
                            Span {
                                text: format!("{} {}: ", float.kind.name(), float.number),
                                bold: true,
                                ..Span::default()
                            },
                        );
                    }
                    self.blocks.push(TexBlock::Caption(spans));
                }
            }
            "maketitle" => {
                self.flush_paragraph();
                if let Some(t) = self.title.clone() {
                    self.blocks.push(TexBlock::Heading {
                        level: 0,
                        spans: vec![Span { text: t, bold: true, ..Span::default() }],
                    });
                }
                let byline: Vec<String> =
                    [self.author.clone(), self.date.clone()].into_iter().flatten().collect();
                if !byline.is_empty() {
                    self.blocks.push(TexBlock::Paragraph(vec![Span {
                        text: byline.join(" — "),
                        italic: true,
                        ..Span::default()
                    }]));
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
                    let saved = self.style;
                    self.style.link = true;
                    self.push_text(&format!("[{}]", arg.trim()));
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
            "newpage" | "clearpage" | "pagebreak" | "tableofcontents" | "listoffigures"
            | "listoftables" | "noindent" | "indent" | "centering" | "raggedright"
            | "raggedleft" | "smallskip" | "medskip" | "bigskip" | "hfill" | "vfill"
            | "frontmatter" | "mainmatter" | "backmatter" | "appendix" | "printbibliography"
            | "selectfont" | "flushbottom" | "onehalfspacing" | "doublespacing"
            | "singlespacing" | "footnotesize" | "small" | "normalsize" | "large" | "Large"
            | "LARGE" | "huge" | "Huge" | "tiny" | "scriptsize" => {}
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
                    self.groups.push(self.style);
                }
                '}' => {
                    commit!();
                    self.pos += 1;
                    if let Some(saved) = self.groups.pop() {
                        self.style = saved;
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
const SYMBOLS: [(&str, &str); 96] = [
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
    let items = p.parse_row(&[]);
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
        MathNode::Sym { text: c.to_string(), italic: c.is_ascii_alphabetic() }
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
            "mathbf" | "boldsymbol" | "bm" | "mathbb" | "mathcal" | "mathit" => self.parse_arg(),
            "hat" | "widehat" => self.accent('\u{0302}'),
            "bar" | "overline" => self.accent('\u{0304}'),
            "vec" => self.accent('\u{20D7}'),
            "tilde" | "widetilde" => self.accent('\u{0303}'),
            "dot" => self.accent('\u{0307}'),
            "ddot" => self.accent('\u{0308}'),
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
                    // An alignment tab outside a matrix: TeX sets a gap there.
                    self.pos += 1;
                    items.push(MathNode::Space { em: 0.5, literal: false });
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
pub fn math_spans(node: &MathNode) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    collect_spans(node, &mut out);
    // Merge neighbours that share a style, and drop doubled spaces.
    let mut merged: Vec<(String, bool)> = Vec::new();
    for (text, italic) in out {
        if text.is_empty() {
            continue;
        }
        match merged.last_mut() {
            Some((prev, prev_it)) if *prev_it == italic => prev.push_str(&text),
            _ => merged.push((text, italic)),
        }
    }
    merged
}

fn collect_spans(node: &MathNode, out: &mut Vec<(String, bool)>) {
    match node {
        MathNode::Sym { text, italic } => out.push((text.clone(), *italic)),
        MathNode::Row(items) => items.iter().for_each(|i| collect_spans(i, out)),
        MathNode::Space { .. } => out.push((flatten_math(node), false)),
        // Descend so a scripted variable keeps its slant: the `x` of `x^2`
        // is still a variable, even though the exponent is not.
        MathNode::Script { base, sup, sub } => {
            collect_spans(base, out);
            if let Some(sb) = sub {
                out.push((script_text(&flatten_math(sb), false), false));
            }
            if let Some(sp) = sup {
                out.push((script_text(&flatten_math(sp), true), false));
            }
        }
        MathNode::Accent { base, .. } => collect_spans(base, out),
        MathNode::Fenced { open, close, body } => {
            out.push((open.clone(), false));
            collect_spans(body, out);
            out.push((close.clone(), false));
        }
        // Everything else keeps the flattened form; its internals are
        // already positional rather than stylistic.
        other => out.push((flatten_math(other), false)),
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
            TexBlock::Paragraph(spans) => {
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
            TexBlock::Paragraph(spans) => assert_eq!(text_of(spans), "50% done next line"),
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn tabular_rows_and_cells() {
        let blocks = parse(
            "\\begin{tabular}{ll}\n\\hline\nA & B \\\\\n1 & 2 \\\\\n\\hline\n\\end{tabular}\n",
        );
        match &blocks[0] {
            TexBlock::Table(rows) => {
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
            TexBlock::Paragraph(spans) => {
                assert!(text_of(spans).contains("x²"));
                // The variable slants; the exponent is a number, so it does not.
                assert!(spans.iter().any(|s| s.italic && s.text.contains('x')));
                assert!(spans.iter().any(|s| !s.italic && s.text.contains('²')));
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
                TexBlock::Paragraph(s) | TexBlock::ListItem { spans: s, .. } => text_of(s),
                TexBlock::Table(rows) => {
                    rows.iter().flatten().map(|c| text_of(c)).collect::<String>()
                }
                _ => String::new(),
            })
            .collect();
        assert!(!all.contains("leftmargin"), "enumitem options leaked: {all}");
        assert!(!all.contains("htbp"), "float placement leaked: {all}");
        assert!(!all.contains("]sdfc"), "empty cite options leaked: {all}");
        assert!(all.contains("[sdfc,mff]"), "citation key missing: {all}");
        assert!(all.contains("one"), "list item text missing: {all}");
    }

    /// A bracketed group inside inline math must survive: `[0,1]^{...}` used
    /// to lose everything from the closing bracket onward.
    #[test]
    fn brackets_and_scripts_survive_inline_math() {
        let blocks = parse("Given $x \\in [0,1]^{B \\times C}$ done.\n");
        let TexBlock::Paragraph(spans) = &blocks[0] else { panic!("expected paragraph") };
        let text = text_of(spans);
        assert!(text.contains("[0,1]"), "bracket group lost: {text}");
        assert!(text.contains('B') && text.contains('C'), "script lost: {text}");
        assert!(text.contains("done."), "text after math lost: {text}");
    }

    /// Variables slant; numbers, operators and function names do not.
    #[test]
    fn inline_math_italicizes_only_variables() {
        let blocks = parse("Let $g = \\min(8, H)$.\n");
        let TexBlock::Paragraph(spans) = &blocks[0] else { panic!("expected paragraph") };
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
                TexBlock::Paragraph(s) => Some(text_of(s)),
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
                TexBlock::Caption(s) => Some(text_of(s)),
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
        let TexBlock::Paragraph(spans) = &blocks[0] else { panic!("expected paragraph") };
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
                TexBlock::Paragraph(s) => Some(text_of(s)),
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
        let TexBlock::Table(rows) = &blocks[0] else { panic!("expected table, got {blocks:?}") };
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
            matches!(last, TexBlock::Paragraph(s) if text_of(s).contains("stays inline")),
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
            TexBlock::Paragraph(spans) => assert_eq!(text_of(spans), "Ada"),
            other => panic!("expected byline, got {other:?}"),
        }
    }
}

