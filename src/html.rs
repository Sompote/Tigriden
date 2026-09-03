//! A small HTML reader: enough of the parse to lay a page out.
//!
//! It is not the HTML5 tree algorithm. It recognizes tags and attributes,
//! decodes entities, drops comments and raw script/style bodies, and closes
//! the elements real documents leave open (`<p>`, `<li>`, `<td>`). What comes
//! out is a balanced event stream the viewer walks the way it walks Markdown.

/// One step of the document, in source order.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Open { name: String, attrs: Vec<(String, String)> },
    Close { name: String },
    /// Text with its entities already decoded. Whitespace is left as written;
    /// collapsing it is the layout's business, since `<pre>` keeps it.
    Text(String),
}

/// Looks an attribute up by its lowercased name.
pub fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// Elements that never have a closing tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
    "source", "track", "wbr",
];

/// Elements whose start ends an open paragraph.
const BLOCK: &[&str] = &[
    "address", "article", "aside", "blockquote", "details", "div", "dl", "dd", "dt",
    "fieldset", "figcaption", "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6",
    "header", "hr", "li", "main", "nav", "ol", "p", "pre", "section", "summary", "table",
    "tbody", "td", "tfoot", "th", "thead", "tr", "ul",
];

/// Elements whose content is raw text the reader throws away.
const RAW_SKIP: &[&str] = &["script", "style"];

pub fn parse(src: &str) -> Vec<Event> {
    let mut r = Reader { out: Vec::new(), open: Vec::new() };
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let mut text_from = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &src[i..];
        // Comments, doctypes and processing instructions carry no content.
        if let Some(skip_to) = markup_declaration(rest) {
            r.text(&src[text_from..i]);
            i += skip_to;
            text_from = i;
            continue;
        }
        let closing = rest.starts_with("</");
        let name_at = i + if closing { 2 } else { 1 };
        // A `<` that no name follows is literal text ("a < b").
        if !bytes.get(name_at).is_some_and(u8::is_ascii_alphabetic) {
            i += 1;
            continue;
        }
        let Some(end) = tag_end(bytes, name_at) else { break };
        r.text(&src[text_from..i]);
        let inner = &src[name_at..end];
        let split = inner
            .find(|c: char| !c.is_ascii_alphanumeric() && c != ':' && c != '-')
            .unwrap_or(inner.len());
        let name = inner[..split].to_ascii_lowercase();
        let tail = &inner[split..];
        i = end + 1;
        text_from = i;
        if closing {
            r.end(&name);
            continue;
        }
        let self_closing = tail.trim_end().ends_with('/');
        r.start(name.clone(), parse_attrs(tail), self_closing);
        if !self_closing && RAW_SKIP.contains(&name.as_str()) {
            // Everything up to the matching close tag is raw text, and `<` in
            // a script body is not markup, so scan for the tag rather than parse.
            i = find_close(src, &name, i).unwrap_or(src.len());
            text_from = i;
            r.end(&name);
        }
    }
    r.text(&src[text_from.min(src.len())..]);
    r.finish()
}

/// Length of a `<!-- -->`, `<!doctype …>` or `<?…>` run starting at `rest`,
/// or None when `rest` opens an ordinary tag.
fn markup_declaration(rest: &str) -> Option<usize> {
    if let Some(body) = rest.strip_prefix("<!--") {
        return Some(match body.find("-->") {
            Some(at) => 4 + at + 3,
            None => rest.len(),
        });
    }
    if rest.starts_with("<!") || rest.starts_with("<?") {
        return Some(match rest.find('>') {
            Some(at) => at + 1,
            None => rest.len(),
        });
    }
    None
}

/// Index of the `>` that ends a tag, skipping any inside quoted values.
fn tag_end(b: &[u8], mut i: usize) -> Option<usize> {
    let mut quote = 0u8;
    while i < b.len() {
        match b[i] {
            c if quote != 0 => {
                if c == quote {
                    quote = 0;
                }
            }
            c @ (b'"' | b'\'') => quote = c,
            b'>' => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Index just past the `</name…>` that ends a raw-text element, searched
/// from `from`. Case-insensitive, and it does not match `</scripts>`.
fn find_close(src: &str, name: &str, from: usize) -> Option<usize> {
    let (b, n) = (src.as_bytes(), name.as_bytes());
    let mut i = from;
    while i + 2 + n.len() <= b.len() {
        if b[i] == b'<' && b[i + 1] == b'/' && b[i + 2..i + 2 + n.len()].eq_ignore_ascii_case(n) {
            let after = b.get(i + 2 + n.len()).copied().unwrap_or(b'>');
            if after == b'>' || after.is_ascii_whitespace() {
                return Some(src[i..].find('>').map_or(src.len(), |e| i + e + 1));
            }
        }
        i += 1;
    }
    None
}

fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if b[i].is_ascii_whitespace() || b[i] == b'/' {
            i += 1;
            continue;
        }
        let name_at = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'/') {
            i += 1;
        }
        let name = s[name_at..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            match b.get(i) {
                Some(&q @ (b'"' | b'\'')) => {
                    i += 1;
                    let from = i;
                    while i < b.len() && b[i] != q {
                        i += 1;
                    }
                    value = decode_entities(&s[from..i]);
                    i += usize::from(i < b.len());
                }
                _ => {
                    let from = i;
                    while i < b.len() && !b[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    value = decode_entities(&s[from..i]);
                }
            }
        }
        if !name.is_empty() {
            out.push((name, value));
        }
    }
    out
}

/// Turns the tag stream into a balanced one, keeping the open-element stack
/// that decides where an unclosed element ends.
struct Reader {
    out: Vec<Event>,
    open: Vec<String>,
}

impl Reader {
    fn text(&mut self, raw: &str) {
        if !raw.is_empty() {
            self.out.push(Event::Text(decode_entities(raw)));
        }
    }

    fn start(&mut self, name: String, attrs: Vec<(String, String)>, self_closing: bool) {
        self.implicit_close(&name);
        let void = self_closing || VOID.contains(&name.as_str());
        self.out.push(Event::Open { name: name.clone(), attrs });
        if void {
            self.out.push(Event::Close { name });
        } else {
            self.open.push(name);
        }
    }

    fn end(&mut self, name: &str) {
        if VOID.contains(&name) {
            return;
        }
        // A close tag for something that was never opened is stray markup.
        let Some(at) = self.open.iter().rposition(|n| n == name) else { return };
        while self.open.len() > at {
            let name = self.open.pop().unwrap_or_default();
            self.out.push(Event::Close { name });
        }
    }

    /// Ends the elements a new start tag implicitly closes: a list item ends
    /// at the next item, a cell at the next cell, a paragraph at any block.
    fn implicit_close(&mut self, name: &str) {
        match name {
            "li" => self.close_nearest(&["li"], &["ul", "ol"]),
            "dt" | "dd" => self.close_nearest(&["dt", "dd"], &["dl"]),
            // Closing an element pops what it contains, so naming the outer
            // one here is enough: ending a row ends the cell still open in it.
            "tr" => self.close_nearest(&["tr"], &["table"]),
            "td" | "th" => self.close_nearest(&["td", "th"], &["tr", "table"]),
            "thead" | "tbody" | "tfoot" => {
                self.close_nearest(&["thead", "tbody", "tfoot"], &["table"])
            }
            "option" => self.close_nearest(&["option"], &["select"]),
            _ => {}
        }
        if BLOCK.contains(&name) && self.open.last().is_some_and(|n| n == "p") {
            self.end("p");
        }
    }

    /// Closes the nearest open element named in `names`, giving up at the
    /// first `stop` element so an item in an outer list is left alone.
    fn close_nearest(&mut self, names: &[&str], stop: &[&str]) {
        for n in self.open.iter().rev() {
            if names.contains(&n.as_str()) {
                let name = n.clone();
                self.end(&name);
                return;
            }
            if stop.contains(&n.as_str()) {
                return;
            }
        }
    }

    fn finish(mut self) -> Vec<Event> {
        while let Some(name) = self.open.pop() {
            self.out.push(Event::Close { name });
        }
        self.out
    }
}

/// Replaces `&…;` references with the characters they name. An unknown or
/// unterminated reference is kept as written, which is what a browser does.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        let end = tail.char_indices().take(32).find(|(_, c)| *c == ';').map(|(i, _)| i);
        match end.and_then(|e| entity(&tail[..e])) {
            Some(text) => {
                out.push_str(&text);
                rest = &tail[end.unwrap_or(0) + 1..];
            }
            None => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<String> {
    if let Some(num) = name.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse::<u32>().ok()?,
        };
        return char::from_u32(code).map(String::from);
    }
    let c = match name {
        // The five that markup itself needs, and the spaces, which the page
        // treats as spacing rather than as the characters they name.
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "ensp" | "emsp" | "thinsp" | "nnbsp" => ' ',
        "shy" => '\u{ad}',
        "zwj" | "zwnj" => return Some(String::new()),
        // The rest of the HTML 4 named set: punctuation, Greek, math signs,
        // arrows, and the accented Latin letters names are spelled with.
        "Aacute" => 'Á',
        "aacute" => 'á',
        "Acirc" => 'Â',
        "acirc" => 'â',
        "acute" => '´',
        "AElig" => 'Æ',
        "aelig" => 'æ',
        "Agrave" => 'À',
        "agrave" => 'à',
        "alefsym" => 'ℵ',
        "Alpha" => 'Α',
        "alpha" => 'α',
        "and" => '∧',
        "ang" => '∠',
        "Aring" => 'Å',
        "aring" => 'å',
        "asymp" => '≈',
        "Atilde" => 'Ã',
        "atilde" => 'ã',
        "Auml" => 'Ä',
        "auml" => 'ä',
        "bdquo" => '„',
        "Beta" => 'Β',
        "beta" => 'β',
        "brvbar" => '¦',
        "bull" => '•',
        "cap" => '∩',
        "Ccedil" => 'Ç',
        "ccedil" => 'ç',
        "cedil" => '¸',
        "cent" => '¢',
        "Chi" => 'Χ',
        "chi" => 'χ',
        "circ" => 'ˆ',
        "clubs" => '♣',
        "cong" => '≅',
        "copy" => '©',
        "crarr" => '↵',
        "cup" => '∪',
        "curren" => '¤',
        "Dagger" => '‡',
        "dagger" => '†',
        "dArr" => '⇓',
        "darr" => '↓',
        "deg" => '°',
        "Delta" => 'Δ',
        "delta" => 'δ',
        "diams" => '♦',
        "divide" => '÷',
        "Eacute" => 'É',
        "eacute" => 'é',
        "Ecirc" => 'Ê',
        "ecirc" => 'ê',
        "Egrave" => 'È',
        "egrave" => 'è',
        "empty" => '∅',
        "Epsilon" => 'Ε',
        "epsilon" => 'ε',
        "equiv" => '≡',
        "Eta" => 'Η',
        "eta" => 'η',
        "ETH" => 'Ð',
        "eth" => 'ð',
        "Euml" => 'Ë',
        "euml" => 'ë',
        "euro" => '€',
        "exist" => '∃',
        "fnof" => 'ƒ',
        "forall" => '∀',
        "frac12" => '½',
        "frac14" => '¼',
        "frac34" => '¾',
        "frasl" => '⁄',
        "Gamma" => 'Γ',
        "gamma" => 'γ',
        "ge" => '≥',
        "hArr" => '⇔',
        "harr" => '↔',
        "hearts" => '♥',
        "hellip" => '…',
        "Iacute" => 'Í',
        "iacute" => 'í',
        "Icirc" => 'Î',
        "icirc" => 'î',
        "iexcl" => '¡',
        "Igrave" => 'Ì',
        "igrave" => 'ì',
        "image" => 'ℑ',
        "infin" => '∞',
        "int" => '∫',
        "Iota" => 'Ι',
        "iota" => 'ι',
        "iquest" => '¿',
        "isin" => '∈',
        "Iuml" => 'Ï',
        "iuml" => 'ï',
        "Kappa" => 'Κ',
        "kappa" => 'κ',
        "Lambda" => 'Λ',
        "lambda" => 'λ',
        "lang" => '〈',
        "laquo" => '«',
        "lArr" => '⇐',
        "larr" => '←',
        "lceil" => '⌈',
        "ldquo" => '“',
        "le" => '≤',
        "lfloor" => '⌊',
        "lowast" => '∗',
        "loz" => '◊',
        "lrm" => '‎',
        "lsaquo" => '‹',
        "lsquo" => '‘',
        "macr" => '¯',
        "mdash" => '—',
        "micro" => 'µ',
        "middot" => '·',
        "minus" => '−',
        "Mu" => 'Μ',
        "mu" => 'μ',
        "nabla" => '∇',
        "ndash" => '–',
        "ne" => '≠',
        "ni" => '∋',
        "not" => '¬',
        "notin" => '∉',
        "nsub" => '⊄',
        "Ntilde" => 'Ñ',
        "ntilde" => 'ñ',
        "Nu" => 'Ν',
        "nu" => 'ν',
        "Oacute" => 'Ó',
        "oacute" => 'ó',
        "Ocirc" => 'Ô',
        "ocirc" => 'ô',
        "OElig" => 'Œ',
        "oelig" => 'œ',
        "Ograve" => 'Ò',
        "ograve" => 'ò',
        "oline" => '‾',
        "Omega" => 'Ω',
        "omega" => 'ω',
        "Omicron" => 'Ο',
        "omicron" => 'ο',
        "oplus" => '⊕',
        "or" => '∨',
        "ordf" => 'ª',
        "ordm" => 'º',
        "Oslash" => 'Ø',
        "oslash" => 'ø',
        "Otilde" => 'Õ',
        "otilde" => 'õ',
        "otimes" => '⊗',
        "Ouml" => 'Ö',
        "ouml" => 'ö',
        "para" => '¶',
        "part" => '∂',
        "permil" => '‰',
        "perp" => '⊥',
        "Phi" => 'Φ',
        "phi" => 'φ',
        "Pi" => 'Π',
        "pi" => 'π',
        "piv" => 'ϖ',
        "plusmn" => '±',
        "pound" => '£',
        "Prime" => '″',
        "prime" => '′',
        "prod" => '∏',
        "prop" => '∝',
        "Psi" => 'Ψ',
        "psi" => 'ψ',
        "radic" => '√',
        "rang" => '〉',
        "raquo" => '»',
        "rArr" => '⇒',
        "rarr" => '→',
        "rceil" => '⌉',
        "rdquo" => '”',
        "real" => 'ℜ',
        "reg" => '®',
        "rfloor" => '⌋',
        "Rho" => 'Ρ',
        "rho" => 'ρ',
        "rlm" => '‏',
        "rsaquo" => '›',
        "rsquo" => '’',
        "sbquo" => '‚',
        "Scaron" => 'Š',
        "scaron" => 'š',
        "sdot" => '⋅',
        "sect" => '§',
        "Sigma" => 'Σ',
        "sigma" => 'σ',
        "sigmaf" => 'ς',
        "sim" => '∼',
        "spades" => '♠',
        "sub" => '⊂',
        "sube" => '⊆',
        "sum" => '∑',
        "sup" => '⊃',
        "sup1" => '¹',
        "sup2" => '²',
        "sup3" => '³',
        "supe" => '⊇',
        "szlig" => 'ß',
        "Tau" => 'Τ',
        "tau" => 'τ',
        "there4" => '∴',
        "Theta" => 'Θ',
        "theta" => 'θ',
        "thetasym" => 'ϑ',
        "THORN" => 'Þ',
        "thorn" => 'þ',
        "tilde" => '˜',
        "times" => '×',
        "trade" => '™',
        "Uacute" => 'Ú',
        "uacute" => 'ú',
        "uArr" => '⇑',
        "uarr" => '↑',
        "Ucirc" => 'Û',
        "ucirc" => 'û',
        "Ugrave" => 'Ù',
        "ugrave" => 'ù',
        "uml" => '¨',
        "upsih" => 'ϒ',
        "Upsilon" => 'Υ',
        "upsilon" => 'υ',
        "Uuml" => 'Ü',
        "uuml" => 'ü',
        "weierp" => '℘',
        "Xi" => 'Ξ',
        "xi" => 'ξ',
        "Yacute" => 'Ý',
        "yacute" => 'ý',
        "yen" => '¥',
        "Yuml" => 'Ÿ',
        "yuml" => 'ÿ',
        "Zeta" => 'Ζ',
        "zeta" => 'ζ',
        _ => return None,
    };
    Some(String::from(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .map(|e| match e {
                Event::Open { name, .. } => format!("<{name}>"),
                Event::Close { name } => format!("</{name}>"),
                Event::Text(t) => format!("{t:?}"),
            })
            .collect()
    }

    #[test]
    fn an_unclosed_paragraph_ends_at_the_next_block() {
        let events = parse("<p>one<p>two<div>three</div>");
        assert_eq!(
            names(&events),
            ["<p>", "\"one\"", "</p>", "<p>", "\"two\"", "</p>", "<div>", "\"three\"", "</div>"]
        );
    }

    #[test]
    fn list_items_close_each_other_but_not_the_outer_list() {
        let events = parse("<ul><li>a<ul><li>b</ul><li>c</ul>");
        assert_eq!(
            names(&events),
            [
                "<ul>", "<li>", "\"a\"", "<ul>", "<li>", "\"b\"", "</li>", "</ul>", "</li>",
                "<li>", "\"c\"", "</li>", "</ul>"
            ]
        );
    }

    #[test]
    fn cells_and_rows_close_each_other() {
        let events = parse("<table><tr><td>a<td>b<tr><td>c</table>");
        let opened: Vec<_> = names(&events).into_iter().filter(|n| n.starts_with("</")).collect();
        assert_eq!(opened, ["</td>", "</td>", "</tr>", "</td>", "</tr>", "</table>"]);
    }

    #[test]
    fn script_and_style_bodies_never_reach_the_text() {
        let events = parse("<style>p > a {}</style><script>if (a<b) x()</script><p>kept");
        assert!(matches!(events.last(), Some(Event::Close { name }) if name == "p"));
        assert!(!names(&events).iter().any(|n| n.contains("x()") || n.contains("{}")));
    }

    #[test]
    fn comments_and_doctypes_are_dropped() {
        let events = parse("<!doctype html><!-- <p>hidden</p> --><p>shown");
        assert_eq!(names(&events), ["<p>", "\"shown\"", "</p>"]);
    }

    #[test]
    fn attributes_survive_quotes_and_angle_brackets() {
        let events = parse(r#"<img src="a b>c.png" alt='x' width=50%>"#);
        let Event::Open { attrs, .. } = &events[0] else { panic!("an open tag") };
        assert_eq!(attr(attrs, "src"), Some("a b>c.png"));
        assert_eq!(attr(attrs, "alt"), Some("x"));
        assert_eq!(attr(attrs, "width"), Some("50%"));
        assert_eq!(events.len(), 2, "a void element closes itself");
    }

    #[test]
    fn entities_decode_and_unknown_ones_stay() {
        assert_eq!(decode_entities("a &amp; b &#x3c; c &#8212; d"), "a & b < c — d");
        assert_eq!(decode_entities("&nope; &"), "&nope; &");
    }

    #[test]
    fn a_bare_angle_bracket_is_text() {
        assert_eq!(names(&parse("a < b")), ["\"a < b\""]);
    }
}
