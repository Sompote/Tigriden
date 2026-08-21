use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub label: String,
    pub command: String,
    #[serde(default = "default_true")]
    pub send_enter: bool,
}

fn default_true() -> bool {
    true
}

/// A named group of presets shown in its own window ("agent team").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub name: String,
    #[serde(default)]
    pub presets: Vec<Preset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Theme id from `crate::theme::THEMES` ("classic-dark", "vivid-light", …).
    /// The pre-0.1.2 values "dark" and "light" still load.
    pub theme: String,
    /// Accent override as "#rrggbb"; empty means the theme's own accent.
    pub accent: String,
    /// Editor / viewer font family. A family this machine does not have is
    /// swapped for one it does at startup — see [`Config::resolve_families`].
    pub font_family: String,
    /// Terminal font family. Empty means "follow `font_family`", which is what
    /// a config written before the two were split says, so an upgrade keeps
    /// the terminal exactly as it was until the user picks a font for it.
    #[serde(default)]
    pub term_font_family: String,
    /// Editor / viewer text size, in logical pixels.
    pub font_size: f32,
    /// Terminal text size, in logical pixels. `0` means "follow `font_size`",
    /// which is what a config written before the two were split says — so an
    /// upgrade keeps the terminal exactly as it was until the user moves it.
    #[serde(default)]
    pub term_font_size: f32,
    /// Chrome text size (sidebar, tabs, dialogs), in logical pixels.
    pub ui_font_size: f32,
    pub scrollback: usize,
    /// Whether new windows start with the git Changes panel on.
    pub show_changes: bool,
    pub presets: Vec<Preset>,
    #[serde(default)]
    pub teams: Vec<Team>,
    /// Which generation of the built-in preset list this config was written
    /// against. Older files are offered newly shipped agents once (see
    /// [`Config::adopt_new_presets`]); missing means the oldest generation.
    #[serde(default)]
    pub presets_version: u32,
}

/// Current generation of the built-in preset list. Bump this whenever an agent
/// is added to [`Config::default`] so existing configs are offered it.
pub const PRESETS_VERSION: u32 = 1;

/// The built-in preset list of each earlier generation, newest last. A config
/// still holding one of these verbatim has never been touched, so it can take
/// the new list; anything else is the user's own and is left alone.
const PRESET_GENERATIONS: [&[&str]; 1] = [&["claude", "codex", "gemini"]];

pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 28.0;
pub const MIN_UI_FONT_SIZE: f32 = 10.0;
pub const MAX_UI_FONT_SIZE: f32 = 18.0;

impl Config {
    /// Presets for a team index; None or out-of-range falls back to the
    /// default flat list.
    pub fn presets_for(&self, team: Option<usize>) -> &[Preset] {
        team.and_then(|i| self.teams.get(i))
            .map(|t| t.presets.as_slice())
            .unwrap_or(&self.presets)
    }

    /// Pulls hand-edited (or stale) values back into range so the rest of the
    /// app can use them without re-checking.
    pub fn sanitize(&mut self) {
        self.theme = crate::theme::by_id(&self.theme).id.to_string();
        if !self.accent.is_empty() && crate::theme::parse_hex(&self.accent).is_none() {
            self.accent.clear();
        }
        if self.font_family.trim().is_empty() {
            self.font_family = crate::fonts::default_family().into();
        }
        if self.term_font_family.trim().is_empty() {
            self.term_font_family = self.font_family.clone();
        }
        self.font_size = self.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if self.term_font_size <= 0.0 {
            self.term_font_size = self.font_size;
        }
        self.term_font_size = self.term_font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        self.ui_font_size = self.ui_font_size.clamp(MIN_UI_FONT_SIZE, MAX_UI_FONT_SIZE);
        self.scrollback = self.scrollback.clamp(200, 500_000);
    }

    /// Points both families at something the machine actually has. Called
    /// once the font database is up, since `sanitize` runs before it; returns
    /// whether either name moved, so the caller can write the substitution
    /// back instead of resolving it again on every launch.
    pub fn resolve_families(&mut self) -> bool {
        let font_family = crate::fonts::resolve(&self.font_family);
        let term_font_family = crate::fonts::resolve(&self.term_font_family);
        let changed =
            font_family != self.font_family || term_font_family != self.term_font_family;
        self.font_family = font_family;
        self.term_font_family = term_font_family;
        changed
    }

    /// Takes on newly shipped agent buttons, but only for a preset list still
    /// identical to a build's built-in one — a list the user has added to or
    /// pruned is theirs to manage. Returns whether anything changed, so the
    /// caller can record the new generation on disk and not ask again.
    fn adopt_new_presets(&mut self) -> bool {
        if self.presets_version >= PRESETS_VERSION {
            return false;
        }
        self.presets_version = PRESETS_VERSION;
        let labels: Vec<&str> = self.presets.iter().map(|p| p.label.as_str()).collect();
        if !PRESET_GENERATIONS.contains(&labels.as_slice()) {
            return true;
        }
        self.presets = Self::default().presets;
        true
    }

    /// Accent actually in use: the user's override, else the theme's own.
    pub fn accent_rgb(&self) -> [u8; 3] {
        crate::theme::parse_hex(&self.accent)
            .unwrap_or_else(|| crate::theme::by_id(&self.theme).ui.accent)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "classic-dark".into(),
            accent: String::new(),
            font_family: crate::fonts::default_family().into(),
            term_font_family: crate::fonts::default_family().into(),
            font_size: 13.0,
            term_font_size: 13.0,
            ui_font_size: 13.0,
            scrollback: 10_000,
            show_changes: false,
            presets: vec![
                Preset { label: "claude".into(), command: "claude".into(), send_enter: true },
                Preset { label: "codex".into(), command: "codex".into(), send_enter: true },
                Preset { label: "gemini".into(), command: "gemini".into(), send_enter: true },
                Preset { label: "opencode".into(), command: "opencode".into(), send_enter: true },
            ],
            teams: Vec::new(),
            presets_version: PRESETS_VERSION,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedState {
    pub folders: Vec<PathBuf>,
    pub active: usize,
    pub split_ratio: Option<f32>,
    /// Every folder ever added, most recent first — survives removal from the
    /// workbench so it can be re-opened from the Recent menu.
    pub recent_folders: Vec<PathBuf>,
}

fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("tigriden"))
}

pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

fn state_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("state.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_presets(labels: &[&str], version: u32) -> Config {
        Config {
            presets: labels
                .iter()
                .map(|l| Preset {
                    label: (*l).to_string(),
                    command: (*l).to_string(),
                    send_enter: true,
                })
                .collect(),
            presets_version: version,
            ..Config::default()
        }
    }

    fn labels(config: &Config) -> Vec<String> {
        config.presets.iter().map(|p| p.label.clone()).collect()
    }

    /// A config written before the terminal got its own size carries only
    /// `font_size`; the terminal has to keep the size it had rather than snap
    /// back to the built-in default.
    #[test]
    fn an_old_config_keeps_one_size_for_both() {
        let mut config: Config = toml::from_str(
            "theme = \"classic-dark\"\nfont_family = \"Menlo\"\nfont_size = 17.0\n",
        )
        .expect("an old config still parses");
        config.sanitize();
        assert_eq!(config.term_font_size, 17.0, "the terminal inherits the one size on file");
        assert_eq!(config.font_size, 17.0);
    }

    /// Same for the family: an upgrade must not leave the terminal in a
    /// different font from the one the user has been reading all along.
    #[test]
    fn an_old_config_keeps_one_family_for_both() {
        let mut config: Config =
            toml::from_str("font_family = \"Courier New\"\n").expect("an old config still parses");
        config.sanitize();
        assert_eq!(config.term_font_family, "Courier New", "the terminal inherits the one family");
        assert_eq!(config.font_family, "Courier New");
    }

    /// Once the terminal has a family of its own, editing the editor's leaves
    /// it alone.
    #[test]
    fn the_two_families_are_independent() {
        let mut config: Config =
            toml::from_str("font_family = \"Courier New\"\nterm_font_family = \"Menlo\"\n")
                .expect("both families parse");
        config.sanitize();
        assert_eq!((config.font_family.as_str(), config.term_font_family.as_str()),
            ("Courier New", "Menlo"));

        config.font_family = "Monaco".into();
        config.sanitize();
        assert_eq!(config.term_font_family, "Menlo", "the terminal keeps its own font");
    }

    /// The macOS default in a config carried to a machine without it (a
    /// Windows one, say) must not be handed to cosmic-text, which would answer
    /// with the platform's proportional interface font.
    #[test]
    fn a_family_this_machine_lacks_is_swapped_for_one_it_has() {
        let mut db = cosmic_text::fontdb::Database::new();
        db.load_font_file("vendor/cosmic-text/fonts/FiraMono-Medium.ttf")
            .expect("the vendored mono font is there");
        crate::fonts::index(&db);

        let mut config: Config =
            toml::from_str("font_family = \"Menlo\"\n").expect("the config parses");
        config.sanitize();
        assert!(config.resolve_families(), "the substitution is worth writing back");
        assert_eq!(config.font_family, "Fira Mono");
        assert_eq!(config.term_font_family, "Fira Mono");
        assert!(!config.resolve_families(), "resolving again changes nothing");
    }

    /// Once the two are set apart they stay apart, each clamped on its own.
    #[test]
    fn the_two_sizes_are_independent() {
        let mut config: Config = toml::from_str(
            "font_size = 15.0\nterm_font_size = 11.0\n",
        )
        .expect("both sizes parse");
        config.sanitize();
        assert_eq!((config.font_size, config.term_font_size), (15.0, 11.0));

        config.term_font_size = MAX_FONT_SIZE + 40.0;
        config.sanitize();
        assert_eq!(config.term_font_size, MAX_FONT_SIZE, "clamped, not reset to the editor size");
        assert_eq!(config.font_size, 15.0, "the editor size is untouched");
    }

    #[test]
    fn untouched_preset_lists_take_on_new_agents() {
        let mut config = with_presets(&["claude", "codex", "gemini"], 0);
        assert!(config.adopt_new_presets(), "an older generation migrates");
        assert_eq!(labels(&config), labels(&Config::default()));
        assert_eq!(config.presets_version, PRESETS_VERSION);
        // Once recorded, it never migrates again — so an agent the user drops
        // afterwards stays dropped.
        config.presets.pop();
        assert!(!config.adopt_new_presets(), "the current generation is left alone");
        assert_eq!(config.presets.len(), Config::default().presets.len() - 1);
    }

    #[test]
    fn customised_preset_lists_are_left_alone() {
        let mut config = with_presets(&["claude", "my-agent"], 0);
        assert!(config.adopt_new_presets(), "the generation is still recorded");
        assert_eq!(labels(&config), vec!["claude", "my-agent"], "custom presets survive");
    }
}

/// Loads the config, writing defaults on first run. A malformed file falls
/// back to defaults but is left untouched on disk.
pub fn load_config() -> (Config, bool) {
    let Some(path) = config_path() else { return (Config::default(), false) };
    match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(mut config) => {
                config.sanitize();
                // Write the new generation straight back, so an agent the user
                // then removes is not offered again on the next launch.
                if config.adopt_new_presets() {
                    save_config(&config);
                }
                (config, false)
            }
            Err(err) => {
                eprintln!("tigriden: malformed {}: {err}", path.display());
                (Config::default(), true)
            }
        },
        Err(_) => {
            let config = Config::default();
            if let Some(dir) = config_dir() {
                let _ = std::fs::create_dir_all(&dir);
                if let Ok(text) = toml::to_string_pretty(&config) {
                    let _ = std::fs::write(&path, text);
                }
            }
            (config, false)
        }
    }
}

/// Writes config.toml (the Settings dialog's persistence). Rewrites the whole
/// file, so hand-written comments do not survive a change made in Settings.
pub fn save_config(config: &Config) {
    let Some(path) = config_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match toml::to_string_pretty(config) {
        Ok(text) => {
            if let Err(err) = std::fs::write(&path, text) {
                eprintln!("tigriden: cannot write {}: {err}", path.display());
            }
        }
        Err(err) => eprintln!("tigriden: cannot serialize config: {err}"),
    }
}

pub fn load_state() -> PersistedState {
    state_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_state(state: &PersistedState) {
    let Some(path) = state_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = toml::to_string_pretty(state) {
        let _ = std::fs::write(path, text);
    }
}
