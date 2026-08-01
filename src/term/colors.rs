use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

/// Default dark palette (16 base colors), loosely gruvbox-ish neutral.
const DARK: [[u8; 3]; 16] = [
    [0x1e, 0x22, 0x27], // black (background)
    [0xe0, 0x5f, 0x65], // red
    [0x8c, 0xc2, 0x65], // green
    [0xe2, 0xb0, 0x4c], // yellow
    [0x5c, 0x9c, 0xe0], // blue
    [0xc6, 0x84, 0xdd], // magenta
    [0x51, 0xba, 0xba], // cyan
    [0xd6, 0xdb, 0xe1], // white (foreground)
    [0x5c, 0x65, 0x70], // bright black
    [0xef, 0x83, 0x88], // bright red
    [0xa5, 0xd6, 0x80], // bright green
    [0xf0, 0xc6, 0x74], // bright yellow
    [0x85, 0xb8, 0xef], // bright blue
    [0xd9, 0xa4, 0xed], // bright magenta
    [0x7b, 0xd4, 0xd4], // bright cyan
    [0xf0, 0xf3, 0xf6], // bright white
];

const LIGHT: [[u8; 3]; 16] = [
    [0xfa, 0xfa, 0xfa],
    [0xc7, 0x39, 0x40],
    [0x44, 0x84, 0x2c],
    [0xa8, 0x71, 0x0f],
    [0x2b, 0x63, 0xbf],
    [0x94, 0x40, 0xb3],
    [0x1f, 0x8a, 0x8a],
    [0x24, 0x29, 0x2f],
    [0x8a, 0x91, 0x99],
    [0xe0, 0x5f, 0x65],
    [0x5f, 0xa5, 0x44],
    [0xc9, 0x91, 0x2b],
    [0x4d, 0x84, 0xd9],
    [0xb0, 0x62, 0xcc],
    [0x3d, 0xa8, 0xa8],
    [0x11, 0x14, 0x18],
];

pub fn base_palette(dark: bool) -> &'static [[u8; 3]; 16] {
    if dark { &DARK } else { &LIGHT }
}

pub fn named_rgb(named: NamedColor, dark: bool) -> [u8; 3] {
    let p = base_palette(dark);
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

fn indexed_rgb(idx: u8, dark: bool) -> [u8; 3] {
    match idx {
        0..=15 => base_palette(dark)[idx as usize],
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
pub fn osc_color(index: usize, dark: bool) -> [u8; 3] {
    let p = base_palette(dark);
    match index {
        0..=255 => indexed_rgb(index as u8, dark),
        256 => p[7], // foreground
        257 => p[0], // background
        _ => p[7],   // cursor and friends
    }
}

/// Resolves a cell color against runtime overrides (OSC 4 etc.) then the
/// static palette.
pub fn resolve(color: Color, overrides: &Colors, dark: bool) -> [u8; 3] {
    match color {
        Color::Spec(rgb) => [rgb.r, rgb.g, rgb.b],
        Color::Named(named) => match overrides[named] {
            Some(rgb) => [rgb.r, rgb.g, rgb.b],
            None => named_rgb(named, dark),
        },
        Color::Indexed(idx) => match overrides[idx as usize] {
            Some(rgb) => [rgb.r, rgb.g, rgb.b],
            None => indexed_rgb(idx, dark),
        },
    }
}

pub fn to_rgb(c: [u8; 3]) -> Rgb {
    Rgb { r: c[0], g: c[1], b: c[2] }
}
