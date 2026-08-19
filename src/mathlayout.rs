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
    /// Height above the baseline, leading included: what the block reserves.
    pub ascent: f32,
    /// Depth below the baseline, leading included.
    pub descent: f32,
    /// How far the ink itself reaches above the baseline. TeX grows
    /// delimiters and places scripts against this, not against the line box.
    ink_asc: f32,
    /// How far the ink reaches below the baseline.
    ink_desc: f32,
}

impl MathBox {
    fn empty() -> Self {
        MathBox {
            items: Vec::new(),
            width: 0.0,
            ascent: 0.0,
            descent: 0.0,
            ink_asc: 0.0,
            ink_desc: 0.0,
        }
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
        self.ink_asc = self.ink_asc.max(other.ink_asc - dy);
        self.ink_desc = self.ink_desc.max(other.ink_desc + dy);
        self.items.append(&mut other.items);
    }

    /// Grows the ink extents to cover a rule or a glyph the caller placed by
    /// hand, so an enclosing delimiter still sizes itself around it.
    fn cover_ink(&mut self, top: f32, bottom: f32) {
        self.ink_asc = self.ink_asc.max(-top);
        self.ink_desc = self.ink_desc.max(bottom);
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

/// What kind of atom a node is, for spacing. TeX's table is finer than this,
/// but the distinctions that show are the ones between a relation, a binary
/// operator, a piece of punctuation and a named function.
enum Atom {
    Ordinary,
    /// `=`, `\in`, `\leq` — the widest gap.
    Relation,
    /// `+`, `\times`, `\cdot` — a smaller one.
    Binary,
    /// `,` and `;`, which take their space on the right only.
    Punctuation,
    /// `\sin`, `\LN`, `\Projection`: a thin space follows, which is exactly
    /// what the `\!` in `\LN\!\left(` is written to cancel.
    Operator,
}

fn atom(node: &MathNode) -> Atom {
    const RELATIONS: [&str; 22] = [
        "=", "<", ">", "≤", "≥", "≠", "≈", "≡", "∼", "≃", "∝", "→", "←", "↔", "⇒", "⇐", "⇔",
        "↦", "∈", "∉", "⊆", ":",
    ];
    const BINARY: [&str; 14] =
        ["+", "-", "−", "±", "∓", "×", "÷", "·", "∪", "∩", "∧", "∨", "⊕", "⊗"];
    match node {
        // `\MHA_{feat}(...)` spaces like the operator it is built on.
        MathNode::Script { base, .. } => atom(base),
        MathNode::Sym { text, italic } => {
            if RELATIONS.contains(&text.as_str()) {
                Atom::Relation
            } else if BINARY.contains(&text.as_str()) {
                Atom::Binary
            } else if text == "," || text == ";" {
                Atom::Punctuation
            } else if !*italic && text.chars().count() > 1 && text.chars().all(char::is_alphabetic)
            {
                Atom::Operator
            } else {
                Atom::Ordinary
            }
        }
        _ => Atom::Ordinary,
    }
}

/// The gap TeX leaves on each side of an atom, in ems.
fn atom_space(node: &MathNode) -> (f32, f32) {
    match atom(node) {
        Atom::Relation => (0.28, 0.28),
        Atom::Binary => (0.20, 0.20),
        Atom::Punctuation => (0.0, 0.17),
        Atom::Operator => (0.0, 0.17),
        Atom::Ordinary => (0.0, 0.0),
    }
}

/// How far a character's ink reaches above and below the baseline, in ems.
///
/// cosmic-text reports the face's ascender and descender, which are the same
/// for every string and which no glyph actually touches. TeX sizes delimiters
/// and places scripts against the ink of the characters themselves, so read
/// the extent off what the characters are: a fence, a cap, an x-height body,
/// a descender. Anything unrecognised is assumed to be cap-high, which is the
/// safe middle.
fn char_ink(c: char) -> (f32, f32) {
    match c {
        // Fences and slashes run from above the cap to below the baseline.
        '(' | ')' | '[' | ']' | '{' | '}' | '|' | '/' | '⟨' | '⟩' | '⌈' | '⌉' | '⌊' | '⌋'
        | '‖' => (0.75, 0.24),
        '√' => (0.80, 0.10),
        '∑' | '∏' | '∐' | '∫' | '∮' | '⋀' | '⋁' | '⋂' | '⋃' | '⨁' | '⨂' => (0.75, 0.25),
        // Lowercase that drops below the line. Italic `f` does too.
        'f' | 'g' | 'j' | 'p' | 'q' | 'y' | 'β' | 'γ' | 'η' | 'μ' | 'ρ' | 'ς' | 'φ' | 'χ'
        | 'ψ' | 'ζ' | 'ξ' | ',' | ';' => (0.70, 0.22),
        // Lowercase that stays inside the x-height.
        'a' | 'c' | 'e' | 'm' | 'n' | 'o' | 'r' | 's' | 'u' | 'v' | 'w' | 'x' | 'z' | 'α'
        | 'ε' | 'ι' | 'κ' | 'ν' | 'ο' | 'π' | 'σ' | 'τ' | 'υ' | 'ω' => (0.48, 0.0),
        // Operators and relations sit on the math axis.
        '+' | '-' | '−' | '=' | '×' | '÷' | '±' | '∓' | '<' | '>' | '≤' | '≥' | '≈' | '≡'
        | '≠' | '∈' | '∉' | '∼' | '→' | '←' | '↦' | '·' | '⋅' => (0.56, 0.06),
        '.' => (0.10, 0.0),
        _ => (0.70, 0.0),
    }
}

/// The ink extent of a whole run: the tallest rise and the deepest drop.
fn ink_extent(text: &str, px: f32) -> (f32, f32) {
    let (mut asc, mut desc) = (0.0f32, 0.0f32);
    for c in text.chars() {
        let (a, d) = char_ink(c);
        asc = asc.max(a);
        desc = desc.max(d);
    }
    (asc * px, desc * px)
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
    let (ink_asc, ink_desc) = ink_extent(text, px);
    // `buffer.draw` anchors at the run's top-left, so lift it clear of the
    // baseline by its own ascent.
    MathBox {
        items: vec![MathItem::Run { buffer, x: 0.0, y: -ascent }],
        width,
        ascent,
        descent,
        ink_asc,
        ink_desc,
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

/// TeX sets scripts one size down, and scripts of scripts one size down
/// again — but no further, so a doubly-nested exponent stays legible.
fn script_size(px: f32, level: u8) -> (f32, u8) {
    if level >= 2 {
        (px, level)
    } else {
        ((px * 0.72).max(6.0), level + 1)
    }
}

/// Lays out a formula, with items positioned relative to the baseline.
pub fn layout(fs: &mut FontSystem, node: &MathNode, px: f32, color: Color) -> MathBox {
    lay(fs, node, px, color, 0)
}

/// `level` is TeX's script style: 0 display or text, 1 script, 2 and beyond
/// scriptscript. It decides how far a nested script shrinks.
fn lay(fs: &mut FontSystem, node: &MathNode, px: f32, color: Color, level: u8) -> MathBox {
    match node {
        MathNode::Sym { text, italic } => run(fs, text, px, *italic, color),
        MathNode::Space { em, literal } => {
            // TeX ignores whitespace typed in the source; only explicit
            // spacing macros survive.
            let w = if *literal { 0.0 } else { em * px };
            MathBox { width: w, ..MathBox::empty() }
        }
        MathNode::Row(children) => {
            let mut out = MathBox::empty();
            let mut x = 0.0f32;
            let mut pending = 0.0f32;
            for child in children {
                let (before, after) = atom_space(child);
                let b = lay(fs, child, px, color, level);
                if b.width == 0.0 && b.items.is_empty() {
                    // A typed space contributes nothing of its own but must
                    // not swallow the gap the atoms around it have earned.
                    pending = pending.max(after);
                    continue;
                }
                x += pending.max(before) * px;
                let w = b.width;
                out.absorb(b, x, 0.0);
                x += w;
                pending = after;
            }
            out.width = x;
            out
        }
        MathNode::Frac { num, den } => {
            let sub_px = px * 0.95;
            let num_b = lay(fs, num, sub_px, color, level);
            let den_b = lay(fs, den, sub_px, color, level);
            let thick = rule_thickness(px);
            let ax = axis(px);
            let gap = px * 0.20;
            let width = num_b.width.max(den_b.width) + px * 0.36;
            // Stack the two clear of the bar by their own ink, so a numerator
            // of bare x-height letters is not left floating above it.
            let num_dy = -(ax + thick / 2.0 + gap + num_b.ink_desc);
            let den_dy = gap + thick / 2.0 - ax + den_b.ink_asc;
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
            out.cover_ink(-ax - thick / 2.0, thick / 2.0 - ax);
            out
        }
        MathNode::Script { base, sup, sub } => {
            let base_b = lay(fs, base, px, color, level);
            let (s_px, s_level) = script_size(px, level);
            let kern = px * 0.04;
            let base_w = base_b.width;
            let (base_asc, base_desc) = (base_b.ink_asc, base_b.ink_desc);
            let mut out = MathBox::empty();
            out.absorb(base_b, 0.0, 0.0);
            let sup_b = sup.as_ref().map(|n| lay(fs, n, s_px, color, s_level));
            let sub_b = sub.as_ref().map(|n| lay(fs, n, s_px, color, s_level));
            // TeX raises a superscript's baseline by a fixed amount, and by
            // more only when the base is tall enough to reach into it; the
            // subscript drops the same way.
            let mut up = (base_asc - s_px * 0.30).max(px * 0.45);
            let mut down = (base_desc + s_px * 0.20).max(px * 0.20);
            // With both, keep a gap between the exponent's tail and the
            // index's head rather than letting the two touch.
            if let (Some(a), Some(b)) = (&sup_b, &sub_b) {
                let clear = a.ink_desc + b.ink_asc + px * 0.12;
                let short = clear - (up + down);
                if short > 0.0 {
                    up += short * 0.5;
                    down += short * 0.5;
                }
            }
            let mut widest = 0.0f32;
            if let Some(b) = sup_b {
                widest = widest.max(b.width);
                out.absorb(b, base_w + kern, -up);
            }
            if let Some(b) = sub_b {
                widest = widest.max(b.width);
                out.absorb(b, base_w + kern, down);
            }
            out.width = base_w + kern + widest;
            out
        }
        MathNode::Sqrt { index, arg } => {
            let arg_b = lay(fs, arg, px, color, level);
            let pad = px * 0.16;
            let thick = rule_thickness(px);
            // The radical is scaled to span the argument's ink, not its line
            // box: `√(x)` keeps a normal-sized sign, `√` over a fraction grows.
            let span = (arg_b.ink_asc + arg_b.ink_desc + pad).max(px * 0.72);
            let (rad_asc, rad_desc) = char_ink('√');
            let rad_px = (span / (rad_asc + rad_desc)).max(px);
            let rad_b = run(fs, "√", rad_px, false, color);
            let rad_w = rad_b.width;
            let (arg_w, arg_ink) = (arg_b.width, arg_b.ink_asc);
            let bar_y = -(arg_ink + pad);
            // Line the radical's own tip up with the overline it feeds into.
            let rad_dy = bar_y + rad_asc * rad_px;
            let mut out = MathBox::empty();
            out.absorb(rad_b, 0.0, rad_dy);
            out.absorb(arg_b, rad_w, 0.0);
            out.items.push(MathItem::Rule { x: rad_w, y: bar_y, w: arg_w, h: thick });
            out.ascent = out.ascent.max(-bar_y + thick);
            out.cover_ink(bar_y, 0.0);
            out.width = rad_w + arg_w;
            if let Some(index) = index {
                let b = lay(fs, index, (px * 0.55).max(5.0), color, 2);
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
            let (s_px, s_level) = script_size(px, level);
            let gap = px * 0.16;
            let under_b = under.as_ref().map(|n| lay(fs, n, s_px, color, s_level));
            let over_b = over.as_ref().map(|n| lay(fs, n, s_px, color, s_level));
            let width = sym_b
                .width
                .max(under_b.as_ref().map_or(0.0, |b| b.width))
                .max(over_b.as_ref().map_or(0.0, |b| b.width));
            let (sym_w, sym_asc, sym_desc) = (sym_b.width, sym_b.ink_asc, sym_b.ink_desc);
            let mut out = MathBox::empty();
            out.absorb(sym_b, (width - sym_w) / 2.0, 0.0);
            if let Some(b) = over_b {
                let dy = -(sym_asc + gap + b.ink_desc);
                let w = b.width;
                out.absorb(b, (width - w) / 2.0, dy);
            }
            if let Some(b) = under_b {
                let dy = sym_desc + gap + b.ink_asc;
                let w = b.width;
                out.absorb(b, (width - w) / 2.0, dy);
            }
            out.width = width;
            out
        }
        MathNode::Fenced { open, close, body } => {
            let body_b = lay(fs, body, px, color, level);
            layout_fence(fs, body_b, open, close, px, color)
        }
        MathNode::Matrix { rows, open, close } => {
            let col_gap = px * 0.8;
            let row_gap = px * 0.45;
            let cells: Vec<Vec<MathBox>> = rows
                .iter()
                .map(|r| r.iter().map(|c| lay(fs, c, px, color, level)).collect())
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
            let base_b = lay(fs, base, px, color, level);
            let (base_w, base_ink) = (base_b.width, base_b.ink_asc);
            let mut out = MathBox::empty();
            out.absorb(base_b, 0.0, 0.0);
            // An arrow is a full-size glyph where the others are small marks.
            let m_px = if *mark == '→' { px * 0.62 } else { px };
            let m = run(fs, &mark.to_string(), m_px, false, color);
            let w = m.width;
            // Each mark is drawn at the height that clears an x-height letter;
            // lift it by whatever more the base needs, and center it, letting a
            // mark wider than its base hang over both sides.
            let gap = px * 0.04;
            let (m_asc, m_desc) = (m.ink_asc, m.ink_desc);
            let dy = (accent_foot(*mark) * m_px - base_ink - gap).min(0.0);
            out.absorb(m, (base_w - w) / 2.0, dy);
            out.cover_ink(dy - m_asc, dy + m_desc);
            out.width = base_w;
            out
        }
    }
}

/// How far above its own baseline a spacing accent's underside sits, in ems.
/// A circumflex hangs lower than a macron, and an arrow — which `\vec` sets —
/// rests on the baseline like any other character.
fn accent_foot(mark: char) -> f32 {
    match mark {
        '\u{00AF}' => 0.62,               // macron, for \bar
        '\u{02D9}' | '\u{00A8}' => 0.58, // dot, diaeresis
        '\u{02DC}' => 0.54,               // small tilde
        '→' => -0.06,
        _ => 0.50, // modifier circumflex, for \hat
    }
}

/// Wraps an already-laid-out body in delimiters sized to reach around it.
///
/// This is TeX's `\left ... \right` rule. The delimiter has to cover the
/// body's reach on the far side of the math axis, doubled — but only to
/// within `DELIMITER_FACTOR`, and that slack is what stops the size cascading:
/// a normal parenthesis is itself tall enough to satisfy the run it encloses,
/// so `LN(ReLU(W f + b))` keeps normal parentheses at every level instead of
/// growing one step per nesting.
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
    /// TeX's \delimiterfactor: a delimiter may fall this far short of the
    /// span it nominally has to cover.
    const DELIMITER_FACTOR: f32 = 0.901;
    let ax = axis(px);
    let (fence_asc, fence_desc) = char_ink('(');
    let reach = (body.ink_asc - ax).max(body.ink_desc + ax);
    let need = 2.0 * reach * DELIMITER_FACTOR;
    let scale = need / ((fence_asc + fence_desc) * px);
    // Sizes step, the way \big \Big \bigg do, so near-identical delimiters
    // in neighbouring subformulas do not come out imperceptibly different.
    let d_px = if scale <= 1.0 { px } else { px * (scale * 5.0).ceil() / 5.0 };
    // A delimiter is centered on the math axis, however far it has grown.
    let dy = (fence_asc - fence_desc) / 2.0 * d_px - ax;
    let mut out = MathBox::empty();
    let mut x = 0.0f32;
    if !open.is_empty() {
        let b = run(fs, open, d_px, false, color);
        let w = b.width;
        out.absorb(b, x, dy);
        x += w;
    }
    let body_w = body.width;
    out.absorb(body, x, 0.0);
    x += body_w;
    if !close.is_empty() {
        let b = run(fs, close, d_px, false, color);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tex::parse_math;

    fn box_of(src: &str) -> MathBox {
        let mut fs = FontSystem::new();
        layout(&mut fs, &parse_math(src), 20.0, Color::rgb(0, 0, 0))
    }

    /// Nesting `\left(...\right)` must not grow the delimiters one step per
    /// level. A normal parenthesis is already tall enough to enclose another,
    /// and TeX's delimiter factor is the slack that says so; without it
    /// `LN(ReLU(W f + b))` came out with four visibly different paren sizes.
    #[test]
    fn nested_delimiters_stay_the_same_size() {
        let one = box_of(r"\left( x \right)");
        let four = box_of(r"\left(\left(\left(\left( x \right)\right)\right)\right)");
        assert!(
            (one.height() - four.height()).abs() < 0.5,
            "delimiters grew with nesting: {} then {}",
            one.height(),
            four.height()
        );
    }

    /// A delimiter around something genuinely tall still grows.
    #[test]
    fn delimiters_grow_around_a_fraction() {
        let plain = box_of(r"\left( x \right)");
        let frac = box_of(r"\left( \frac{a+b}{c-d} \right)");
        assert!(
            frac.height() > plain.height() * 1.6,
            "fraction delimiters did not grow: {} then {}",
            plain.height(),
            frac.height()
        );
    }

    /// A superscript sits against the base's own ink. Measured from the line
    /// box instead, every exponent floated a third of an em too high.
    #[test]
    fn a_superscript_rides_on_the_base_not_above_it() {
        let bare = box_of("R");
        let scripted = box_of("R^{16}");
        let rise = scripted.ascent - bare.ascent;
        assert!(
            rise > 0.0 && rise < 20.0 * 0.35,
            "exponent is not sitting on the cap: rose {rise} on a 20px body"
        );
    }

    /// An operator name is followed by a thin space, which is exactly what the
    /// `\!` of `\LN\!\left(` is written to cancel: the parenthesis should
    /// end up where it would with neither.
    #[test]
    fn a_negative_thin_space_cancels_the_operator_gap() {
        let tucked = box_of(r"\LN\!\left( x \right)");
        let plain = box_of(r"\LN\left( x \right)");
        assert!(
            tucked.width < plain.width,
            "\\! did not tighten anything: {} then {}",
            plain.width,
            tucked.width
        );
        assert!(
            plain.width - tucked.width < 20.0 * 0.2,
            "\\! pulled the parenthesis into the operator: {} then {}",
            plain.width,
            tucked.width
        );
    }
}
