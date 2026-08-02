use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

use crate::theme::ThemeDef;

/// ANSI 0-15 for a theme; index 0 is the background, 7 the foreground.
pub fn base_palette(theme: &'static ThemeDef) -> &'static [[u8; 3]; 16] {
    &theme.term
}

pub fn named_rgb(named: NamedColor, theme: &'static ThemeDef) -> [u8; 3] {
    let p = base_palette(theme);
    match named {
        NamedColor::Black => p[0],
        NamedColor::Red => p[1],
        NamedColor::Green => p[2],
        NamedColor::Yellow => p[3],
        NamedColor::Blue => p[4],
        NamedColor::Magenta => p[5],
        NamedColor::Cyan => p[6],
        NamedColor::White => p[7],
        NamedColor::BrightBlack => p[8],
        NamedColor::BrightRed => p[9],
        NamedColor::BrightGreen => p[10],
        NamedColor::BrightYellow => p[11],
        NamedColor::BrightBlue => p[12],
        NamedColor::BrightMagenta => p[13],
        NamedColor::BrightCyan => p[14],
        NamedColor::BrightWhite => p[15],
        NamedColor::Foreground | NamedColor::BrightForeground => p[7],
        NamedColor::Background => p[0],
        NamedColor::Cursor => p[7],
        NamedColor::DimBlack => dim(p[0]),
        NamedColor::DimRed => dim(p[1]),
        NamedColor::DimGreen => dim(p[2]),
        NamedColor::DimYellow => dim(p[3]),
        NamedColor::DimBlue => dim(p[4]),
        NamedColor::DimMagenta => dim(p[5]),
        NamedColor::DimCyan => dim(p[6]),
        NamedColor::DimWhite | NamedColor::DimForeground => dim(p[7]),
    }
}

fn dim(c: [u8; 3]) -> [u8; 3] {
    [(c[0] as u16 * 2 / 3) as u8, (c[1] as u16 * 2 / 3) as u8, (c[2] as u16 * 2 / 3) as u8]
}

fn indexed_rgb(idx: u8, theme: &'static ThemeDef) -> [u8; 3] {
    match idx {
        0..=15 => base_palette(theme)[idx as usize],
        16..=231 => {
            let i = idx as u16 - 16;
            let steps = [0u8, 95, 135, 175, 215, 255];
            [
                steps[(i / 36) as usize],
                steps[((i / 6) % 6) as usize],
                steps[(i % 6) as usize],
            ]
        }
        232..=255 => {
            let v = 8 + (idx as u16 - 232) * 10;
            [v as u8, v as u8, v as u8]
        }
    }
}

/// Palette lookup for OSC color-query answerbacks (indices follow alacritty's
/// layout: 0-255 palette, then foreground/background/cursor).
pub fn osc_color(index: usize, theme: &'static ThemeDef) -> [u8; 3] {
    let p = base_palette(theme);
    match index {
        0..=255 => indexed_rgb(index as u8, theme),
        256 => p[7], // foreground
        257 => p[0], // background
        _ => p[7],   // cursor and friends
    }
}

/// Resolves a cell color against runtime overrides (OSC 4 etc.) then the
/// static palette.
pub fn resolve(color: Color, overrides: &Colors, theme: &'static ThemeDef) -> [u8; 3] {
    match color {
        Color::Spec(rgb) => [rgb.r, rgb.g, rgb.b],
        Color::Named(named) => match overrides[named] {
            Some(rgb) => [rgb.r, rgb.g, rgb.b],
            None => named_rgb(named, theme),
        },
        Color::Indexed(idx) => match overrides[idx as usize] {
            Some(rgb) => [rgb.r, rgb.g, rgb.b],
            None => indexed_rgb(idx, theme),
        },
    }
}

pub fn to_rgb(c: [u8; 3]) -> Rgb {
    Rgb { r: c[0], g: c[1], b: c[2] }
}
