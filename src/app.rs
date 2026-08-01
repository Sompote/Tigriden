use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use alacritty_terminal::grid::Scroll;
use cosmic_text::{FontSystem, SwashCache, SyntaxSystem};
use slint::{ComponentHandle, Image, ModelRc, SharedString, VecModel};

use crate::config::{self, Config, PersistedState};
use crate::editor::{EditorState, KeyOutcome};
use crate::session::{Session, TermHandle};
use crate::term::keys::{self, Mods};
use crate::term::render::TermRenderer;
use crate::term::{TermHooks, TermSession};
use crate::{MainWindow, PresetItem, TreeRow};

static SYNTAX_SYSTEM: OnceLock<SyntaxSystem> = OnceLock::new();
static NEXT_TERM_ID: AtomicU64 = AtomicU64::new(1);

fn syntax_system() -> &'static SyntaxSystem {
    SYNTAX_SYSTEM.get_or_init(SyntaxSystem::new)
}

thread_local! {
    static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) };
}

pub fn install(app: Rc<RefCell<App>>) {
    APP.with(|slot| *slot.borrow_mut() = Some(app));
}

/// Runs `f` against the installed app. Used by closures arriving via
/// invoke_from_event_loop (PTY repaints, watcher events, timers).
pub fn with_app(f: impl FnOnce(&mut App)) {
    APP.with(|slot| {
        if let Some(rc) = slot.borrow().as_ref() {
            f(&mut rc.borrow_mut());
        }
    });
}

#[derive(Clone)]
enum RowTarget {
    Header(usize),
    Dir(usize, PathBuf),
    File(usize, PathBuf),
}

enum PendingAction {
    OpenFile(usize, PathBuf),
    CloseSession(usize),
}

enum Banner {
    None,
    Unsaved(PendingAction),
    DiskChanged,
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
    ui: slint::Weak<MainWindow>,
    pub config: Config,
    dark: bool,
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
    banner: Banner,
    fs_timer_armed: bool,
    resize_timer_armed: bool,
    shutting_down: bool,
}

impl App {
    pub fn new(ui: &MainWindow, config: Config, recents: Vec<PathBuf>) -> Self {
        let dark = config.theme != "light";
        let font_family: &'static str = Box::leak(config.font_family.clone().into_boxed_str());
        let presets: Vec<PresetItem> = config
            .presets
            .iter()
            .map(|p| PresetItem { label: SharedString::from(p.label.as_str()) })
            .collect();
        ui.set_presets(ModelRc::new(VecModel::from(presets)));

        Self {
            ui: ui.as_weak(),
            config,
            dark,
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
            banner: Banner::None,
            fs_timer_armed: false,
            resize_timer_armed: false,
            shutting_down: false,
        }
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
        let hooks = Self::make_hooks(id, frame_pending.clone());

        match TermSession::spawn(
            root,
            cols,
            rows,
            (cell_w as u16, cell_h as u16),
            self.config.scrollback,
            self.dark,
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
        let session = Session::new(root, first_term, move |paths| {
            let fs_root = fs_root.clone();
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|app| app.fs_changed(&fs_root, paths));
            });
        });
        self.sessions.push(session);
        self.set_active(self.sessions.len() - 1);
        if persist {
            self.persist();
        }

        // Test hooks: type $TIGRIDEN_TEST_INPUT into the first session's
        // terminal / open $TIGRIDEN_TEST_OPEN in its editor shortly after
        // launch (escape \r for Enter).
        #[cfg(feature = "framedump")]
        if self.sessions.len() == 1 {
            if let Ok(cmds) = std::env::var("TIGRIDEN_TEST_INPUT") {
                // Stages separated by \\t are sent 3 s apart.
                for (i, stage) in cmds.split("\\t").enumerate() {
                    let bytes = stage.replace("\\r", "\r").into_bytes();
                    let delay = std::time::Duration::from_millis(1500 + i as u64 * 3000);
                    slint::Timer::single_shot(delay, move || {
                        with_app(|app| {
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
                slint::Timer::single_shot(std::time::Duration::from_millis(2500), || {
                    with_app(|app| app.new_terminal_active());
                });
                if mode == "back" {
                    slint::Timer::single_shot(std::time::Duration::from_millis(5000), || {
                        with_app(|app| app.term_tab_clicked(0));
                    });
                }
            }
            if let Ok(path) = std::env::var("TIGRIDEN_TEST_OPEN") {
                slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
                    with_app(|app| app.open_file(0, std::path::PathBuf::from(&path)));
                });
            }
        }
    }

    fn make_hooks(term_id: u64, pending: Arc<AtomicBool>) -> TermHooks {
        let repaint = Arc::new(move || {
            if !pending.swap(true, Ordering::AcqRel) {
                let _ = slint::invoke_from_event_loop(move || {
                    with_app(|app| app.term_repaint(term_id));
                });
            }
        });
        let exited = Arc::new(move || {
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|app| app.term_exited(term_id));
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
            None => {}
        }
    }

    fn rebuild_tree(&mut self) {
        let Some(ui) = self.ui() else { return };
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
            slint::Timer::single_shot(std::time::Duration::from_millis(250), || {
                with_app(|app| app.drain_fs());
            });
        }
    }

    fn drain_fs(&mut self) {
        self.fs_timer_armed = false;
        let mut needs_tree = false;
        let mut reload_check: Vec<usize> = Vec::new();
        for (i, session) in self.sessions.iter_mut().enumerate() {
            if session.pending_fs.is_empty() {
                continue;
            }
            let dirs = std::mem::take(&mut session.pending_fs);
            if let Some(editor) = &session.editor {
                if dirs.iter().any(|p| *p == editor.path) {
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
    }

    fn maybe_reload_editor(&mut self, idx: usize) {
        let is_active = idx == self.active;
        let font_family = self.font_family;
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
            self.dark,
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

    pub fn editor_key(&mut self, text: &str, mods: Mods) -> bool {
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

    pub fn editor_mouse(&mut self, kind: i32, x: f32, y: f32) {
        let scale = self.scale();
        let Some(session) = self.sessions.get_mut(self.active) else { return };
        let Some(editor) = session.editor.as_mut() else { return };
        editor.handle_mouse(&mut self.font_system, kind, (x * scale) as i32, (y * scale) as i32);
        if kind != 1 {
            self.render_editor();
        }
    }

    pub fn editor_wheel(&mut self, delta: f32) {
        let scale = self.scale();
        let Some(session) = self.sessions.get_mut(self.active) else { return };
        let Some(editor) = session.editor.as_mut() else { return };
        editor.scroll(&mut self.font_system, delta * scale);
        self.render_editor();
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
        let Some(editor) = session.editor.as_mut() else {
            ui.set_editor_frame(Image::default());
            return;
        };
        if w == 0 || h == 0 {
            return;
        }
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
                if scrolled {
                    handle.term.term.lock().scroll_display(Scroll::Bottom);
                }
                handle.term.write(bytes);
                true
            }
            None => false,
        }
    }

    fn term_shortcut(&mut self, text: &str) -> bool {
        match text.chars().next().map(|c| c.to_ascii_lowercase()) {
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
            slint::Timer::single_shot(std::time::Duration::from_millis(50), || {
                with_app(|app| {
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
            self.dark,
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
        let Some(preset) = self.config.presets.get(idx) else { return };
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
            Banner::None => {}
        }
        self.show_banner(Banner::None);
    }

    fn run_pending(&mut self, action: PendingAction) {
        match action {
            PendingAction::OpenFile(idx, path) => self.really_open_file(idx, path),
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
                match &session.editor {
                    Some(editor) => {
                        ui.set_editor_title(SharedString::from(session.relative_name(&editor.path)));
                        ui.set_editor_dirty(editor.dirty);
                    }
                    None => {
                        ui.set_editor_title(SharedString::default());
                        ui.set_editor_dirty(false);
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
        if self.shutting_down {
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
