//! Box layout for display math.
//!
//! Turns a [`MathNode`] tree into positioned text runs and rules, following
//! the vertical arrangement TeX uses: numerators stacked over a fraction bar,
//! scripts raised and lowered against the base, radicals with an overline,
//! and big operators carrying their limits above and below. The viewer paints
//! the result; nothing here touches the screen.

use std::sync::OnceLock;

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, Wrap};

use crate::tex::MathNode;

/// A positioned piece of a laid-out formula. Coordinates are relative to the
/// formula's baseline at the origin: negative `y` is above it.
pub enum MathItem {
    Run { buffer: Buffer, x: f32, y: f32 },
    /// Fraction bars and radical overlines.
    Rule { x: f32, y: f32, w: f32, h: f32 },
}

/// A measured formula.
pub struct MathBox {
    pub items: Vec<MathItem>,
    pub width: f32,
    /// Height above the baseline.
    pub ascent: f32,
    /// Depth below the baseline.
    pub descent: f32,
}

impl MathBox {
    fn empty() -> Self {
        MathBox { items: Vec::new(), width: 0.0, ascent: 0.0, descent: 0.0 }
    }

    pub fn height(&self) -> f32 {
        self.ascent + self.descent
    }

    /// Moves every item by (dx, dy).
    fn shift(&mut self, dx: f32, dy: f32) {
        for item in &mut self.items {
            match item {
                MathItem::Run { x, y, .. } => {
                    *x += dx;
                    *y += dy;
                }
                MathItem::Rule { x, y, .. } => {
                    *x += dx;
                    *y += dy;
                }
            }
        }
    }

    /// Absorbs `other`, placed at x offset `dx` with its baseline `dy` below
    /// ours, growing our extents to cover it.
    fn absorb(&mut self, mut other: MathBox, dx: f32, dy: f32) {
        other.shift(dx, dy);
        self.ascent = self.ascent.max(other.ascent - dy);
        self.descent = self.descent.max(other.descent + dy);
        self.items.append(&mut other.items);
    }

    /// Re-anchors items to the box's top-left corner, for painting.
    pub fn into_top_left(mut self) -> MathBox {
        let ascent = self.ascent;
        self.shift(0.0, ascent);
        self
    }
}

/// The serif face a typeset paper uses. cosmic-text's generic `Family::Serif`
/// resolves to the sans-serif default on macOS, so name a real face and fall
/// back only if none of them is installed.
pub fn serif_family(fs: &FontSystem) -> Family<'static> {
    static PICK: OnceLock<Option<&'static str>> = OnceLock::new();
    let name = PICK.get_or_init(|| {
        const CANDIDATES: [&str; 6] =
            ["Times New Roman", "Times", "New York", "Charter", "Georgia", "DejaVu Serif"];
        let db = fs.db();
        CANDIDATES
            .into_iter()
            .find(|c| db.faces().any(|f| f.families.iter().any(|(n, _)| n == c)))
    });
    name.map_or(Family::Serif, Family::Name)
}

/// Relations get the widest gap, binary operators a smaller one — the
/// spacing that makes `a + b = c` read as math rather than as a word.
fn atom_space(node: &MathNode) -> f32 {
    let MathNode::Sym { text, .. } = node else { return 0.0 };
    const RELATIONS: [&str; 22] = [
        "=", "<", ">", "≤", "≥", "≠", "≈", "≡", "∼", "≃", "∝", "→", "←", "↔", "⇒", "⇐", "⇔",
        "↦", "∈", "∉", "⊆", ":",
    ];
    const BINARY: [&str; 14] =
        ["+", "-", "−", "±", "∓", "×", "÷", "·", "∪", "∩", "∧", "∨", "⊕", "⊗"];
    if RELATIONS.contains(&text.as_str()) {
        0.28
    } else if BINARY.contains(&text.as_str()) {
        0.20
    } else {
        0.0
    }
}

/// Shapes one run of symbols and measures it from the font's own metrics.
fn run(fs: &mut FontSystem, text: &str, px: f32, italic: bool, color: Color) -> MathBox {
    if text.is_empty() {
        return MathBox::empty();
    }
    let px = px.max(4.0);
    let mut buffer = Buffer::new(fs, Metrics::new(px, px * 1.2));
    buffer.set_wrap(Wrap::None);
    let mut attrs = Attrs::new().family(serif_family(fs)).color(color);
    if italic {
        attrs = attrs.style(Style::Italic);
    }
    buffer.set_text(text, &attrs, Shaping::Advanced, None);
    buffer.set_size(Some(f32::MAX), None);
    buffer.shape_until_scroll(fs, false);
    let (mut width, mut ascent, mut descent) = (0.0f32, px * 0.75, px * 0.25);
    if let Some(r) = buffer.layout_runs().next() {
        width = r.line_w;
        ascent = r.line_y;
        descent = (r.line_height - r.line_y).max(0.0);
    }
    // `buffer.draw` anchors at the run's top-left, so lift it clear of the
    // baseline by its own ascent.
    MathBox {
        items: vec![MathItem::Run { buffer, x: 0.0, y: -ascent }],
        width,
        ascent,
        descent,
    }
}

/// The thickness TeX gives fraction bars and radical rules.
fn rule_thickness(px: f32) -> f32 {
    (px / 18.0).max(1.0)
}

/// Height of the math axis — the line fraction bars and delimiters center on.
fn axis(px: f32) -> f32 {
    px * 0.26
}

/// Lays out a formula, with items positioned relative to the baseline.
pub fn layout(fs: &mut FontSystem, node: &MathNode, px: f32, color: Color) -> MathBox {
    match node {
        MathNode::Sym { text, italic } => run(fs, text, px, *italic, color),
        MathNode::Space { em, literal } => {
            // TeX ignores whitespace typed in the source; only explicit
            // spacing macros survive.
            let w = if *literal { 0.0 } else { em * px };
            MathBox { items: Vec::new(), width: w, ascent: 0.0, descent: 0.0 }
        }
        MathNode::Row(children) => {
            let mut out = MathBox::empty();
            let mut x = 0.0f32;
            let mut pending = 0.0f32;
            for child in children {
                let gap = pending.max(atom_space(child));
                let b = layout(fs, child, px, color);
                if b.width == 0.0 && b.items.is_empty() {
                    pending = pending.max(atom_space(child));
                    x += b.width;
                    continue;
                }
                x += gap * px;
                let w = b.width;
                out.absorb(b, x, 0.0);
                x += w;
                pending = atom_space(child);
            }
            out.width = x;
            out
        }
        MathNode::Frac { num, den } => {
            let sub_px = px * 0.95;
            let num_b = layout(fs, num, sub_px, color);
            let den_b = layout(fs, den, sub_px, color);
            let thick = rule_thickness(px);
            let ax = axis(px);
            let gap = px * 0.20;
            let width = num_b.width.max(den_b.width) + px * 0.36;
            let num_dy = -(ax + thick / 2.0 + gap + num_b.descent);
            let den_dy = gap + thick / 2.0 - ax + den_b.ascent;
            let (num_w, den_w) = (num_b.width, den_b.width);
            let mut out = MathBox::empty();
            out.absorb(num_b, (width - num_w) / 2.0, num_dy);
            out.absorb(den_b, (width - den_w) / 2.0, den_dy);
            out.items.push(MathItem::Rule {
                x: 0.0,
                y: -ax - thick / 2.0,
                w: width,
                h: thick,
            });
            out.width = width;
            out.ascent = out.ascent.max(ax + thick / 2.0);
            out.descent = out.descent.max(thick / 2.0 - ax);
            out
        }
        MathNode::Script { base, sup, sub } => {
            let base_b = layout(fs, base, px, color);
            let s_px = (px * 0.72).max(6.0);
            let kern = px * 0.04;
            let base_w = base_b.width;
            let (base_asc, base_desc) = (base_b.ascent, base_b.descent);
            let mut out = MathBox::empty();
            out.absorb(base_b, 0.0, 0.0);
            let mut widest = 0.0f32;
            if let Some(sup) = sup {
                let b = layout(fs, sup, s_px, color);
                // Sit clear of the base's own height, never below half an em.
                let dy = -(base_asc - s_px * 0.35).max(px * 0.42) - b.descent;
                widest = widest.max(b.width);
                out.absorb(b, base_w + kern, dy);
            }
            if let Some(sub) = sub {
                let b = layout(fs, sub, s_px, color);
                // TeX drops a subscript's baseline about a quarter em; only a
                // base with real depth pushes it further.
                let dy = (px * 0.25).max(base_desc * 0.6 + s_px * 0.10);
                widest = widest.max(b.width);
                out.absorb(b, base_w + kern, dy);
            }
            out.width = base_w + kern + widest;
            out
        }
        MathNode::Sqrt { index, arg } => {
            let arg_b = layout(fs, arg, px, color);
            let pad = px * 0.14;
            let thick = rule_thickness(px);
            let inner_h = (arg_b.ascent + arg_b.descent + pad).max(px);
            // Scale the radical glyph so it spans the whole argument.
            let rad_px = (inner_h * 1.15).max(px);
            let rad_b = run(fs, "√", rad_px, false, color);
            let rad_w = rad_b.width;
            let (arg_w, arg_asc) = (arg_b.width, arg_b.ascent);
            let bar_y = -(arg_asc + pad);
            // Line the radical's top up with the overline it feeds into.
            let rad_dy = bar_y + rad_b.ascent;
            let mut out = MathBox::empty();
            out.absorb(rad_b, 0.0, rad_dy);
            out.absorb(arg_b, rad_w, 0.0);
            out.items.push(MathItem::Rule { x: rad_w, y: bar_y, w: arg_w, h: thick });
            out.ascent = out.ascent.max(-bar_y + thick);
            out.width = rad_w + arg_w;
            if let Some(index) = index {
                let b = layout(fs, index, (px * 0.55).max(5.0), color);
                let w = b.width;
                let dy = bar_y * 0.55;
                out.absorb(b, 0.0, dy);
                // Nudge everything right to make room for the index.
                let shift = (w - rad_w * 0.45).max(0.0);
                if shift > 0.0 {
                    out.shift(shift, 0.0);
                    out.width += shift;
                }
            }
            out
        }
        MathNode::BigOp { sym, under, over } => {
            let big_px = px * 1.5;
            let sym_b = run(fs, sym, big_px, false, color);
            let s_px = (px * 0.7).max(6.0);
            let gap = px * 0.16;
            let under_b = under.as_ref().map(|n| layout(fs, n, s_px, color));
            let over_b = over.as_ref().map(|n| layout(fs, n, s_px, color));
            let width = sym_b
                .width
                .max(under_b.as_ref().map_or(0.0, |b| b.width))
                .max(over_b.as_ref().map_or(0.0, |b| b.width));
            let (sym_w, sym_asc, sym_desc) = (sym_b.width, sym_b.ascent, sym_b.descent);
            let mut out = MathBox::empty();
            out.absorb(sym_b, (width - sym_w) / 2.0, 0.0);
            if let Some(b) = over_b {
                let dy = -(sym_asc + gap + b.descent);
                let w = b.width;
                out.absorb(b, (width - w) / 2.0, dy);
            }
            if let Some(b) = under_b {
                let dy = sym_desc + gap + b.ascent;
                let w = b.width;
                out.absorb(b, (width - w) / 2.0, dy);
            }
            out.width = width;
            out
        }
        MathNode::Fenced { open, close, body } => {
            let body_b = layout(fs, body, px, color);
            layout_fence(fs, body_b, open, close, px, color)
        }
        MathNode::Matrix { rows, open, close } => {
            let col_gap = px * 0.8;
            let row_gap = px * 0.45;
            let cells: Vec<Vec<MathBox>> = rows
                .iter()
                .map(|r| r.iter().map(|c| layout(fs, c, px, color)).collect())
                .collect();
            let ncols = cells.iter().map(Vec::len).max().unwrap_or(0);
            let mut col_w = vec![0.0f32; ncols];
            for row in &cells {
                for (i, c) in row.iter().enumerate() {
                    col_w[i] = col_w[i].max(c.width);
                }
            }
            let row_h: Vec<(f32, f32)> = cells
                .iter()
                .map(|r| {
                    (
                        r.iter().map(|c| c.ascent).fold(0.0, f32::max),
                        r.iter().map(|c| c.descent).fold(0.0, f32::max),
                    )
                })
                .collect();
            let total_h: f32 = row_h.iter().map(|(a, d)| a + d).sum::<f32>()
                + row_gap * (row_h.len().saturating_sub(1)) as f32;
            let body_w: f32 =
                col_w.iter().sum::<f32>() + col_gap * (ncols.saturating_sub(1)) as f32;
            // Center the block on the math axis.
            let top = -(total_h / 2.0) - axis(px);
            let mut body = MathBox::empty();
            let mut y = top;
            for (row, (asc, desc)) in cells.into_iter().zip(row_h.iter()) {
                y += asc;
                let mut x = 0.0f32;
                for (i, c) in row.into_iter().enumerate() {
                    let w = c.width;
                    body.absorb(c, x, y);
                    x += col_w.get(i).copied().unwrap_or(w) + col_gap;
                }
                y += desc + row_gap;
            }
            body.width = body_w;
            layout_fence(fs, body, open, close, px, color)
        }
        MathNode::Accent { base, mark } => {
            let base_b = layout(fs, base, px, color);
            let (base_w, base_asc) = (base_b.width, base_b.ascent);
            let mut out = MathBox::empty();
            out.absorb(base_b, 0.0, 0.0);
            let m = run(fs, &mark.to_string(), px, false, color);
            let w = m.width;
            // Combining marks carry no advance; center one over the base.
            out.absorb(m, (base_w - w).max(0.0) / 2.0, -(base_asc * 0.02));
            out.width = base_w;
            out
        }
    }
}

/// Wraps an already-laid-out body in delimiters stretched to its height.
fn layout_fence(
    fs: &mut FontSystem,
    body: MathBox,
    open: &str,
    close: &str,
    px: f32,
    color: Color,
) -> MathBox {
    if open.is_empty() && close.is_empty() {
        return body;
    }
    // Delimiters grow with the body and center on the math axis.
    let d_px = (body.height() * 1.06).max(px);
    let center = (body.descent - body.ascent) / 2.0;
    let mut out = MathBox::empty();
    let mut x = 0.0f32;
    if !open.is_empty() {
        let b = run(fs, open, d_px, false, color);
        let dy = center + (b.ascent - b.descent) / 2.0;
        let w = b.width;
        out.absorb(b, x, dy);
        x += w;
    }
    let body_w = body.width;
    out.absorb(body, x, 0.0);
    x += body_w;
    if !close.is_empty() {
        let b = run(fs, close, d_px, false, color);
        let dy = center + (b.ascent - b.descent) / 2.0;
        let w = b.width;
        out.absorb(b, x, dy);
        x += w;
    }
    out.width = x;
    out
}

/// Lays out an equation number like `(3)`, upright at body size.
pub fn layout_number(fs: &mut FontSystem, text: &str, px: f32, color: Color) -> MathBox {
    run(fs, text, px, false, color)
}
