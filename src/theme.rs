//! Named themes: one definition drives the Slint chrome, the terminal's
//! 16-color palette and the editor's syntect theme, so everything shifts
//! together when the user picks a new one in Settings.

/// Colors for the Slint chrome (sidebar, tab strips, dialogs).
#[derive(Clone, Copy)]
pub struct UiColors {
    pub bg: [u8; 3],
    pub panel: [u8; 3],
    pub panel_hover: [u8; 3],
    pub selection: [u8; 3],
    pub border: [u8; 3],
    pub text: [u8; 3],
    pub text_dim: [u8; 3],
    pub accent: [u8; 3],
}

pub struct ThemeDef {
    /// Stable id written to config.toml.
    pub id: &'static str,
    pub label: &'static str,
    /// "classic" | "minimal" | "vivid" — the style axis in Settings.
    pub style: &'static str,
    pub dark: bool,
    /// Theme name inside syntect's default set (editor highlighting).
    pub syntect: &'static str,
    pub ui: UiColors,
    /// ANSI 0-15; index 0 is the background and 7 the default foreground.
    pub term: [[u8; 3]; 16],
}

impl ThemeDef {
    pub fn mode(&self) -> &'static str {
        if self.dark {
            "dark"
        } else {
            "light"
        }
    }
}

/// Accent swatches offered in Settings; an empty id means "theme default".
pub const ACCENTS: [(&str, &str); 7] = [
    ("", "Theme"),
    ("#e8912d", "Amber"),
    ("#3aa9ff", "Blue"),
    ("#34c759", "Green"),
    ("#af7aff", "Purple"),
    ("#ff4d8d", "Pink"),
    ("#22c2b0", "Teal"),
];

pub static THEMES: [ThemeDef; 6] = [
    ThemeDef {
        id: "classic-dark",
        label: "Classic Dark",
        style: "classic",
        dark: true,
        syntect: "base16-eighties.dark",
        ui: UiColors {
            bg: [0x1e, 0x22, 0x27],
            panel: [0x24, 0x29, 0x2f],
            panel_hover: [0x2d, 0x33, 0x3b],
            selection: [0x39, 0x41, 0x4b],
            border: [0x33, 0x3a, 0x42],
            text: [0xd6, 0xdb, 0xe1],
            text_dim: [0x8b, 0x94, 0x9e],
            accent: [0xe8, 0x91, 0x2d],
        },
        term: [
            [0x1e, 0x22, 0x27],
            [0xe0, 0x5f, 0x65],
            [0x8c, 0xc2, 0x65],
            [0xe2, 0xb0, 0x4c],
            [0x5c, 0x9c, 0xe0],
            [0xc6, 0x84, 0xdd],
            [0x51, 0xba, 0xba],
            [0xd6, 0xdb, 0xe1],
            [0x5c, 0x65, 0x70],
            [0xef, 0x83, 0x88],
            [0xa5, 0xd6, 0x80],
            [0xf0, 0xc6, 0x74],
            [0x85, 0xb8, 0xef],
            [0xd9, 0xa4, 0xed],
            [0x7b, 0xd4, 0xd4],
            [0xf0, 0xf3, 0xf6],
        ],
    },
    ThemeDef {
        id: "classic-light",
        label: "Classic Light",
        style: "classic",
        dark: false,
        syntect: "InspiredGitHub",
        ui: UiColors {
            bg: [0xfa, 0xfa, 0xfa],
            panel: [0xf0, 0xf0, 0xf0],
            panel_hover: [0xe4, 0xe4, 0xe4],
            selection: [0xd8, 0xd8, 0xd8],
            border: [0xd0, 0xd0, 0xd0],
            text: [0x24, 0x29, 0x2f],
            text_dim: [0x6e, 0x77, 0x81],
            accent: [0xc9, 0x76, 0x1f],
        },
        term: [
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
        ],
    },
    ThemeDef {
        id: "minimal-dark",
        label: "Minimal Dark",
        style: "minimal",
        dark: true,
        syntect: "base16-ocean.dark",
        ui: UiColors {
            bg: [0x16, 0x18, 0x1a],
            panel: [0x1b, 0x1e, 0x21],
            panel_hover: [0x23, 0x27, 0x2b],
            selection: [0x2b, 0x30, 0x35],
            border: [0x24, 0x28, 0x2c],
            text: [0xc9, 0xce, 0xd3],
            text_dim: [0x76, 0x7c, 0x83],
            accent: [0xa8, 0xb0, 0xb8],
        },
        term: [
            [0x16, 0x18, 0x1a],
            [0xb5, 0x7b, 0x7b],
            [0x93, 0xa8, 0x88],
            [0xbf, 0xa9, 0x7c],
            [0x85, 0x98, 0xad],
            [0xa2, 0x94, 0xb0],
            [0x84, 0xa5, 0xa5],
            [0xc9, 0xce, 0xd3],
            [0x5a, 0x60, 0x66],
            [0xc9, 0x94, 0x94],
            [0xa9, 0xbd, 0x9e],
            [0xd3, 0xbf, 0x93],
            [0x9d, 0xb0, 0xc4],
            [0xb6, 0xa9, 0xc4],
            [0x9b, 0xbc, 0xbc],
            [0xe6, 0xea, 0xee],
        ],
    },
    ThemeDef {
        id: "minimal-light",
        label: "Minimal Light",
        style: "minimal",
        dark: false,
        syntect: "base16-ocean.light",
        ui: UiColors {
            bg: [0xff, 0xff, 0xff],
            panel: [0xf7, 0xf7, 0xf7],
            panel_hover: [0xef, 0xef, 0xef],
            selection: [0xe3, 0xe3, 0xe3],
            border: [0xe6, 0xe6, 0xe6],
            text: [0x24, 0x27, 0x2b],
            text_dim: [0x76, 0x7b, 0x81],
            accent: [0x4d, 0x56, 0x5f],
        },
        term: [
            [0xff, 0xff, 0xff],
            [0xa1, 0x5c, 0x5c],
            [0x5f, 0x7a, 0x51],
            [0x8a, 0x72, 0x38],
            [0x4d, 0x64, 0x80],
            [0x6f, 0x61, 0x80],
            [0x4d, 0x73, 0x73],
            [0x24, 0x27, 0x2b],
            [0x9a, 0xa0, 0xa6],
            [0xb5, 0x7b, 0x7b],
            [0x7c, 0x94, 0x70],
            [0xa6, 0x8f, 0x56],
            [0x6b, 0x81, 0xa0],
            [0x8b, 0x7d, 0x9c],
            [0x6b, 0x90, 0x90],
            [0x10, 0x12, 0x14],
        ],
    },
    ThemeDef {
        id: "vivid-dark",
        label: "Vivid Dark",
        style: "vivid",
        dark: true,
        syntect: "base16-mocha.dark",
        ui: UiColors {
            bg: [0x0e, 0x10, 0x16],
            panel: [0x16, 0x1a, 0x24],
            panel_hover: [0x1f, 0x25, 0x33],
            selection: [0x2a, 0x32, 0x44],
            border: [0x22, 0x2a, 0x3a],
            text: [0xe8, 0xec, 0xf6],
            text_dim: [0x8b, 0x95, 0xad],
            accent: [0xff, 0x7a, 0x1a],
        },
        term: [
            [0x0e, 0x10, 0x16],
            [0xff, 0x4d, 0x6d],
            [0x38, 0xe0, 0x8b],
            [0xff, 0xd5, 0x3d],
            [0x3a, 0xa9, 0xff],
            [0xc7, 0x7d, 0xff],
            [0x22, 0xe0, 0xd6],
            [0xe8, 0xec, 0xf6],
            [0x4d, 0x56, 0x6e],
            [0xff, 0x7d, 0x94],
            [0x6f, 0xf0, 0xae],
            [0xff, 0xe4, 0x79],
            [0x74, 0xc4, 0xff],
            [0xdb, 0xa6, 0xff],
            [0x6e, 0xf2, 0xea],
            [0xff, 0xff, 0xff],
        ],
    },
    ThemeDef {
        id: "vivid-light",
        label: "Vivid Light",
        style: "vivid",
        dark: false,
        syntect: "Solarized (light)",
        ui: UiColors {
            bg: [0xff, 0xff, 0xff],
            panel: [0xf2, 0xf4, 0xfb],
            panel_hover: [0xe7, 0xeb, 0xf8],
            selection: [0xdb, 0xe2, 0xf6],
            border: [0xdd, 0xe3, 0xf1],
            text: [0x10, 0x13, 0x1c],
            text_dim: [0x5a, 0x63, 0x79],
            accent: [0xe3, 0x52, 0x05],
        },
        term: [
            [0xff, 0xff, 0xff],
            [0xe0, 0x11, 0x3a],
            [0x00, 0x99, 0x4d],
            [0xd9, 0x86, 0x00],
            [0x0a, 0x6e, 0xe0],
            [0x9b, 0x2f, 0xd6],
            [0x00, 0x9e, 0xa0],
            [0x10, 0x13, 0x1c],
            [0x8c, 0x93, 0xa5],
            [0xff, 0x3b, 0x5c],
            [0x00, 0xb8, 0x5e],
            [0xf0, 0xa3, 0x00],
            [0x2f, 0x8b, 0xff],
            [0xb8, 0x55, 0xef],
            [0x00, 0xc2, 0xc4],
            [0x00, 0x00, 0x00],
        ],
    },
];

pub fn default_theme() -> &'static ThemeDef {
    &THEMES[0]
}

/// Theme for a config id. Accepts the pre-0.1.2 ids "dark" and "light";
/// anything unknown falls back to Classic Dark.
pub fn by_id(id: &str) -> &'static ThemeDef {
    let id = match id {
        "dark" => "classic-dark",
        "light" => "classic-light",
        other => other,
    };
    THEMES.iter().find(|t| t.id == id).unwrap_or_else(|| default_theme())
}

pub fn index_of(id: &str) -> u8 {
    let theme = by_id(id);
    THEMES.iter().position(|t| t.id == theme.id).unwrap_or(0) as u8
}

pub fn by_index(index: u8) -> &'static ThemeDef {
    THEMES.get(index as usize).unwrap_or_else(|| default_theme())
}

/// Theme for a (style, mode) pair — how Settings composes its two chip rows.
/// Falls back to the same style in the other mode, then to Classic Dark.
pub fn by_style_mode(style: &str, mode: &str) -> &'static ThemeDef {
    let dark = mode != "light";
    THEMES
        .iter()
        .find(|t| t.style == style && t.dark == dark)
        .or_else(|| THEMES.iter().find(|t| t.style == style))
        .unwrap_or_else(|| default_theme())
}

/// Parses "#rrggbb" (or "rrggbb"); None for anything else, including "".
pub fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_style_has_both_modes() {
        for style in ["classic", "minimal", "vivid"] {
            for mode in ["dark", "light"] {
                let theme = by_style_mode(style, mode);
                assert_eq!(theme.style, style, "{style}/{mode}");
                assert_eq!(theme.mode(), mode, "{style}/{mode}");
            }
        }
    }

    #[test]
    fn ids_round_trip_through_the_index_shared_with_pty_threads() {
        for theme in THEMES.iter() {
            assert_eq!(by_index(index_of(theme.id)).id, theme.id);
        }
    }

    #[test]
    fn legacy_and_unknown_ids_fall_back() {
        assert_eq!(by_id("dark").id, "classic-dark");
        assert_eq!(by_id("light").id, "classic-light");
        assert_eq!(by_id("nonsense").id, "classic-dark");
    }

    /// A typo here would make every file fail to open in that theme.
    #[test]
    fn syntect_theme_names_exist() {
        let system = cosmic_text::SyntaxSystem::new();
        for theme in THEMES.iter() {
            assert!(
                system.theme_set.themes.contains_key(theme.syntect),
                "{}: missing syntect theme {}",
                theme.id,
                theme.syntect
            );
        }
    }

    #[test]
    fn accent_swatches_parse() {
        for (id, _) in ACCENTS.iter().skip(1) {
            assert!(parse_hex(id).is_some(), "{id}");
        }
        assert_eq!(parse_hex("#3aa9ff"), Some([0x3a, 0xa9, 0xff]));
        assert_eq!(parse_hex(""), None);
        assert_eq!(parse_hex("#zzzzzz"), None);
    }
}
