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
    pub theme: String,
    pub font_family: String,
    pub font_size: f32,
    pub scrollback: usize,
    pub presets: Vec<Preset>,
    #[serde(default)]
    pub teams: Vec<Team>,
}

impl Config {
    /// Presets for a team index; None or out-of-range falls back to the
    /// default flat list.
    pub fn presets_for(&self, team: Option<usize>) -> &[Preset] {
        team.and_then(|i| self.teams.get(i))
            .map(|t| t.presets.as_slice())
            .unwrap_or(&self.presets)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            font_family: "Menlo".into(),
            font_size: 13.0,
            scrollback: 10_000,
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
        Ok(text) => match toml::from_str(&text) {
            Ok(config) => (config, false),
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
