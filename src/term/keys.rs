use alacritty_terminal::term::TermMode;

// Slint special-key characters (i-slint-common key_codes.rs).
const K_BACKSPACE: char = '\u{0008}';
const K_TAB: char = '\u{0009}';
const K_RETURN: char = '\u{000a}';
const K_ESCAPE: char = '\u{001b}';
const K_DELETE: char = '\u{007f}';
const K_UP: char = '\u{F700}';
const K_DOWN: char = '\u{F701}';
const K_LEFT: char = '\u{F702}';
const K_RIGHT: char = '\u{F703}';
const K_HOME: char = '\u{F729}';
const K_END: char = '\u{F72B}';
const K_PAGE_UP: char = '\u{F72C}';
const K_PAGE_DOWN: char = '\u{F72D}';

pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
    pub shift: bool,
}

impl Mods {
    fn xterm_code(&self) -> u8 {
        1 + (self.shift as u8) + ((self.alt as u8) << 1) + ((self.ctrl as u8) << 2)
    }

    fn any_encoding_mod(&self) -> bool {
        self.shift || self.alt || self.ctrl
    }
}

/// Encodes a Slint key event into the byte sequence a PTY expects.
/// Returns None for keys the terminal should not consume (e.g. Cmd shortcuts,
/// which the app layer handles).
pub fn encode(text: &str, mods: &Mods, mode: TermMode) -> Option<Vec<u8>> {
    let mut chars = text.chars();
    let ch = chars.next()?;
    // Multi-char text (IME bursts) goes through as-is.
    if chars.next().is_some() {
        return Some(text.as_bytes().to_vec());
    }

    // Cmd combos are app shortcuts, never terminal input.
    if mods.meta {
        return None;
    }

    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let arrow = |letter: u8, mods: &Mods| -> Vec<u8> {
        if mods.any_encoding_mod() {
            format!("\x1b[1;{}{}", mods.xterm_code(), letter as char).into_bytes()
        } else if app_cursor {
            vec![0x1b, b'O', letter]
        } else {
            vec![0x1b, b'[', letter]
        }
    };

    let seq = match ch {
        K_UP => arrow(b'A', mods),
        K_DOWN => arrow(b'B', mods),
        K_RIGHT => arrow(b'C', mods),
        K_LEFT => arrow(b'D', mods),
        K_HOME => arrow(b'H', mods),
        K_END => arrow(b'F', mods),
        K_PAGE_UP => b"\x1b[5~".to_vec(),
        K_PAGE_DOWN => b"\x1b[6~".to_vec(),
        K_DELETE => b"\x1b[3~".to_vec(),
        K_RETURN => vec![b'\r'],
        K_TAB => {
            if mods.shift {
                b"\x1b[Z".to_vec()
            } else {
                vec![b'\t']
            }
        }
        K_BACKSPACE => {
            if mods.alt {
                vec![0x1b, 0x7f]
            } else {
                vec![0x7f]
            }
        }
        K_ESCAPE => vec![0x1b],
        c if mods.ctrl => {
            let byte = match c {
                'a'..='z' => Some(c as u8 - b'a' + 1),
                'A'..='Z' => Some(c.to_ascii_lowercase() as u8 - b'a' + 1),
                ' ' | '@' => Some(0x00),
                '[' => Some(0x1b),
                '\\' => Some(0x1c),
                ']' => Some(0x1d),
                '^' => Some(0x1e),
                '_' | '-' => Some(0x1f),
                _ => None,
            };
            match byte {
                Some(b) => vec![b],
                None => return None,
            }
        }
        c if mods.alt => {
            let mut bytes = vec![0x1b];
            let mut buf = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            bytes
        }
        c if !c.is_control() => {
            let mut buf = [0u8; 4];
            c.encode_utf8(&mut buf).as_bytes().to_vec()
        }
        _ => return None,
    };
    Some(seq)
}

/// Prepares pasted text: normalizes line endings and applies bracketed paste
/// wrapping when the terminal has requested it.
pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(normalized.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        normalized.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Mods {
        Mods { ctrl: false, alt: false, meta: false, shift: false }
    }

    #[test]
    fn printable_passes_through() {
        assert_eq!(encode("a", &plain(), TermMode::empty()), Some(b"a".to_vec()));
        assert_eq!(encode("é", &plain(), TermMode::empty()), Some("é".as_bytes().to_vec()));
    }

    #[test]
    fn ctrl_letters() {
        let mods = Mods { ctrl: true, ..plain() };
        assert_eq!(encode("c", &mods, TermMode::empty()), Some(vec![0x03]));
        assert_eq!(encode("a", &mods, TermMode::empty()), Some(vec![0x01]));
    }

    #[test]
    fn arrows_normal_and_app_cursor() {
        assert_eq!(encode("\u{F700}", &plain(), TermMode::empty()), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode("\u{F700}", &plain(), TermMode::APP_CURSOR), Some(b"\x1bOA".to_vec()));
        let shift = Mods { shift: true, ..plain() };
        assert_eq!(encode("\u{F700}", &shift, TermMode::empty()), Some(b"\x1b[1;2A".to_vec()));
    }

    #[test]
    fn enter_backspace_escape() {
        assert_eq!(encode("\u{000a}", &plain(), TermMode::empty()), Some(b"\r".to_vec()));
        assert_eq!(encode("\u{0008}", &plain(), TermMode::empty()), Some(vec![0x7f]));
        assert_eq!(encode("\u{001b}", &plain(), TermMode::empty()), Some(vec![0x1b]));
    }

    #[test]
    fn meta_combos_are_left_to_the_app() {
        let meta = Mods { meta: true, ..plain() };
        assert_eq!(encode("v", &meta, TermMode::empty()), None);
        assert_eq!(encode("s", &meta, TermMode::empty()), None);
    }

    #[test]
    fn alt_prefixes_escape() {
        let alt = Mods { alt: true, ..plain() };
        assert_eq!(encode("b", &alt, TermMode::empty()), Some(vec![0x1b, b'b']));
    }

    #[test]
    fn bracketed_paste_wraps_and_normalizes() {
        let out = encode_paste("a\r\nb\n", TermMode::BRACKETED_PASTE);
        assert_eq!(out, b"\x1b[200~a\rb\r\x1b[201~".to_vec());
        assert_eq!(encode_paste("x\ny", TermMode::empty()), b"x\ry".to_vec());
    }
}
