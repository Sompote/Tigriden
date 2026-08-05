use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{viewport_to_point, TermMode};
use cosmic_text::{FontSystem, SwashCache, SyntaxSystem};
use slint::{ComponentHandle, Image, ModelRc, SharedString, VecModel};

use crate::config::{self, Config, PersistedState};
use crate::editor::{EditorState, KeyOutcome};
use crate::session::{Session, TermHandle};
use crate::viewer::{self, ViewerState};
use crate::term::keys::{self, Mods};
use crate::term::render::TermRenderer;
use crate::term::{TermHooks, TermSession};
use crate::theme::{self, ThemeDef};
use crate::{AccentOption, MainWindow, PresetItem, Theme as UiTheme, TreeRow};

static SYNTAX_SYSTEM: OnceLock<SyntaxSystem> = OnceLock::new();
static NEXT_TERM_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_APP_ID: AtomicU64 = AtomicU64::new(1);

fn syntax_system() -> &'static SyntaxSystem {
    SYNTAX_SYSTEM.get_or_init(SyntaxSystem::new)
}

struct WindowEntry {
    id: u64,
    app: Rc<RefCell<App>>,
    // Strong handle: keeps the shown window alive while registered.
    _ui: MainWindow,
}

thread_local! {
    static WINDOWS: RefCell<Vec<WindowEntry>> = const { RefCell::new(Vec::new()) };
    /// The live config, shared by every window: Settings edits go here first,
    /// then out to disk and to each window.
    static CONFIG: RefCell<Config> = RefCell::new(Config::default());
    /// Font family names handed to cosmic-text, which wants `&'static str`.
    /// Interned so repeated Settings edits do not leak a string each time.
    static FONT_NAMES: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

pub fn set_config(config: Config) {
    CONFIG.with(|slot| *slot.borrow_mut() = config);
}

pub fn config() -> Config {
    CONFIG.with(|slot| slot.borrow().clone())
}

fn intern_font(name: &str) -> &'static str {
    FONT_NAMES.with(|slot| {
        let mut names = slot.borrow_mut();
        if let Some(found) = names.iter().find(|n| **n == name) {
            return *found;
        }
        let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
        names.push(leaked);
        leaked
    })
}

pub fn register(id: u64, app: Rc<RefCell<App>>, ui: MainWindow) {
    WINDOWS.with(|slot| slot.borrow_mut().push(WindowEntry { id, app, _ui: ui }));
}

/// Runs `f` against the window's app; no-op if the window is gone (late PTY
/// events after close). Used by UI callbacks and closures arriving via
/// invoke_from_event_loop (PTY repaints, watcher events, timers).
pub fn with_app_id(id: u64, f: impl FnOnce(&mut App)) {
    // Clone the Rc out of the registry borrow so `f` can't deadlock a
    // registry re-borrow.
    let app = WINDOWS.with(|slot| {
        slot.borrow().iter().find(|e| e.id == id).map(|e| e.app.clone())
    });
    if let Some(app) = app {
        f(&mut app.borrow_mut());
    }
}

/// Drops the window's registry entry (closing it); returns how many remain.
pub fn remove_window(id: u64) -> usize {
    WINDOWS.with(|slot| {
        let mut windows = slot.borrow_mut();
        windows.retain(|e| e.id != id);
        windows.len()
    })
}

/// Shuts down every remaining window's app (⌘Q quits without a per-window
/// close_requested).
pub fn shutdown_all() {
    let apps = WINDOWS.with(|slot| {
        slot.borrow().iter().map(|e| e.app.clone()).collect::<Vec<_>>()
    });
    for app in apps {
        app.borrow_mut().shutdown();
    }
    WINDOWS.with(|slot| slot.borrow_mut().clear());
}

fn slint_color(rgb: [u8; 3]) -> slint::Color {
    slint::Color::from_rgb_u8(rgb[0], rgb[1], rgb[2])
}

/// Copies the active theme (and the accent / UI text size overrides) into the
/// Slint `Theme` global that the chrome binds to.
fn push_theme(ui: &MainWindow, config: &Config) {
    let theme = theme::by_id(&config.theme);
    let global = ui.global::<UiTheme>();
    global.set_dark(theme.dark);
    global.set_bg(slint_color(theme.ui.bg));
    global.set_panel(slint_color(theme.ui.panel));
    global.set_panel_hover(slint_color(theme.ui.panel_hover));
    global.set_selection(slint_color(theme.ui.selection));
    global.set_border(slint_color(theme.ui.border));
    global.set_text(slint_color(theme.ui.text));
    global.set_text_dim(slint_color(theme.ui.text_dim));
    global.set_accent(slint_color(config.accent_rgb()));
    global.set_font_size(config.ui_font_size);
}

/// Saves the edited config and pushes it into every open window. Called from
/// UI callbacks that are *not* wrapped in `with_app_id`, so no window is
/// borrowed while we walk the registry.
fn apply_and_save(config: Config) {
    set_config(config.clone());
    config::save_config(&config);
    let apps = WINDOWS.with(|slot| slot.borrow().iter().map(|e| e.app.clone()).collect::<Vec<_>>());
    for app in apps {
        app.borrow_mut().apply_config(config.clone());
    }
}

/// One control in the Settings dialog changed. Values arrive as strings;
/// anything unparseable leaves the config untouched.
pub fn settings_changed(key: &str, value: &str) {
    let mut config = config();
    let current = theme::by_id(&config.theme);
    match key {
        "mode" => config.theme = theme::by_style_mode(current.style, value).id.into(),
        "style" => config.theme = theme::by_style_mode(value, current.mode()).id.into(),
        "accent" => config.accent = value.to_string(),
        "font-family" => config.font_family = value.to_string(),
        "font-size" => match value.parse::<f32>() {
            Ok(size) => config.font_size = size,
            Err(_) => return,
        },
        "font-size-step" => match value.parse::<f32>() {
            Ok(step) => config.font_size += step,
            Err(_) => return,
        },
        "ui-font-size" => match value.parse::<f32>() {
            Ok(size) => config.ui_font_size = size,
            Err(_) => return,
        },
        "scrollback" => match value.parse::<usize>() {
            Ok(lines) => config.scrollback = lines,
            Err(_) => return,
        },
        "show-changes" => config.show_changes = value == "true",
        _ => return,
    }
    config.sanitize();
    apply_and_save(config);
}

/// Settings ▸ Reset to defaults — keeps the user's presets and teams, which
/// the dialog does not edit.
pub fn settings_reset() {
    let current = config();
    let fresh = Config { presets: current.presets, teams: current.teams, ..Config::default() };
    apply_and_save(fresh);
}

/// Shows config.toml in the file manager (presets and teams are edited there).
pub fn reveal_config() {
    let Some(path) = config::config_path() else { return };
    if !path.exists() {
        config::save_config(&config());
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(&path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg("/select,").arg(&path).spawn();
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let _ = std::process::Command::new("xdg-open")
        .arg(path.parent().unwrap_or(&path))
        .spawn();
}

/// Raw `+[NSEvent modifierFlags]`: the current hardware modifier state,
/// readable by the app itself without any TCC permission (unlike the
/// CGEventSource APIs, which are Input-Monitoring-gated on modern macOS).
#[cfg(target_os = "macos")]
fn ns_modifier_flags() -> usize {
    use std::ffi::c_void;
    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const u8) -> *mut c_void;
        fn sel_registerName(name: *const u8) -> *mut c_void;
        fn objc_msgSend(receiver: *mut c_void, sel: *mut c_void) -> usize;
    }
    // NSEvent lives in AppKit; winit links it anyway, this makes it explicit.
    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}
    unsafe {
        let cls = objc_getClass(c"NSEvent".as_ptr().cast());
        if cls.is_null() {
            return 0;
        }
        objc_msgSend(cls, sel_registerName(c"modifierFlags".as_ptr().cast()))
    }
}

/// Whether the wheel-zoom modifier (⌘ or Ctrl) is held right now, asked
/// straight from AppKit. Slint tracks modifiers from key events, but on macOS
/// the ⌘ flags-changed event does not reliably reach that bookkeeping while
/// the pointer is scrolling, so scroll events report stale modifiers.
#[cfg(target_os = "macos")]
fn zoom_modifier_down() -> bool {
    const CMD: usize = 1 << 20; // NSEventModifierFlagCommand
    const CTRL: usize = 1 << 18; // NSEventModifierFlagControl
    ns_modifier_flags() & (CMD | CTRL) != 0
}

/// Current (ctrl, alt, meta, shift) held state of the physical keys, straight
/// from the OS. Used to correct slint's per-event modifier flags, which lag
/// behind reality when the event queue is busy.
pub fn native_modifier_state() -> (bool, bool, bool, bool) {
    #[cfg(target_os = "macos")]
    {
        let raw = ns_modifier_flags();
        return (
            raw & (1 << 18) != 0, // NSEventModifierFlagControl
            raw & (1 << 19) != 0, // NSEventModifierFlagOption
            raw & (1 << 20) != 0, // NSEventModifierFlagCommand
            raw & (1 << 17) != 0, // NSEventModifierFlagShift
        );
    }
    #[allow(unreachable_code)]
    (false, false, false, false)
}

/// Elsewhere the slint-reported modifier is trustworthy.
#[cfg(not(target_os = "macos"))]
fn zoom_modifier_down() -> bool {
    false
}

#[derive(Clone)]
enum RowTarget {
    Header(usize),
    Dir(usize, PathBuf),
    File(usize, PathBuf),
    ChangesHeader(usize),
    /// Absolute path + combined status letter (M/A/D) of a changed file.
    Change(usize, PathBuf, char),
}

enum PendingAction {
    OpenFile(usize, PathBuf),
    OpenDiff(usize, PathBuf),
    CloseSession(usize),
}

enum Banner {
    None,
    Unsaved(PendingAction),
    DiskChanged,
    ConfirmDiscard(usize, PathBuf, char),
    ConfirmDiscardAll(usize),
}

/// What the name-input dialog will do with the entered name.
enum NameAction {
    NewFile(usize, PathBuf),
    NewFolder(usize, PathBuf),
    Rename(usize, PathBuf),
}


#[cfg(feature = "framedump")]
fn dump_frame(name: &str, buffer: &slint::SharedPixelBuffer<slint::Rgba8Pixel>) {
    let Ok(dir) = std::env::var("TIGRIDEN_DUMP") else { return };
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    let _ = std::fs::create_dir_all(&dir);
    let file = match std::fs::File::create(&path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), buffer.width(), buffer.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    if let Ok(mut writer) = encoder.write_header() {
        let bytes: &[u8] = bytemuck_cast(buffer.as_slice());
        let _ = writer.write_image_data(bytes);
    }
}

#[cfg(feature = "framedump")]
fn bytemuck_cast(pixels: &[slint::Rgba8Pixel]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(pixels.as_ptr() as *const u8, pixels.len() * 4) }
}

pub struct App {
    pub id: u64,
    is_primary: bool,
    ui: slint::Weak<MainWindow>,
    pub config: Config,
    /// Team index this window was opened with (None = the flat preset list).
    team: Option<usize>,
    presets: Vec<crate::config::Preset>,
    theme: &'static ThemeDef,
    /// Theme index shared with the PTY threads for OSC color answerbacks.
    theme_index: Arc<AtomicU8>,
    font_family: &'static str,
    sessions: Vec<Session>,
    active: usize,
    recents: Vec<PathBuf>,
    row_map: Vec<RowTarget>,
    font_system: FontSystem,
    swash_cache: SwashCache,
    term_renderer: Option<TermRenderer>,
    renderer_scale: f32,
    clipboard: Option<arboard::Clipboard>,
    term_view: (f32, f32),
    editor_view: (f32, f32),
    wheel_accum: f32,
    /// Trailing repaint armed while zoom wheel events are throttled.
    zoom_render_armed: bool,
    last_zoom_render: std::time::Instant,
    term_mouse_down: bool,
    banner: Banner,
    pending_name: Option<NameAction>,
    fs_timer_armed: bool,
    resize_timer_armed: bool,
    /// Runtime toggle for the git Changes panel; the initial value comes from
    /// `show_changes` in the config.
    changes_enabled: bool,
    shutting_down: bool,
}

impl App {
    pub fn new(
        ui: &MainWindow,
        config: Config,
        recents: Vec<PathBuf>,
        team: Option<usize>,
        is_primary: bool,
    ) -> Self {
        let id = NEXT_APP_ID.fetch_add(1, Ordering::Relaxed);
        let theme = theme::by_id(&config.theme);
        let theme_index = Arc::new(AtomicU8::new(theme::index_of(&config.theme)));
        let font_family = intern_font(&config.font_family);
        let presets: Vec<crate::config::Preset> = config.presets_for(team).to_vec();
        let preset_items: Vec<PresetItem> = presets
            .iter()
            .map(|p| PresetItem { label: SharedString::from(p.label.as_str()) })
            .collect();
        ui.set_presets(ModelRc::new(VecModel::from(preset_items)));
        let team_names: Vec<SharedString> = config
            .teams
            .iter()
            .map(|t| SharedString::from(t.name.as_str()))
            .collect();
        ui.set_team_names(ModelRc::new(VecModel::from(team_names)));
        if let Some(team) = team.and_then(|i| config.teams.get(i)) {
            ui.set_window_title(SharedString::from(format!("Tigriden — {}", team.name)));
        }
        // Colors before the first frame, so the window never flashes the
        // built-in Slint defaults.
        push_theme(ui, &config);
        let changes_enabled = config.show_changes;
        ui.set_changes_enabled(changes_enabled);

        Self {
            id,
            is_primary,
            ui: ui.as_weak(),
            config,
            team,
            presets,
            theme,
            theme_index,
            font_family,
            sessions: Vec::new(),
            active: 0,
            recents,
            row_map: Vec::new(),
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            term_renderer: None,
            renderer_scale: 0.0,
            clipboard: arboard::Clipboard::new().ok(),
            term_view: (0.0, 0.0),
            editor_view: (0.0, 0.0),
            wheel_accum: 0.0,
            zoom_render_armed: false,
            last_zoom_render: std::time::Instant::now(),
            term_mouse_down: false,
            banner: Banner::None,
            pending_name: None,
            fs_timer_armed: false,
            resize_timer_armed: false,
            changes_enabled,
            shutting_down: false,
        }
    }

    // ----- settings -----

    /// Tigriden → Settings…
    pub fn open_settings(&mut self) {
        self.push_settings();
        if let Some(ui) = self.ui() {
            ui.set_settings_visible(true);
        }
    }

    pub fn close_settings(&mut self) {
        let Some(ui) = self.ui() else { return };
        ui.set_settings_visible(false);
        // The dialog took focus from the panes; hand it back to the terminal.
        ui.invoke_focus_terminal();
    }

    /// Monospaced families cosmic-text can actually resolve, plus whatever the
    /// config names (so a hand-written family still shows as selected).
    fn font_families(&self) -> Vec<SharedString> {
        let mut names: Vec<String> = self
            .font_system
            .db()
            .faces()
            .filter(|face| face.monospaced)
            .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
            .collect();
        names.push(self.config.font_family.clone());
        names.sort_by_key(|n| n.to_lowercase());
        names.dedup();
        names.into_iter().map(SharedString::from).collect()
    }

    /// Mirrors the config into the dialog's properties.
    fn push_settings(&mut self) {
        let Some(ui) = self.ui() else { return };
        let theme = theme::by_id(&self.config.theme);
        ui.set_settings_mode(SharedString::from(theme.mode()));
        ui.set_settings_style(SharedString::from(theme.style));
        ui.set_settings_theme_label(SharedString::from(theme.label));
        ui.set_settings_accent(SharedString::from(self.config.accent.as_str()));

        let accents: Vec<AccentOption> = theme::ACCENTS
            .iter()
            .map(|(id, label)| AccentOption {
                id: SharedString::from(*id),
                label: SharedString::from(*label),
                color: slint_color(theme::parse_hex(id).unwrap_or(theme.ui.accent)),
            })
            .collect();
        ui.set_settings_accent_options(ModelRc::new(VecModel::from(accents)));

        // A taste of the palette: accent plus six ANSI colors.
        let mut preview = vec![slint_color(self.config.accent_rgb())];
        preview.extend((1..=6).map(|i| slint_color(theme.term[i])));
        ui.set_settings_preview_colors(ModelRc::new(VecModel::from(preview)));

        ui.set_settings_font_families(ModelRc::new(VecModel::from(self.font_families())));
        ui.set_settings_font_family(SharedString::from(self.config.font_family.as_str()));
        ui.set_settings_font_size(self.config.font_size);
        ui.set_settings_ui_font_size(self.config.ui_font_size);
        ui.set_settings_scrollback(self.config.scrollback as i32);
        ui.set_settings_show_changes(self.config.show_changes);
        ui.set_settings_config_path(SharedString::from(
            config::config_path().map(|p| p.display().to_string()).unwrap_or_default(),
        ));
    }

    /// Adopts an edited config: chrome colors, terminal palette, editor theme
    /// and fonts all change in place, without restarting shells.
    pub fn apply_config(&mut self, config: Config) {
        let theme = theme::by_id(&config.theme);
        let font_family = intern_font(&config.font_family);
        let text_changed = !std::ptr::eq(font_family, self.font_family)
            || (config.font_size - self.config.font_size).abs() > f32::EPSILON;
        let theme_changed = !std::ptr::eq(theme, self.theme);
        let scrollback_changed = config.scrollback != self.config.scrollback;

        self.config = config;
        self.theme = theme;
        self.theme_index.store(theme::index_of(theme.id), Ordering::Relaxed);
        self.font_family = font_family;
        self.presets = self.config.presets_for(self.team).to_vec();

        if let Some(ui) = self.ui() {
            push_theme(&ui, &self.config);
        }
        self.push_settings();

        // Running shells adopt the new limit in place; already-trimmed lines
        // cannot come back, but history keeps growing from here.
        if scrollback_changed {
            for session in &self.sessions {
                for handle in &session.terms {
                    handle.term.set_scrollback(self.config.scrollback);
                }
            }
        }

        if !text_changed && !theme_changed {
            return;
        }
        if text_changed {
            // Cell metrics moved: rebuild on the next render, then re-grid.
            self.term_renderer = None;
        }

        let scale = self.scale();
        let font_px = self.config.font_size * scale;
        for idx in 0..self.sessions.len() {
            if let Some(editor) = self.sessions[idx].editor.as_mut() {
                editor.restyle(&mut self.font_system, font_family, font_px, theme);
            }
        }
        // Viewers bake colors and layout into their blocks, so rebuild them.
        let viewers: Vec<(usize, PathBuf, viewer::ViewKind)> = self
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.viewer.as_ref().map(|v| (i, v.path.clone(), v.kind)))
            .collect();
        for (idx, path, kind) in viewers {
            self.open_viewer(idx, path, kind);
        }

        self.apply_editor_size();
        self.apply_term_size();
        self.render_editor();
        self.render_term();
    }

    /// File → Show/Hide Changes Panel.
    pub fn toggle_changes(&mut self) {
        self.changes_enabled = !self.changes_enabled;
        if let Some(ui) = self.ui() {
            ui.set_changes_enabled(self.changes_enabled);
        }
        if self.changes_enabled {
            for idx in 0..self.sessions.len() {
                self.sessions[idx].tracking = crate::git::detect(&self.sessions[idx].root);
                // Shadow folders get a fresh baseline: "watch from now".
                self.refresh_changes(idx, true);
            }
        } else {
            for session in &mut self.sessions {
                session.tracking = None;
                session.changes.clear();
            }
        }
        self.rebuild_tree();
    }

    fn ui(&self) -> Option<MainWindow> {
        self.ui.upgrade()
    }

    fn scale(&self) -> f32 {
        self.ui().map(|ui| ui.window().scale_factor()).unwrap_or(1.0)
    }

    fn ensure_renderer(&mut self) -> (u32, u32) {
        let scale = self.scale();
        if self.term_renderer.is_none() || (self.renderer_scale - scale).abs() > 0.01 {
            let px = self.config.font_size * scale;
            self.term_renderer = Some(TermRenderer::new(self.font_family, px, &mut self.font_system));
            self.renderer_scale = scale;
        }
        let r = self.term_renderer.as_ref().unwrap();
        (r.cell_w, r.cell_h)
    }

    // ----- sessions -----

    /// Spawns one terminal (shell) in `root` with its own event routing id.
    fn spawn_term(&mut self, root: &Path) -> Option<TermHandle> {
        let (cell_w, cell_h) = self.ensure_renderer();
        let (cols, rows) = if self.term_view.0 > 0.0 {
            let scale = self.scale();
            self.term_renderer.as_ref().unwrap().grid_size(
                (self.term_view.0 * scale) as u32,
                (self.term_view.1 * scale) as u32,
            )
        } else {
            (80, 24)
        };

        let id = NEXT_TERM_ID.fetch_add(1, Ordering::Relaxed);
        let frame_pending = Arc::new(AtomicBool::new(false));
        let hooks = Self::make_hooks(self.id, id, frame_pending.clone());

        match TermSession::spawn(
            root,
            cols,
            rows,
            (cell_w as u16, cell_h as u16),
            self.config.scrollback,
            self.theme_index.clone(),
            hooks,
        ) {
            Ok(term) => Some(TermHandle { id, term, frame_pending }),
            Err(err) => {
                eprintln!("tigriden: {err}");
                None
            }
        }
    }

    pub fn add_session(&mut self, root: PathBuf, persist: bool) {
        let root = root.canonicalize().unwrap_or(root);
        if persist {
            self.touch_recent(&root);
        }
        if let Some(idx) = self.sessions.iter().position(|s| s.root == root) {
            self.set_active(idx);
            return;
        }

        let Some(first_term) = self.spawn_term(&root) else { return };

        let fs_root = root.clone();
        let app_id = self.id;
        let session = Session::new(root, first_term, move |paths| {
            let fs_root = fs_root.clone();
            let _ = slint::invoke_from_event_loop(move || {
                with_app_id(app_id, |app| app.fs_changed(&fs_root, paths));
            });
        });
        self.sessions.push(session);
        let new_idx = self.sessions.len() - 1;
        if self.changes_enabled {
            self.sessions[new_idx].tracking = crate::git::detect(&self.sessions[new_idx].root);
            self.refresh_changes(new_idx, true);
        }
        self.set_active(new_idx);
        if persist {
            self.persist();
        }

        // Test hooks: type $TIGRIDEN_TEST_INPUT into the first session's
        // terminal / open $TIGRIDEN_TEST_OPEN in its editor shortly after
        // launch (escape \r for Enter).
        #[cfg(feature = "framedump")]
        if self.sessions.len() == 1 {
            let app_id = self.id;
            if let Ok(cmds) = std::env::var("TIGRIDEN_TEST_INPUT") {
                // Stages separated by \\t are sent 3 s apart.
                for (i, stage) in cmds.split("\\t").enumerate() {
                    let bytes = stage.replace("\\r", "\r").into_bytes();
                    let delay = std::time::Duration::from_millis(1500 + i as u64 * 3000);
                    slint::Timer::single_shot(delay, move || {
                        with_app_id(app_id, |app| {
                            if let Some(handle) =
                                app.sessions.first().and_then(|s| s.terms.first())
                            {
                                handle.term.write(bytes.clone());
                            }
                        });
                    });
                }
            }
            if let Ok(mode) = std::env::var("TIGRIDEN_TEST_NEWTERM") {
                slint::Timer::single_shot(std::time::Duration::from_millis(2500), move || {
                    with_app_id(app_id, |app| app.new_terminal_active());
                });
                if mode == "back" {
                    slint::Timer::single_shot(std::time::Duration::from_millis(5000), move || {
                        with_app_id(app_id, |app| app.term_tab_clicked(0));
                    });
                }
            }
            if std::env::var("TIGRIDEN_TEST_SELECT").is_ok() {
                slint::Timer::single_shot(std::time::Duration::from_millis(3000), move || {
                    with_app_id(app_id, |app| {
                        app.term_mouse(0, 5.0, 5.0);
                        app.term_mouse(2, 300.0, 5.0);
                        app.term_mouse(1, 300.0, 5.0);
                        let copied = app
                            .sessions
                            .get(app.active)
                            .and_then(Session::active_term)
                            .and_then(|h| h.term.term.lock().selection_to_string());
                        eprintln!("TEST_SELECT: {copied:?}");
                    });
                });
            }
            if std::env::var("TIGRIDEN_TEST_PASTE").is_ok() {
                slint::Timer::single_shot(std::time::Duration::from_millis(3000), move || {
                    with_app_id(app_id, |app| {
                        let handled = app.term_key(
                            "v",
                            crate::term::keys::Mods { ctrl: false, alt: false, meta: true, shift: false },
                        );
                        eprintln!("TEST_PASTE handled={handled} clipboard_ok={}", app.clipboard.is_some());
                    });
                });
            }
            if let Ok(path) = std::env::var("TIGRIDEN_TEST_DROP") {
                slint::Timer::single_shot(std::time::Duration::from_millis(3000), move || {
                    with_app_id(app_id, |app| app.file_dropped(std::path::PathBuf::from(&path)));
                });
            }
            if std::env::var("TIGRIDEN_TEST_CTX").is_ok() {
                slint::Timer::single_shot(std::time::Duration::from_millis(3000), move || {
                    with_app_id(app_id, |app| {
                        app.tree_context(1, 0); // New Folder on session header
                        app.name_dialog_accept("ctx-folder".into());
                        app.tree_context(0, 0); // New File on session header
                        app.name_dialog_accept("ctx-file.txt".into());
                        eprintln!("TEST_CTX done, editor open: {:?}",
                            app.sessions.first().and_then(|s| s.editor.as_ref().map(|e| e.path.clone())));
                    });
                });
            }
            if let Ok(path) = std::env::var("TIGRIDEN_TEST_OPEN") {
                slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
                    with_app_id(app_id, |app| app.open_file(0, std::path::PathBuf::from(&path)));
                });
            }
            // Reports the Changes panel's tracking mode and contents before
            // and after a write into the folder.
            if std::env::var("TIGRIDEN_TEST_CHANGES").is_ok() {
                let probe = self.sessions[0].root.join("tigriden-probe.txt");
                let report = move |app: &mut App, when: &str| {
                    let session = &app.sessions[0];
                    eprintln!(
                        "TEST_CHANGES[{when}] enabled={} tracking={:?} changes={:?}",
                        app.changes_enabled, session.tracking, session.changes
                    );
                };
                slint::Timer::single_shot(std::time::Duration::from_millis(3000), move || {
                    with_app_id(app_id, |app| report(app, "baseline"));
                    let _ = std::fs::write(&probe, "probe\n");
                    slint::Timer::single_shot(std::time::Duration::from_millis(4000), move || {
                        with_app_id(app_id, |app| report(app, "after-write"));
                    });
                });
            }
            // Comma-separated "key=value" settings edits, applied like clicks
            // in the Settings dialog (e.g. "style=vivid,font-size=16").
            if let Ok(edits) = std::env::var("TIGRIDEN_TEST_SETTINGS") {
                slint::Timer::single_shot(std::time::Duration::from_millis(2500), move || {
                    for edit in edits.split(',') {
                        if let Some((key, value)) = edit.split_once('=') {
                            settings_changed(key, value);
                        }
                    }
                    eprintln!("TEST_SETTINGS applied: {:?}", config());
                });
            }
        }
    }

    fn make_hooks(app_id: u64, term_id: u64, pending: Arc<AtomicBool>) -> TermHooks {
        let repaint = Arc::new(move || {
            if !pending.swap(true, Ordering::AcqRel) {
                let _ = slint::invoke_from_event_loop(move || {
                    with_app_id(app_id, |app| app.term_repaint(term_id));
                });
            }
        });
        let exited = Arc::new(move || {
            let _ = slint::invoke_from_event_loop(move || {
                with_app_id(app_id, |app| app.term_exited(term_id));
            });
        });
        TermHooks { repaint, exited }
    }

    // ----- terminal tabs -----

    fn find_term(&self, id: u64) -> Option<(usize, usize)> {
        self.sessions.iter().enumerate().find_map(|(si, session)| {
            session.terms.iter().position(|t| t.id == id).map(|ti| (si, ti))
        })
    }

    pub fn new_terminal_active(&mut self) {
        self.new_terminal(self.active);
    }

    pub fn new_terminal(&mut self, session_idx: usize) {
        if session_idx >= self.sessions.len() {
            return;
        }
        let root = self.sessions[session_idx].root.clone();
        let Some(handle) = self.spawn_term(&root) else { return };
        let session = &mut self.sessions[session_idx];
        session.terms.push(handle);
        session.active_term = session.terms.len() - 1;
        self.active = session_idx;
        self.apply_term_size();
        self.refresh_all();
        if let Some(ui) = self.ui() {
            ui.invoke_focus_terminal();
        }
    }

    pub fn term_tab_clicked(&mut self, tab: usize) {
        let Some(session) = self.sessions.get_mut(self.active) else { return };
        if tab >= session.terms.len() || tab == session.active_term {
            return;
        }
        session.active_term = tab;
        self.apply_term_size();
        self.render_term();
        self.update_chrome();
        if let Some(ui) = self.ui() {
            ui.invoke_focus_terminal();
        }
    }

    pub fn close_terminal(&mut self, tab: usize) {
        let Some(session) = self.sessions.get_mut(self.active) else { return };
        // The last terminal of a session can only go by closing the session.
        if session.terms.len() <= 1 || tab >= session.terms.len() {
            return;
        }
        let mut handle = session.terms.remove(tab);
        handle.term.shutdown();
        if session.active_term >= session.terms.len() {
            session.active_term = session.terms.len() - 1;
        }
        self.apply_term_size();
        self.refresh_all();
    }

    // ----- recent folders -----

    /// Moves `root` to the front of the recent list (kept even after the
    /// folder is removed from the workbench).
    fn touch_recent(&mut self, root: &Path) {
        self.recents.retain(|p| p != root);
        self.recents.insert(0, root.to_path_buf());
        self.recents.truncate(10);
        self.update_recents_model();
    }

    pub fn update_recents_model(&mut self) {
        let Some(ui) = self.ui() else { return };
        let home = dirs::home_dir();
        let labels: Vec<SharedString> = self
            .recents
            .iter()
            .map(|p| {
                let shown = home
                    .as_ref()
                    .and_then(|h| p.strip_prefix(h).ok())
                    .map(|rel| format!("~/{}", rel.display()))
                    .unwrap_or_else(|| p.display().to_string());
                SharedString::from(shown)
            })
            .collect();
        ui.set_recent_folders(ModelRc::new(VecModel::from(labels)));
    }

    pub fn recent_clicked(&mut self, idx: usize) {
        let Some(path) = self.recents.get(idx).cloned() else { return };
        if !path.is_dir() {
            // Folder vanished since it was last used; drop it from the list.
            self.forget_recent(idx);
            return;
        }
        self.add_session(path, true);
    }

    pub fn forget_recent(&mut self, idx: usize) {
        if idx < self.recents.len() {
            self.recents.remove(idx);
            self.update_recents_model();
            self.persist();
        }
    }

    pub fn close_session(&mut self, idx: usize) {
        if idx >= self.sessions.len() {
            return;
        }
        if self.sessions[idx].editor.as_ref().is_some_and(|e| e.dirty) {
            self.show_banner(Banner::Unsaved(PendingAction::CloseSession(idx)));
            return;
        }
        self.really_close_session(idx);
    }

    fn really_close_session(&mut self, idx: usize) {
        let mut session = self.sessions.remove(idx);
        for handle in &mut session.terms {
            handle.term.shutdown();
        }
        if self.active >= self.sessions.len() {
            self.active = self.sessions.len().saturating_sub(1);
        }
        self.persist();
        self.refresh_all();
    }

    pub fn set_active(&mut self, idx: usize) {
        if idx >= self.sessions.len() {
            return;
        }
        self.active = idx;
        self.apply_term_size();
        self.apply_editor_size();
        self.persist();
        self.refresh_all();
    }

    // ----- tree -----

    pub fn row_clicked(&mut self, id: i32) {
        match self.row_map.get(id as usize).cloned() {
            Some(RowTarget::File(idx, path)) => self.open_file(idx, path),
            Some(RowTarget::Header(idx)) => self.set_active(idx),
            Some(RowTarget::Dir(idx, path)) => {
                self.sessions[idx].tree.toggle(&path);
                self.rebuild_tree();
            }
            Some(RowTarget::Change(idx, path, _)) => self.open_diff_view(idx, path),
            Some(RowTarget::ChangesHeader(idx)) => self.toggle_changes_section(idx),
            None => {}
        }
    }

    pub fn row_toggled(&mut self, id: i32) {
        match self.row_map.get(id as usize).cloned() {
            Some(RowTarget::Header(idx)) => {
                self.sessions[idx].tree_visible = !self.sessions[idx].tree_visible;
                self.active = idx;
                self.rebuild_tree();
            }
            Some(RowTarget::Dir(idx, path)) => {
                self.sessions[idx].tree.toggle(&path);
                self.rebuild_tree();
            }
            Some(RowTarget::File(idx, path)) => self.open_file(idx, path),
            Some(RowTarget::Change(idx, path, _)) => self.open_diff_view(idx, path),
            Some(RowTarget::ChangesHeader(idx)) => self.toggle_changes_section(idx),
            None => {}
        }
    }

    fn toggle_changes_section(&mut self, idx: usize) {
        if let Some(session) = self.sessions.get_mut(idx) {
            session.changes_visible = !session.changes_visible;
        }
        self.rebuild_tree();
    }

    fn rebuild_tree(&mut self) {
        let Some(ui) = self.ui() else { return };
        let changes_enabled = self.changes_enabled;
        let mut rows: Vec<TreeRow> = Vec::new();
        self.row_map.clear();
        for (i, session) in self.sessions.iter_mut().enumerate() {
            rows.push(TreeRow {
                kind: 0,
                indent: 0,
                name: SharedString::from(session.name.as_str()),
                expanded: session.tree_visible,
                session: i as i32,
                row_id: self.row_map.len() as i32,
                active: i == self.active,
            });
            self.row_map.push(RowTarget::Header(i));
            if !session.tree_visible {
                continue;
            }
            if changes_enabled && session.tracking.is_some() {
                rows.push(TreeRow {
                    kind: 3,
                    indent: 1,
                    name: SharedString::from(format!("Changes ({})", session.changes.len())),
                    expanded: session.changes_visible,
                    session: i as i32,
                    row_id: self.row_map.len() as i32,
                    active: false,
                });
                self.row_map.push(RowTarget::ChangesHeader(i));
                if session.changes_visible {
                    for change in &session.changes {
                        rows.push(TreeRow {
                            kind: 4,
                            indent: 2,
                            name: SharedString::from(format!("{}  {}", change.status, change.rel)),
                            expanded: false,
                            session: i as i32,
                            row_id: self.row_map.len() as i32,
                            active: false,
                        });
                        self.row_map.push(RowTarget::Change(
                            i,
                            change.abs(&session.root),
                            change.status,
                        ));
                    }
                }
            }
            for flat in session.tree.flatten() {
                rows.push(TreeRow {
                    kind: flat.kind,
                    indent: flat.indent,
                    name: SharedString::from(flat.name.as_str()),
                    expanded: flat.expanded,
                    session: i as i32,
                    row_id: self.row_map.len() as i32,
                    active: false,
                });
                self.row_map.push(if flat.kind == 1 {
                    RowTarget::Dir(i, flat.path)
                } else {
                    RowTarget::File(i, flat.path)
                });
            }
        }
        ui.set_tree_rows(ModelRc::new(VecModel::from(rows)));
    }

    // ----- file watching -----

    fn fs_changed(&mut self, root: &Path, paths: Vec<PathBuf>) {
        let Some(session) = self.sessions.iter_mut().find(|s| s.root == root) else { return };
        for path in &paths {
            if let Some(parent) = path.parent() {
                if !session.pending_fs.contains(&PathBuf::from(parent)) {
                    session.pending_fs.push(parent.to_path_buf());
                }
            }
            // Directory events (create/delete inside) also arrive as the dir
            // itself.
            if !session.pending_fs.contains(path) {
                session.pending_fs.push(path.clone());
            }
        }
        if !self.fs_timer_armed {
            self.fs_timer_armed = true;
            let app_id = self.id;
            slint::Timer::single_shot(std::time::Duration::from_millis(250), move || {
                with_app_id(app_id, |app| app.drain_fs());
            });
        }
    }

    fn drain_fs(&mut self) {
        self.fs_timer_armed = false;
        let mut needs_tree = false;
        let mut reload_check: Vec<usize> = Vec::new();
        let mut git_refresh: Vec<usize> = Vec::new();
        for (i, session) in self.sessions.iter_mut().enumerate() {
            if session.pending_fs.is_empty() {
                continue;
            }
            if session.tracking.is_some() {
                git_refresh.push(i);
            }
            let dirs = std::mem::take(&mut session.pending_fs);
            if let Some(editor) = &session.editor {
                if dirs.iter().any(|p| *p == editor.path) {
                    reload_check.push(i);
                }
            }
            if let Some(viewer_state) = &session.viewer {
                if dirs.iter().any(|p| *p == viewer_state.path) {
                    reload_check.push(i);
                }
            }
            session.tree.invalidate(dirs);
            needs_tree = true;
        }
        if needs_tree {
            self.rebuild_tree();
        }
        for idx in reload_check {
            self.maybe_reload_editor(idx);
        }
        for idx in git_refresh {
            self.refresh_changes(idx, false);
        }
    }

    // ----- git changes panel -----

    /// Re-runs `git status` for one session on a background thread. Results
    /// arrive in `changes_ready`; a generation counter drops stale ones.
    /// `rebaseline` re-snapshots shadow-tracked folders ("watch from now").
    fn refresh_changes(&mut self, idx: usize, rebaseline: bool) {
        if !self.changes_enabled {
            return;
        }
        let Some(session) = self.sessions.get_mut(idx) else { return };
        let Some(tracking) = session.tracking.clone() else { return };
        session.changes_gen += 1;
        let generation = session.changes_gen;
        let root = session.root.clone();
        let app_id = self.id;
        std::thread::spawn(move || {
            if let crate::git::Tracking::Shadow(dir) = &tracking {
                if rebaseline || !dir.join("HEAD").exists() {
                    crate::git::snapshot_baseline(&root, dir);
                }
            }
            let changes = crate::git::status(&root, &tracking);
            let _ = slint::invoke_from_event_loop(move || {
                with_app_id(app_id, |app| app.changes_ready(&root, generation, changes));
            });
        });
    }

    fn changes_ready(&mut self, root: &Path, generation: u64, changes: Vec<crate::git::Change>) {
        // Look up by root — session indices may have shifted meanwhile.
        let Some(idx) = self.sessions.iter().position(|s| s.root == root) else { return };
        let session = &mut self.sessions[idx];
        if session.tracking.is_none() || session.changes_gen != generation {
            return; // panel toggled off, or a newer refresh is in flight
        }
        if session.changes == changes {
            return;
        }
        session.changes = changes;
        self.rebuild_tree();
    }

    fn maybe_reload_editor(&mut self, idx: usize) {
        let is_active = idx == self.active;
        let font_family = self.font_family;
        if let Some(viewer_state) = &self.sessions[idx].viewer {
            let (path, kind) = (viewer_state.path.clone(), viewer_state.kind);
            self.open_viewer(idx, path, kind);
            return;
        }
        // A read-only diff has no disk_mtime; re-run the diff instead of
        // letting the mtime check replace it with raw file text.
        if self.sessions[idx].editor.as_ref().is_some_and(|e| e.read_only) {
            let path = self.sessions[idx].editor.as_ref().unwrap().path.clone();
            self.open_diff_view(idx, path);
            return;
        }
        let Some(editor) = self.sessions[idx].editor.as_mut() else { return };
        let mtime = std::fs::metadata(&editor.path).and_then(|m| m.modified()).ok();
        if mtime == editor.disk_mtime {
            return; // our own save, or spurious event
        }
        if editor.dirty {
            if is_active {
                self.show_banner(Banner::DiskChanged);
            }
            return;
        }
        editor.reload(&mut self.font_system, font_family);
        if is_active {
            self.apply_editor_size();
            self.render_editor();
            self.update_chrome();
        }
    }

    // ----- editor -----

    fn open_file(&mut self, idx: usize, path: PathBuf) {
        if self.sessions[idx].editor.as_ref().is_some_and(|e| e.dirty && e.path != path) {
            self.active = idx;
            self.show_banner(Banner::Unsaved(PendingAction::OpenFile(idx, path)));
            return;
        }
        self.really_open_file(idx, path);
    }

    fn really_open_file(&mut self, idx: usize, path: PathBuf) {
        self.active = idx;
        if let Some(kind) = viewer::classify(&path) {
            self.open_viewer(idx, path, kind);
            return;
        }
        self.open_text_editor(idx, path);
    }

    fn open_viewer(&mut self, idx: usize, path: PathBuf, kind: viewer::ViewKind) {
        let scale = self.scale();
        let width_px = (self.editor_view.0 * scale).max(200.0);
        // Repaint when the PDF worker finishes rasterizing a page.
        let app_id = self.id;
        let notify: viewer::Notify = std::sync::Arc::new(move || {
            let _ = slint::invoke_from_event_loop(move || {
                with_app_id(app_id, |app| app.render_editor());
            });
        });
        match ViewerState::open(
            &mut self.font_system,
            &path,
            kind,
            self.font_family,
            self.config.font_size * scale,
            self.theme,
            self.config.accent_rgb(),
            width_px,
            notify,
        ) {
            Ok(viewer_state) => {
                let session = &mut self.sessions[idx];
                session.viewer = Some(viewer_state);
                session.editor = None;
                self.render_editor();
                self.update_chrome();
            }
            Err(err) => {
                eprintln!("tigriden: {err}");
                if let Some(ui) = self.ui() {
                    ui.set_editor_title(SharedString::from(err));
                }
            }
        }
    }

    /// Reopens the current viewer's file in the text editor, or the current
    /// text file in its rendered view.
    pub fn toggle_view(&mut self) {
        let Some(session) = self.sessions.get_mut(self.active) else { return };
        // Diff view's "File" chip: jump to the editable file (one-way; the
        // diff is reopened from the Changes panel).
        if session.editor.as_ref().is_some_and(|e| e.read_only) {
            let path = session.editor.as_ref().unwrap().path.clone();
            session.editor = None;
            self.really_open_file(self.active, path);
            return;
        }
        if let Some(viewer_state) = &session.viewer {
            let path = viewer_state.path.clone();
            session.viewer = None;
            self.open_text_editor(self.active, path);
        } else if let Some(editor) = &session.editor {
            let path = editor.path.clone();
            if let Some(kind) = viewer::classify(&path) {
                if editor.dirty {
                    self.show_banner(Banner::Unsaved(PendingAction::OpenFile(self.active, path)));
                    return;
                }
                session.editor = None;
                self.open_viewer(self.active, path, kind);
            }
        }
    }

    fn open_text_editor(&mut self, idx: usize, path: PathBuf) {
        self.active = idx;
        self.sessions[idx].viewer = None;
        // Binary sniff: NUL byte in the first 8 KiB.
        let is_binary = std::fs::File::open(&path)
            .and_then(|mut f| {
                use std::io::Read;
                let mut buf = [0u8; 8192];
                let n = f.read(&mut buf)?;
                Ok(buf[..n].contains(&0))
            })
            .unwrap_or(true);
        if is_binary {
            self.sessions[idx].editor = None;
            if let Some(ui) = self.ui() {
                let name = self.sessions[idx].relative_name(&path);
                ui.set_editor_title(SharedString::from(format!("{name} (binary — not opened)")));
                ui.set_editor_dirty(false);
                ui.set_editor_frame(Image::default());
            }
            return;
        }

        let scale = self.scale();
        match EditorState::open(
            &mut self.font_system,
            syntax_system(),
            &path,
            self.font_family,
            self.config.font_size * scale,
            self.theme,
        ) {
            Ok(editor) => {
                self.sessions[idx].editor = Some(editor);
                self.apply_editor_size();
                self.render_editor();
                self.update_chrome();
            }
            Err(err) => eprintln!("tigriden: {err}"),
        }
    }

    /// Opens a read-only diff of `path` vs HEAD in the editor pane.
    fn open_diff_view(&mut self, idx: usize, path: PathBuf) {
        if self.sessions[idx].editor.as_ref().is_some_and(|e| e.dirty && !e.read_only) {
            self.active = idx;
            self.show_banner(Banner::Unsaved(PendingAction::OpenDiff(idx, path)));
            return;
        }
        self.active = idx;
        let session = &mut self.sessions[idx];
        let Some(tracking) = session.tracking.clone() else { return };
        session.diff_gen += 1;
        let generation = session.diff_gen;
        let root = session.root.clone();
        let app_id = self.id;
        std::thread::spawn(move || {
            let text = crate::git::diff_file(&root, &tracking, &path);
            let _ = slint::invoke_from_event_loop(move || {
                with_app_id(app_id, |app| app.diff_ready(&root, generation, path, text));
            });
        });
    }

    fn diff_ready(&mut self, root: &Path, generation: u64, path: PathBuf, text: String) {
        let Some(idx) = self.sessions.iter().position(|s| s.root == root) else { return };
        if self.sessions[idx].diff_gen != generation {
            return;
        }
        let scale = self.scale();
        match EditorState::open_diff(
            &mut self.font_system,
            syntax_system(),
            &path,
            &text,
            self.font_family,
            self.config.font_size * scale,
            self.theme,
        ) {
            Ok(editor) => {
                self.sessions[idx].viewer = None;
                self.sessions[idx].editor = Some(editor);
                self.apply_editor_size();
                self.render_editor();
                self.update_chrome();
            }
            Err(err) => eprintln!("tigriden: {err}"),
        }
    }

    pub fn editor_key(&mut self, text: &str, mods: Mods) -> bool {
        if self.viewer_zoom_key(text, &mods) {
            return true;
        }
        if self.viewer_scroll_key(text, &mods) {
            return true;
        }
        let Some(session) = self.sessions.get_mut(self.active) else { return false };
        let Some(editor) = session.editor.as_mut() else { return false };
        match editor.handle_key(&mut self.font_system, &mut self.clipboard, text, &mods) {
            KeyOutcome::Save => {
                self.save_editor();
                true
            }
            KeyOutcome::Consumed => {
                self.render_editor();
                self.update_chrome();
                true
            }
            KeyOutcome::Ignored => false,
        }
    }

    /// Cmd+= / Cmd+- / Cmd+0 zoom the viewer (images and PDF pages).
    fn viewer_zoom_key(&mut self, text: &str, mods: &Mods) -> bool {
        if !mods.meta || mods.ctrl || mods.alt {
            return false;
        }
        let Some(session) = self.sessions.get_mut(self.active) else { return false };
        let Some(viewer_state) = session.viewer.as_mut() else { return false };
        let handled = match text.chars().next() {
            Some('=') | Some('+') => {
                viewer_state.zoom_by(&mut self.font_system, 1.25);
                true
            }
            Some('-') => {
                viewer_state.zoom_by(&mut self.font_system, 0.8);
                true
            }
            Some('0') => {
                viewer_state.zoom_reset(&mut self.font_system);
                true
            }
            _ => false,
        };
        if handled {
            self.render_editor();
        }
        handled
    }

    /// PageUp/PageDown, Home/End and arrow keys scroll the viewer.
    fn viewer_scroll_key(&mut self, text: &str, mods: &Mods) -> bool {
        if mods.ctrl || mods.alt || mods.meta || mods.shift {
            return false;
        }
        let scale = self.scale();
        let view_h = self.editor_view.1 * scale;
        let line = self.config.font_size * scale * 3.0;
        let Some(session) = self.sessions.get_mut(self.active) else { return false };
        let Some(viewer_state) = session.viewer.as_mut() else { return false };
        match text.chars().next() {
            Some(keys::K_PAGE_UP) => viewer_state.scroll_page(view_h, false),
            Some(keys::K_PAGE_DOWN) => viewer_state.scroll_page(view_h, true),
            Some(keys::K_HOME) => viewer_state.scroll_home(false),
            Some(keys::K_END) => viewer_state.scroll_home(true),
            Some(keys::K_UP) => viewer_state.scroll_by(0.0, line),
            Some(keys::K_DOWN) => viewer_state.scroll_by(0.0, -line),
            _ => return false,
        }
        self.render_editor();
        true
    }

    /// Header magnifier buttons: same steps as Cmd+= / Cmd+-.
    pub fn viewer_zoom_in(&mut self) {
        self.viewer_zoom_step(1.25);
    }

    pub fn viewer_zoom_out(&mut self) {
        self.viewer_zoom_step(0.8);
    }

    fn viewer_zoom_step(&mut self, factor: f32) {
        let Some(session) = self.sessions.get_mut(self.active) else { return };
        let Some(viewer_state) = session.viewer.as_mut() else { return };
        viewer_state.zoom_by(&mut self.font_system, factor);
        self.render_editor();
    }

    pub fn editor_mouse(&mut self, kind: i32, x: f32, y: f32) {
        let scale = self.scale();
        let view_h = self.editor_view.1 * scale;
        let Some(session) = self.sessions.get_mut(self.active) else { return };
        if let Some(viewer_state) = session.viewer.as_mut() {
            if viewer_state.handle_mouse(kind, x * scale, y * scale, view_h) {
                self.render_editor();
            }
            return;
        }
        let Some(editor) = session.editor.as_mut() else { return };
        editor.handle_mouse(&mut self.font_system, kind, (x * scale) as i32, (y * scale) as i32);
        if kind != 1 {
            self.render_editor();
        }
    }

    /// `zoom` is set while Ctrl/Cmd is held: the wheel then zooms the viewer
    /// (images and PDF pages) instead of scrolling it. Slint's own modifier
    /// bookkeeping misses ⌘ during scroll on macOS, so ask the OS directly.
    pub fn editor_wheel(&mut self, delta_x: f32, delta_y: f32, zoom: bool) {
        let zoom = zoom || zoom_modifier_down();
        // TIGRIDEN_DEBUG_WHEEL=1: trace wheel events to diagnose zoom issues.
        if std::env::var_os("TIGRIDEN_DEBUG_WHEEL").is_some() {
            let viewer = self
                .sessions
                .get(self.active)
                .and_then(|s| s.viewer.as_ref())
                .map(|v| v.zoomable())
                .map_or("none".to_string(), |z| format!("zoomable={z}"));
            #[cfg(target_os = "macos")]
            let raw = ns_modifier_flags();
            #[cfg(not(target_os = "macos"))]
            let raw = 0usize;
            eprintln!(
                "wheel dx={delta_x:.1} dy={delta_y:.1} zoom-mod={zoom} native-mod={} raw=0x{raw:x} viewer:{viewer}",
                zoom_modifier_down()
            );
        }
        let scale = self.scale();
        let Some(session) = self.sessions.get_mut(self.active) else { return };
        if let Some(viewer_state) = session.viewer.as_mut() {
            if zoom {
                let factor = (1.0 + delta_y * 0.002 * scale).clamp(0.5, 2.0);
                viewer_state.zoom_by(&mut self.font_system, factor);
                self.throttled_viewer_render(std::time::Duration::from_millis(33));
            } else {
                viewer_state.scroll_by(delta_x * scale, delta_y * scale);
                self.throttled_viewer_render(std::time::Duration::from_millis(15));
            }
            return;
        }
        let Some(editor) = session.editor.as_mut() else { return };
        editor.scroll(&mut self.font_system, delta_y * scale);
        self.render_editor();
    }

    /// Wheel events arrive at up to 120/s; painting every one outruns the
    /// frame budget and backlogs the event queue, which reads as lag (and
    /// delays modifier flag changes queued behind the wheel events). Paint at
    /// most once per `frame`, with a trailing shot for the final state.
    fn throttled_viewer_render(&mut self, frame: std::time::Duration) {
        if self.last_zoom_render.elapsed() >= frame {
            self.last_zoom_render = std::time::Instant::now();
            self.render_editor();
        } else if !self.zoom_render_armed {
            self.zoom_render_armed = true;
            let app_id = self.id;
            slint::Timer::single_shot(frame, move || {
                with_app_id(app_id, |app| {
                    app.zoom_render_armed = false;
                    app.last_zoom_render = std::time::Instant::now();
                    app.render_editor();
                });
            });
        }
    }

    pub fn editor_resized(&mut self, w: f32, h: f32) {
        self.editor_view = (w, h);
        self.apply_editor_size();
        self.render_editor();
    }

    fn apply_editor_size(&mut self) {
        let scale = self.scale();
        let (w, h) = self.editor_view;
        let Some(session) = self.sessions.get_mut(self.active) else { return };
        if let Some(editor) = session.editor.as_mut() {
            editor.set_viewport(&mut self.font_system, w * scale, h * scale);
        }
        if let Some(viewer_state) = session.viewer.as_mut() {
            viewer_state.set_viewport(&mut self.font_system, w * scale, h * scale);
        }
    }

    pub fn save_editor(&mut self) {
        let Some(session) = self.sessions.get_mut(self.active) else { return };
        let Some(editor) = session.editor.as_mut() else { return };
        if let Err(err) = editor.save() {
            eprintln!("tigriden: save failed: {err}");
        }
        self.render_editor();
        self.update_chrome();
    }

    fn render_editor(&mut self) {
        let Some(ui) = self.ui() else { return };
        let scale = self.scale();
        let (w, h) = ((self.editor_view.0 * scale) as u32, (self.editor_view.1 * scale) as u32);
        let Some(session) = self.sessions.get_mut(self.active) else { return };
        if w == 0 || h == 0 {
            return;
        }
        if let Some(viewer_state) = session.viewer.as_mut() {
            let buffer = viewer_state.render(&mut self.font_system, &mut self.swash_cache, w, h);
            #[cfg(feature = "framedump")]
            dump_frame("editor", &buffer);
            ui.set_editor_frame(Image::from_rgba8_premultiplied(buffer));
            return;
        }
        let Some(editor) = session.editor.as_mut() else {
            ui.set_editor_frame(Image::default());
            return;
        };
        let buffer = editor.render(&mut self.font_system, &mut self.swash_cache, w, h);
        #[cfg(feature = "framedump")]
        dump_frame("editor", &buffer);
        ui.set_editor_frame(Image::from_rgba8_premultiplied(buffer));
    }

    // ----- terminal -----

    pub fn term_repaint(&mut self, id: u64) {
        let Some((si, ti)) = self.find_term(id) else { return };
        self.sessions[si].terms[ti].frame_pending.store(false, Ordering::Release);
        if si == self.active && ti == self.sessions[si].active_term {
            self.render_term();
        }
    }

    pub fn term_exited(&mut self, id: u64) {
        let is_visible = self
            .find_term(id)
            .is_some_and(|(si, ti)| si == self.active && ti == self.sessions[si].active_term);
        if is_visible {
            self.update_chrome();
        }
    }

    pub fn term_key(&mut self, text: &str, mods: Mods) -> bool {
        if mods.meta {
            return self.term_shortcut(text);
        }
        if self.term_scroll_key(text, &mods) {
            return true;
        }
        let Some(handle) = self.sessions.get_mut(self.active).and_then(Session::active_term_mut)
        else {
            return false;
        };
        let (mode, scrolled) = {
            let term = handle.term.term.lock();
            (*term.mode(), term.grid().display_offset() != 0)
        };
        match keys::encode(text, &mods, mode) {
            Some(bytes) => {
                {
                    let mut term = handle.term.term.lock();
                    if term.selection.is_some() {
                        term.selection = None;
                    }
                    if scrolled {
                        term.scroll_display(Scroll::Bottom);
                    }
                }
                handle.term.write(bytes);
                true
            }
            None => false,
        }
    }

    fn term_shortcut(&mut self, text: &str) -> bool {
        match text.chars().next().map(|c| c.to_ascii_lowercase()) {
            Some('c') => {
                let selected = self
                    .sessions
                    .get(self.active)
                    .and_then(Session::active_term)
                    .and_then(|h| h.term.term.lock().selection_to_string());
                if let (Some(text), Some(cb)) = (selected, self.clipboard.as_mut()) {
                    if !text.is_empty() {
                        let _ = cb.set_text(text);
                    }
                }
                true
            }
            Some('v') => {
                let pasted = self.clipboard.as_mut().and_then(|cb| cb.get_text().ok());
                let handle = self.sessions.get_mut(self.active).and_then(Session::active_term_mut);
                if let (Some(text), Some(handle)) = (pasted, handle) {
                    let mode = *handle.term.term.lock().mode();
                    handle.term.write(keys::encode_paste(&text, mode));
                }
                true
            }
            _ => false,
        }
    }

    /// kind: 0 = down, 1 = up, 2 = move, 3 = double-click (logical px).
    pub fn term_mouse(&mut self, kind: i32, x: f32, y: f32) {
        let (cell_w, cell_h) = self.ensure_renderer();
        let scale = self.scale();
        let px = (x * scale).max(0.0) as u32;
        let py = (y * scale).max(0.0) as u32;
        let side = if px % cell_w.max(1) < cell_w / 2 { Side::Left } else { Side::Right };
        let mouse_down = &mut self.term_mouse_down;
        let Some(handle) = self.sessions.get_mut(self.active).and_then(Session::active_term_mut)
        else {
            return;
        };
        let col = ((px / cell_w.max(1)) as usize).min(handle.term.cols.saturating_sub(1) as usize);
        let row = ((py / cell_h.max(1)) as usize).min(handle.term.rows.saturating_sub(1) as usize);
        {
            let mut term = handle.term.term.lock();
            let point = viewport_to_point(term.grid().display_offset(), Point::new(row, Column(col)));
            match kind {
                0 => {
                    *mouse_down = true;
                    term.selection = Some(Selection::new(SelectionType::Simple, point, side));
                }
                2 if *mouse_down => {
                    if let Some(selection) = term.selection.as_mut() {
                        selection.update(point, side);
                    }
                }
                1 => {
                    *mouse_down = false;
                    let empty = match &term.selection {
                        Some(selection) => selection.to_range(&term).is_none(),
                        None => true,
                    };
                    if empty {
                        term.selection = None;
                    }
                }
                3 => {
                    term.selection = Some(Selection::new(SelectionType::Semantic, point, side));
                }
                _ => return,
            }
        }
        self.render_term();
    }

    /// Scrollback navigation, following the usual terminal convention:
    /// Shift+PageUp/PageDown page through history and Shift+Home/End jump to
    /// its ends, while the unshifted keys still reach the shell. Returns true
    /// when the key was consumed as a scroll.
    ///
    /// Full-screen apps (vim, less, the agent TUIs) manage their own
    /// scrollback, so on the alternate screen every key goes through.
    fn term_scroll_key(&mut self, text: &str, mods: &Mods) -> bool {
        if !mods.shift || mods.ctrl || mods.alt {
            return false;
        }
        let Some(key) = text.chars().next() else { return false };
        let scroll = match key {
            keys::K_PAGE_UP => Scroll::PageUp,
            keys::K_PAGE_DOWN => Scroll::PageDown,
            keys::K_HOME => Scroll::Top,
            keys::K_END => Scroll::Bottom,
            // Shift+arrows scroll a line at a time.
            keys::K_UP => Scroll::Delta(1),
            keys::K_DOWN => Scroll::Delta(-1),
            _ => return false,
        };
        let Some(handle) = self.sessions.get(self.active).and_then(Session::active_term)
        else {
            return false;
        };
        {
            let mut term = handle.term.term.lock();
            if term.mode().contains(TermMode::ALT_SCREEN) {
                return false;
            }
            term.scroll_display(scroll);
        }
        self.render_term();
        true
    }

    pub fn term_wheel(&mut self, delta: f32) {
        let Some(renderer) = self.term_renderer.as_ref() else { return };
        let cell_h_logical = renderer.cell_h as f32 / self.scale().max(0.01);
        self.wheel_accum += delta / cell_h_logical.max(1.0);
        let lines = self.wheel_accum as i32;
        if lines != 0 {
            self.wheel_accum -= lines as f32;
            if let Some(handle) = self.sessions.get_mut(self.active).and_then(Session::active_term_mut)
            {
                handle.term.term.lock().scroll_display(Scroll::Delta(lines));
                self.render_term();
            }
        }
    }

    pub fn term_resized(&mut self, w: f32, h: f32) {
        self.term_view = (w, h);
        if !self.resize_timer_armed {
            self.resize_timer_armed = true;
            let app_id = self.id;
            slint::Timer::single_shot(std::time::Duration::from_millis(50), move || {
                with_app_id(app_id, |app| {
                    app.resize_timer_armed = false;
                    app.apply_term_size();
                    app.render_term();
                });
            });
        }
    }

    fn apply_term_size(&mut self) {
        let (cell_w, cell_h) = self.ensure_renderer();
        let scale = self.scale();
        let (w_px, h_px) = ((self.term_view.0 * scale) as u32, (self.term_view.1 * scale) as u32);
        if w_px == 0 || h_px == 0 {
            return;
        }
        let (cols, rows) = self.term_renderer.as_ref().unwrap().grid_size(w_px, h_px);
        if let Some(handle) = self.sessions.get_mut(self.active).and_then(Session::active_term_mut) {
            handle.term.resize(cols, rows, (cell_w as u16, cell_h as u16));
        }
    }

    fn render_term(&mut self) {
        let Some(ui) = self.ui() else { return };
        let scale = self.scale();
        let (w_px, h_px) = ((self.term_view.0 * scale) as u32, (self.term_view.1 * scale) as u32);
        if w_px == 0 || h_px == 0 || self.sessions.is_empty() {
            return;
        }
        self.ensure_renderer();
        let focused = ui.get_term_focused();
        let Some(handle) = self.sessions[self.active].active_term() else { return };
        let term = handle.term.term.lock();
        let renderer = self.term_renderer.as_mut().unwrap();
        let buffer = renderer.render(
            &mut self.font_system,
            &mut self.swash_cache,
            &term,
            self.theme,
            focused,
            w_px,
            h_px,
        );
        drop(term);
        #[cfg(feature = "framedump")]
        dump_frame("term", &buffer);
        ui.set_term_frame(Image::from_rgba8_premultiplied(buffer));
    }

    pub fn preset_clicked(&mut self, idx: usize) {
        let Some(preset) = self.presets.get(idx) else { return };
        let mut bytes = preset.command.clone().into_bytes();
        if preset.send_enter {
            bytes.push(b'\r');
        }
        if let Some(handle) = self.sessions.get_mut(self.active).and_then(Session::active_term_mut) {
            handle.term.write(bytes);
        }
        if let Some(ui) = self.ui() {
            ui.invoke_focus_terminal();
        }
    }

    // ----- file drop -----

    /// Quotes a path for the shell unless it is entirely safe characters.
    fn shell_escape(text: &str) -> String {
        let safe = |c: char| c.is_ascii_alphanumeric() || "_-./~+=:@%,".contains(c);
        if !text.is_empty() && text.chars().all(safe) {
            text.to_string()
        } else {
            format!("'{}'", text.replace('\'', "'\\''"))
        }
    }

    pub fn file_drop_hover(&mut self, hovering: bool) {
        if hovering && !self.sessions.is_empty() {
            if let Some(ui) = self.ui() {
                ui.set_term_overlay(SharedString::from("release to insert the file path"));
            }
        } else {
            self.update_chrome();
        }
    }

    /// Types the dropped file's (escaped) path into the active terminal, the
    /// way native terminals do — agents receive it as an attached-file path.
    pub fn file_dropped(&mut self, path: PathBuf) {
        let escaped = Self::shell_escape(&path.display().to_string());
        let Some(handle) = self.sessions.get_mut(self.active).and_then(Session::active_term_mut)
        else {
            return;
        };
        let mode = *handle.term.term.lock().mode();
        handle.term.write(keys::encode_paste(&format!("{escaped} "), mode));
        if let Some(ui) = self.ui() {
            ui.invoke_focus_terminal();
        }
        self.update_chrome();
    }

    // ----- tree context menu -----

    /// Resolves a row to (session index, path, is_dir).
    fn row_target(&self, row_id: i32) -> Option<(usize, PathBuf, bool)> {
        match self.row_map.get(row_id as usize)? {
            RowTarget::Header(i) => Some((*i, self.sessions.get(*i)?.root.clone(), true)),
            RowTarget::Dir(i, p) => Some((*i, p.clone(), true)),
            RowTarget::File(i, p) => Some((*i, p.clone(), false)),
            RowTarget::ChangesHeader(i) => Some((*i, self.sessions.get(*i)?.root.clone(), true)),
            RowTarget::Change(i, p, _) => Some((*i, p.clone(), false)),
        }
    }

    /// Picks a non-clashing sibling name by appending " 2", " 3", ….
    fn unique_path(path: &Path) -> PathBuf {
        if !path.exists() {
            return path.to_path_buf();
        }
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        let parent = path.parent().unwrap_or(Path::new("."));
        (2..)
            .map(|n| parent.join(format!("{stem} {n}{ext}")))
            .find(|p| !p.exists())
            .unwrap()
    }

    fn copy_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
        if from.is_dir() {
            std::fs::create_dir_all(to)?;
            for entry in std::fs::read_dir(from)? {
                let entry = entry?;
                Self::copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
            }
        } else {
            std::fs::copy(from, to)?;
        }
        Ok(())
    }

    fn open_name_dialog(&mut self, title: String, initial: &str, action: NameAction) {
        self.pending_name = Some(action);
        if let Some(ui) = self.ui() {
            ui.set_name_dialog_title(SharedString::from(title));
            ui.set_name_dialog_value(SharedString::from(initial));
            ui.set_name_dialog_visible(true);
        }
    }

    fn close_name_dialog(&mut self) {
        self.pending_name = None;
        if let Some(ui) = self.ui() {
            ui.set_name_dialog_visible(false);
        }
    }

    /// Refreshes a directory listing right away instead of waiting for the
    /// watcher debounce.
    fn refresh_dir(&mut self, session_idx: usize, dir: &Path) {
        if let Some(session) = self.sessions.get_mut(session_idx) {
            session.tree.invalidate([dir.to_path_buf()]);
        }
        self.rebuild_tree();
    }

    pub fn tree_context(&mut self, action: i32, row_id: i32) {
        // Discard Changes: only meaningful on a Changes-panel row.
        if action == 9 {
            if let Some(RowTarget::Change(idx, path, status)) =
                self.row_map.get(row_id as usize).cloned()
            {
                self.active = idx;
                self.show_banner(Banner::ConfirmDiscard(idx, path, status));
            }
            return;
        }
        // Discard All: only meaningful on the Changes subheader.
        if action == 10 {
            if let Some(RowTarget::ChangesHeader(idx)) = self.row_map.get(row_id as usize).cloned()
            {
                if !self.sessions[idx].changes.is_empty() {
                    self.active = idx;
                    self.show_banner(Banner::ConfirmDiscardAll(idx));
                }
            }
            return;
        }
        let Some((idx, path, is_dir)) = self.row_target(row_id) else { return };
        // Where "New File/Folder" creates: the dir itself, or a file's parent.
        let create_dir = if is_dir {
            path.clone()
        } else {
            path.parent().map(Path::to_path_buf).unwrap_or_else(|| path.clone())
        };
        let shown_name =
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match action {
            0 => self.open_name_dialog(
                format!("New file in {}", create_dir.display()),
                "",
                NameAction::NewFile(idx, create_dir),
            ),
            1 => self.open_name_dialog(
                format!("New folder in {}", create_dir.display()),
                "",
                NameAction::NewFolder(idx, create_dir),
            ),
            2 => {
                let _ = std::process::Command::new("open").arg("-R").arg(&path).spawn();
            }
            3 => {
                let _ = std::process::Command::new("open").arg(&path).spawn();
            }
            4 => {
                if let Some(cb) = self.clipboard.as_mut() {
                    let _ = cb.set_text(path.display().to_string());
                }
            }
            5 => {
                let rel = self
                    .sessions
                    .get(idx)
                    .map(|s| s.relative_name(&path))
                    .unwrap_or_else(|| path.display().to_string());
                if let Some(cb) = self.clipboard.as_mut() {
                    let _ = cb.set_text(rel);
                }
            }
            6 => {
                let stem =
                    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let ext = path
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                let target = Self::unique_path(
                    &path.parent().unwrap_or(Path::new(".")).join(format!("{stem} copy{ext}")),
                );
                if let Err(err) = Self::copy_recursive(&path, &target) {
                    eprintln!("tigriden: duplicate failed: {err}");
                }
                let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
                self.refresh_dir(idx, &parent);
            }
            7 => self.open_name_dialog(
                format!("Rename {shown_name}"),
                &shown_name,
                NameAction::Rename(idx, path),
            ),
            8 => {
                // Move to ~/.Trash (session roots are protected: close instead).
                if self.sessions.get(idx).is_some_and(|s| s.root == path) {
                    self.close_session(idx);
                    return;
                }
                let Some(trash) = dirs::home_dir().map(|h| h.join(".Trash")) else { return };
                let target = Self::unique_path(&trash.join(path.file_name().unwrap_or_default()));
                if let Err(err) = std::fs::rename(&path, &target) {
                    eprintln!("tigriden: trash failed: {err}");
                    return;
                }
                if let Some(session) = self.sessions.get_mut(idx) {
                    if session.editor.as_ref().is_some_and(|e| e.path.starts_with(&path)) {
                        session.editor = None;
                    }
                    if session.viewer.as_ref().is_some_and(|v| v.path.starts_with(&path)) {
                        session.viewer = None;
                    }
                    if session.editor.is_none() && session.viewer.is_none() {
                        if let Some(ui) = self.ui() {
                            ui.set_editor_frame(Image::default());
                        }
                    }
                }
                let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
                self.refresh_dir(idx, &parent);
                self.update_chrome();
            }
            _ => {}
        }
    }

    pub fn name_dialog_accept(&mut self, name: String) {
        let name = name.trim();
        if name.is_empty() || name.contains('/') {
            self.close_name_dialog();
            return;
        }
        let Some(action) = self.pending_name.take() else {
            self.close_name_dialog();
            return;
        };
        match action {
            NameAction::NewFile(idx, dir) => {
                let target = dir.join(name);
                if !target.exists() {
                    if let Err(err) = std::fs::write(&target, "") {
                        eprintln!("tigriden: create file failed: {err}");
                    } else {
                        self.refresh_dir(idx, &dir);
                        self.really_open_file(idx, target);
                    }
                }
            }
            NameAction::NewFolder(idx, dir) => {
                if let Err(err) = std::fs::create_dir_all(dir.join(name)) {
                    eprintln!("tigriden: create folder failed: {err}");
                }
                self.refresh_dir(idx, &dir);
            }
            NameAction::Rename(idx, path) => {
                let target = path.parent().unwrap_or(Path::new(".")).join(name);
                if target != path && !target.exists() {
                    if let Err(err) = std::fs::rename(&path, &target) {
                        eprintln!("tigriden: rename failed: {err}");
                    } else {
                        if let Some(editor) = self.sessions.get_mut(idx).and_then(|s| s.editor.as_mut())
                        {
                            if editor.path == path {
                                editor.path = target.clone();
                            }
                        }
                        let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
                        self.refresh_dir(idx, &parent);
                        self.update_chrome();
                    }
                }
            }
        }
        self.close_name_dialog();
    }

    pub fn name_dialog_cancel(&mut self) {
        self.close_name_dialog();
    }

    // ----- menu -----

    fn editor_focused(&self) -> bool {
        self.ui().map(|ui| ui.get_editor_focused()).unwrap_or(false)
    }

    pub fn menu_copy(&mut self) {
        if self.editor_focused() {
            self.editor_key("c", Mods { ctrl: false, alt: false, meta: true, shift: false });
        } else {
            self.term_shortcut("c");
        }
    }

    pub fn menu_paste(&mut self) {
        if self.editor_focused() {
            self.editor_key("v", Mods { ctrl: false, alt: false, meta: true, shift: false });
        } else {
            self.term_shortcut("v");
        }
    }

    pub fn menu_select_all(&mut self) {
        if self.editor_focused() {
            self.editor_key("a", Mods { ctrl: false, alt: false, meta: true, shift: false });
        }
    }

    pub fn menu_close_terminal(&mut self) {
        if let Some(session) = self.sessions.get(self.active) {
            let tab = session.active_term;
            self.close_terminal(tab);
        }
    }

    pub fn menu_close_session(&mut self) {
        self.close_session(self.active);
    }

    // ----- banner -----

    fn show_banner(&mut self, banner: Banner) {
        let (text, primary, secondary) = match &banner {
            Banner::Unsaved(_) => {
                let name = self
                    .sessions
                    .get(self.active)
                    .and_then(|s| s.editor.as_ref().map(|e| s.relative_name(&e.path)))
                    .unwrap_or_default();
                (format!("Unsaved changes in {name}"), "Save", "Discard")
            }
            Banner::DiskChanged => {
                ("File changed on disk".to_string(), "Reload", "Keep mine")
            }
            Banner::ConfirmDiscard(idx, path, _) => {
                let name = self
                    .sessions
                    .get(*idx)
                    .map(|s| s.relative_name(path))
                    .unwrap_or_else(|| path.display().to_string());
                (
                    format!("Discard changes to {name}? This restores the last-committed version."),
                    "Discard",
                    "Cancel",
                )
            }
            Banner::ConfirmDiscardAll(idx) => {
                let count = self.sessions.get(*idx).map(|s| s.changes.len()).unwrap_or(0);
                (
                    format!("Discard all {count} changes? This restores every file to the baseline."),
                    "Discard All",
                    "Cancel",
                )
            }
            Banner::None => (String::new(), "", ""),
        };
        self.banner = banner;
        if let Some(ui) = self.ui() {
            ui.set_banner_text(SharedString::from(text));
            ui.set_banner_primary_label(SharedString::from(primary));
            ui.set_banner_secondary_label(SharedString::from(secondary));
        }
    }

    pub fn banner_primary(&mut self) {
        let banner = std::mem::replace(&mut self.banner, Banner::None);
        match banner {
            Banner::Unsaved(action) => {
                self.save_editor();
                self.run_pending(action);
            }
            Banner::DiskChanged => {
                let font_family = self.font_family;
                if let Some(editor) =
                    self.sessions.get_mut(self.active).and_then(|s| s.editor.as_mut())
                {
                    editor.reload(&mut self.font_system, font_family);
                }
                self.apply_editor_size();
                self.render_editor();
                self.update_chrome();
            }
            Banner::ConfirmDiscard(idx, path, status) => {
                let (root, tracking) = match self.sessions.get(idx) {
                    Some(session) => match &session.tracking {
                        Some(tracking) => (session.root.clone(), tracking.clone()),
                        None => return,
                    },
                    None => return,
                };
                let app_id = self.id;
                std::thread::spawn(move || {
                    crate::git::discard(&root, &tracking, &path, status);
                    let _ = slint::invoke_from_event_loop(move || {
                        with_app_id(app_id, |app| {
                            if let Some(i) = app.sessions.iter().position(|s| s.root == root) {
                                app.refresh_changes(i, false);
                            }
                        });
                    });
                });
            }
            Banner::ConfirmDiscardAll(idx) => {
                let (root, tracking, changes) = match self.sessions.get(idx) {
                    Some(session) => match &session.tracking {
                        Some(tracking) => {
                            (session.root.clone(), tracking.clone(), session.changes.clone())
                        }
                        None => return,
                    },
                    None => return,
                };
                let app_id = self.id;
                std::thread::spawn(move || {
                    crate::git::discard_all(&root, &tracking, &changes);
                    let _ = slint::invoke_from_event_loop(move || {
                        with_app_id(app_id, |app| {
                            if let Some(i) = app.sessions.iter().position(|s| s.root == root) {
                                app.refresh_changes(i, false);
                            }
                        });
                    });
                });
            }
            Banner::None => {}
        }
        self.show_banner(Banner::None);
    }

    pub fn banner_secondary(&mut self) {
        let banner = std::mem::replace(&mut self.banner, Banner::None);
        match banner {
            Banner::Unsaved(action) => {
                if let Some(editor) =
                    self.sessions.get_mut(self.active).and_then(|s| s.editor.as_mut())
                {
                    editor.dirty = false;
                }
                self.run_pending(action);
            }
            // Keep mine: mark our version authoritative until the next save.
            Banner::DiskChanged => {
                if let Some(editor) =
                    self.sessions.get_mut(self.active).and_then(|s| s.editor.as_mut())
                {
                    editor.disk_mtime =
                        std::fs::metadata(&editor.path).and_then(|m| m.modified()).ok();
                }
            }
            Banner::ConfirmDiscard(..) | Banner::ConfirmDiscardAll(..) => {} // Cancel
            Banner::None => {}
        }
        self.show_banner(Banner::None);
    }

    fn run_pending(&mut self, action: PendingAction) {
        match action {
            PendingAction::OpenFile(idx, path) => self.really_open_file(idx, path),
            PendingAction::OpenDiff(idx, path) => self.open_diff_view(idx, path),
            PendingAction::CloseSession(idx) => self.really_close_session(idx),
        }
    }

    // ----- chrome / lifecycle -----

    fn refresh_all(&mut self) {
        self.rebuild_tree();
        self.render_term();
        self.render_editor();
        self.update_chrome();
    }

    fn update_chrome(&mut self) {
        let Some(ui) = self.ui() else { return };
        ui.set_has_session(!self.sessions.is_empty());
        match self.sessions.get(self.active) {
            Some(session) => {
                ui.set_viewer_zoomable(
                    session.viewer.as_ref().is_some_and(|v| v.zoomable()),
                );
                match (&session.viewer, &session.editor) {
                    (Some(viewer_state), _) => {
                        ui.set_editor_title(SharedString::from(
                            session.relative_name(&viewer_state.path),
                        ));
                        ui.set_editor_dirty(false);
                        ui.set_editor_view_toggle(SharedString::from(match viewer_state.kind {
                            viewer::ViewKind::Markdown | viewer::ViewKind::Csv => "Source",
                            viewer::ViewKind::Image | viewer::ViewKind::Pdf => "",
                        }));
                    }
                    (None, Some(editor)) if editor.read_only => {
                        ui.set_editor_title(SharedString::from(format!(
                            "Diff — {}",
                            session.relative_name(&editor.path)
                        )));
                        ui.set_editor_dirty(false);
                        ui.set_editor_view_toggle(SharedString::from("File"));
                    }
                    (None, Some(editor)) => {
                        ui.set_editor_title(SharedString::from(session.relative_name(&editor.path)));
                        ui.set_editor_dirty(editor.dirty);
                        ui.set_editor_view_toggle(SharedString::from(
                            match viewer::classify(&editor.path) {
                                Some(viewer::ViewKind::Markdown) => "Rendered",
                                Some(viewer::ViewKind::Csv) => "Table",
                                _ => "",
                            },
                        ));
                    }
                    (None, None) => {
                        ui.set_editor_title(SharedString::default());
                        ui.set_editor_dirty(false);
                        ui.set_editor_view_toggle(SharedString::default());
                    }
                }
                let exited =
                    session.active_term().is_some_and(|t| t.term.exited.load(Ordering::Acquire));
                ui.set_term_overlay(SharedString::from(if exited {
                    "shell exited — close this terminal tab or session"
                } else {
                    ""
                }));
                let tabs: Vec<SharedString> = (1..=session.terms.len())
                    .map(|n| SharedString::from(n.to_string()))
                    .collect();
                ui.set_term_tabs(ModelRc::new(VecModel::from(tabs)));
                ui.set_active_term(session.active_term as i32);
            }
            None => {
                ui.set_viewer_zoomable(false);
                ui.set_editor_title(SharedString::default());
                ui.set_editor_dirty(false);
                ui.set_term_overlay(SharedString::default());
                ui.set_term_tabs(ModelRc::new(VecModel::from(Vec::<SharedString>::new())));
                ui.set_active_term(0);
            }
        }
    }

    pub fn split_changed(&mut self) {
        self.persist();
    }

    fn persist(&mut self) {
        // Only the primary window owns state.toml; secondaries would clobber it.
        if !self.is_primary || self.shutting_down {
            return;
        }
        let state = PersistedState {
            folders: self.sessions.iter().map(|s| s.root.clone()).collect(),
            active: self.active,
            split_ratio: self.ui().map(|ui| ui.get_split_ratio()),
            recent_folders: self.recents.clone(),
        };
        config::save_state(&state);
    }

    pub fn shutdown(&mut self) {
        self.persist();
        self.shutting_down = true;
        for session in &mut self.sessions {
            for handle in &mut session.terms {
                handle.term.shutdown();
            }
        }
        self.sessions.clear();
    }
}
