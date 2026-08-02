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
    pub font_family: String,
    /// Terminal / editor text size, in logical pixels.
    pub font_size: f32,
    /// Chrome text size (sidebar, tabs, dialogs), in logical pixels.
    pub ui_font_size: f32,
    pub scrollback: usize,
    /// Whether new windows start with the git Changes panel on.
    pub show_changes: bool,
    pub presets: Vec<Preset>,
    #[serde(default)]
    pub teams: Vec<Team>,
}

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
            self.font_family = "Menlo".into();
        }
        self.font_size = self.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        self.ui_font_size = self.ui_font_size.clamp(MIN_UI_FONT_SIZE, MAX_UI_FONT_SIZE);
        self.scrollback = self.scrollback.clamp(200, 500_000);
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
            font_family: "Menlo".into(),
            font_size: 13.0,
            ui_font_size: 13.0,
            scrollback: 10_000,
            show_changes: false,
            presets: vec![
                Preset { label: "claude".into(), command: "claude".into(), send_enter: true },
                Preset { label: "codex".into(), command: "codex".into(), send_enter: true },
                Preset { label: "gemini".into(), command: "gemini".into(), send_enter: true },
            ],
            teams: Vec::new(),
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

/// Loads the config, writing defaults on first run. A malformed file falls
/// back to defaults but is left untouched on disk.
pub fn load_config() -> (Config, bool) {
    let Some(path) = config_path() else { return (Config::default(), false) };
    match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(mut config) => {
                config.sanitize();
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
