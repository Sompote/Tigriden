mod app;
mod config;
mod editor;
mod fonts;
mod git;
mod html;
mod mac;
mod mathlayout;
mod paint;
mod session;
mod term;
mod tex;
mod theme;
mod tree;
mod viewer;
mod webview;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use slint::ComponentHandle;

use app::{with_app_id, App};
use term::keys::Mods;

slint::include_modules!();

fn mods(ctrl: bool, alt: bool, meta: bool, shift: bool) -> Mods {
    // Slint follows the Qt convention on macOS: its `control` modifier is the
    // ⌘ Command key and `meta` is the physical Ctrl key. Terminal semantics
    // need the physical keys, so swap them back.
    #[cfg(target_os = "macos")]
    let (ctrl, meta) = (meta, ctrl);
    // Slint's bookkeeping can miss or lag modifier changes (⌘+key combos
    // arrive with stale flags under load); trust the OS when it says a
    // modifier is down right now.
    let (nc, na, nm, ns) = app::native_modifier_state();
    Mods { ctrl: ctrl || nc, alt: alt || na, meta: meta || nm, shift: shift || ns }
}

thread_local! {
    /// What the key of the event slint is about to deliver types on a Latin
    /// layout, kept from the winit event that still knows its position.
    static LATIN_KEY: Cell<Option<char>> = const { Cell::new(None) };
}

/// The character a key types on a US layout, from where it sits on the board.
fn latin_key(code: winit::keyboard::KeyCode) -> Option<char> {
    use winit::keyboard::KeyCode as C;
    Some(match code {
        C::KeyA => 'a',
        C::KeyB => 'b',
        C::KeyC => 'c',
        C::KeyD => 'd',
        C::KeyE => 'e',
        C::KeyF => 'f',
        C::KeyG => 'g',
        C::KeyH => 'h',
        C::KeyI => 'i',
        C::KeyJ => 'j',
        C::KeyK => 'k',
        C::KeyL => 'l',
        C::KeyM => 'm',
        C::KeyN => 'n',
        C::KeyO => 'o',
        C::KeyP => 'p',
        C::KeyQ => 'q',
        C::KeyR => 'r',
        C::KeyS => 's',
        C::KeyT => 't',
        C::KeyU => 'u',
        C::KeyV => 'v',
        C::KeyW => 'w',
        C::KeyX => 'x',
        C::KeyY => 'y',
        C::KeyZ => 'z',
        C::Digit0 => '0',
        C::Digit1 => '1',
        C::Digit2 => '2',
        C::Digit3 => '3',
        C::Digit4 => '4',
        C::Digit5 => '5',
        C::Digit6 => '6',
        C::Digit7 => '7',
        C::Digit8 => '8',
        C::Digit9 => '9',
        C::Minus => '-',
        C::Equal => '=',
        C::BracketLeft => '[',
        C::BracketRight => ']',
        C::Backslash => '\\',
        C::Semicolon => ';',
        C::Quote => '\'',
        C::Backquote => '`',
        C::Comma => ',',
        C::Period => '.',
        C::Slash => '/',
        C::Space => ' ',
        _ => return None,
    })
}

/// The text of a key event, resolved through a Latin layout when it is part
/// of a Ctrl or ⌘ combo.
///
/// Slint reports what the current layout types, and a Thai (or Cyrillic, or
/// Greek) layout types Ctrl+ญ where the key cap says O: no shortcut matches
/// it and no control byte encodes it, so ^O never reaches nano. macOS itself
/// falls back to a Latin layout for ⌘ shortcuts; do the same for both, using
/// the position of the key that was actually pressed.
fn key_text(text: &slint::SharedString, mods: &Mods) -> slint::SharedString {
    if !(mods.ctrl || mods.meta) {
        return text.clone();
    }
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        // Arrows, Home, the F-keys: slint spells those in the private use
        // area, and no layout types them.
        (Some(ch), None) if !ch.is_ascii() && !('\u{e000}'..='\u{f8ff}').contains(&ch) => {}
        _ => return text.clone(),
    }
    match LATIN_KEY.get() {
        Some(latin) if mods.shift => latin.to_ascii_uppercase().to_string().into(),
        Some(latin) => latin.to_string().into(),
        None => text.clone(),
    }
}

struct WindowOpts {
    /// Team index into `config.teams`; None = the default flat preset list.
    team: Option<usize>,
    initial_folders: Vec<PathBuf>,
    is_primary: bool,
    split_ratio: Option<f32>,
    restore_active: usize,
}

fn open_window(config: config::Config, recents: Vec<PathBuf>, opts: WindowOpts) {
    let ui = MainWindow::new().expect("failed to create window");
    if let Some(ratio) = opts.split_ratio {
        ui.set_split_ratio(ratio.clamp(0.15, 0.85));
    }

    let app = Rc::new(RefCell::new(App::new(
        &ui,
        config.clone(),
        recents,
        opts.team,
        opts.is_primary,
    )));
    let app_id = app.borrow().id;
    app::register(app_id, app, ui.clone_strong());
    with_app_id(app_id, |app| app.update_recents_model());

    wire_callbacks(&ui, app_id);

    // Open initial folders (silently dropping folders that vanished).
    for folder in &opts.initial_folders {
        if folder.is_dir() {
            with_app_id(app_id, |app| app.add_session(folder.clone(), false));
        }
    }
    with_app_id(app_id, |app| app.set_active(opts.restore_active));

    ui.show().expect("failed to show window");
}

fn wire_callbacks(ui: &MainWindow, app_id: u64) {
    ui.on_add_folder(move || {
        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
            with_app_id(app_id, |app| app.add_session(folder, true));
        }
    });
    // New Window ▸ team — a plain closure, not inside with_app_id, because
    // open_window registers a new app in the same registry.
    ui.on_new_window(move |team_idx| {
        let Some(folder) = rfd::FileDialog::new().pick_folder() else { return };
        let team = (team_idx >= 0).then_some(team_idx as usize);
        open_window(
            app::config(),
            config::load_state().recent_folders,
            WindowOpts {
                team,
                initial_folders: vec![folder],
                is_primary: false,
                split_ratio: None,
                restore_active: 0,
            },
        );
    });
    ui.on_recent_clicked(move |i| with_app_id(app_id, |app| app.recent_clicked(i as usize)));
    ui.on_recent_forget(move |i| with_app_id(app_id, |app| app.forget_recent(i as usize)));
    ui.on_row_clicked(move |id| with_app_id(app_id, |app| app.row_clicked(id)));
    ui.on_row_toggled(move |id| with_app_id(app_id, |app| app.row_toggled(id)));
    ui.on_close_session(move |idx| with_app_id(app_id, |app| app.close_session(idx as usize)));
    ui.on_preset_clicked(move |idx| with_app_id(app_id, |app| app.preset_clicked(idx as usize)));
    ui.on_term_tab_clicked(move |tab| with_app_id(app_id, |app| app.term_tab_clicked(tab as usize)));
    ui.on_new_terminal(move || with_app_id(app_id, |app| app.new_terminal_active()));
    ui.on_close_terminal(move |tab| with_app_id(app_id, |app| app.close_terminal(tab as usize)));
    ui.on_split_changed(move || with_app_id(app_id, |app| app.split_changed()));
    ui.on_menu_save(move || with_app_id(app_id, |app| app.save_editor()));
    ui.on_menu_cut(move || with_app_id(app_id, |app| app.menu_cut()));
    ui.on_menu_copy(move || with_app_id(app_id, |app| app.menu_copy()));
    ui.on_menu_paste(move || with_app_id(app_id, |app| app.menu_paste()));
    ui.on_menu_select_all(move || with_app_id(app_id, |app| app.menu_select_all()));
    ui.on_menu_close_terminal(move || with_app_id(app_id, |app| app.menu_close_terminal()));
    ui.on_menu_close_session(move || with_app_id(app_id, |app| app.menu_close_session()));
    ui.on_tree_context(move |action, id| with_app_id(app_id, |app| app.tree_context(action, id)));
    ui.on_tree_key(move |text, ctrl, alt, meta, shift| {
        let mods = mods(ctrl, alt, meta, shift);
        let text = key_text(&text, &mods);
        let mut handled = false;
        with_app_id(app_id, |app| handled = app.tree_key(&text, mods));
        handled
    });
    ui.on_name_dialog_accept(move |name| {
        with_app_id(app_id, |app| app.name_dialog_accept(name.to_string()))
    });
    ui.on_name_dialog_cancel(move || with_app_id(app_id, |app| app.name_dialog_cancel()));
    ui.on_settings_open(move || with_app_id(app_id, |app| app.open_settings()));
    ui.on_settings_close(move || with_app_id(app_id, |app| app.close_settings()));
    // The settings callbacks below touch every window, so they must not run
    // inside with_app_id (which holds a borrow on this one).
    ui.on_settings_changed(|key, value| app::settings_changed(&key, &value));
    ui.on_settings_reset(app::settings_reset);
    ui.on_settings_reveal_config(app::reveal_config);
    ui.on_toggle_view(move || with_app_id(app_id, |app| app.toggle_view()));
    ui.on_viewer_zoom_in(move || with_app_id(app_id, |app| app.viewer_zoom_in()));
    ui.on_viewer_zoom_out(move || with_app_id(app_id, |app| app.viewer_zoom_out()));
    ui.on_toggle_changes(move || with_app_id(app_id, |app| app.toggle_changes()));
    ui.on_banner_primary(move || with_app_id(app_id, |app| app.banner_primary()));
    ui.on_banner_secondary(move || with_app_id(app_id, |app| app.banner_secondary()));

    ui.on_term_key(move |text, ctrl, alt, meta, shift| {
        let mods = mods(ctrl, alt, meta, shift);
        let text = key_text(&text, &mods);
        let mut handled = false;
        with_app_id(app_id, |app| handled = app.term_key(&text, mods));
        handled
    });
    ui.on_term_wheel(move |delta| with_app_id(app_id, |app| app.term_wheel(delta)));
    ui.on_term_mouse(move |kind, x, y| with_app_id(app_id, |app| app.term_mouse(kind, x, y)));
    ui.on_term_size_changed(move |w, h| with_app_id(app_id, |app| app.term_resized(w, h)));
    ui.on_term_context(move |action| with_app_id(app_id, |app| app.term_context(action)));

    ui.on_editor_key(move |text, ctrl, alt, meta, shift| {
        let mods = mods(ctrl, alt, meta, shift);
        let text = key_text(&text, &mods);
        let mut handled = false;
        with_app_id(app_id, |app| handled = app.editor_key(&text, mods));
        handled
    });
    ui.on_editor_mouse(move |kind, x, y| with_app_id(app_id, |app| app.editor_mouse(kind, x, y)));
    ui.on_editor_wheel(move |dx, dy, zoom| {
        with_app_id(app_id, |app| app.editor_wheel(dx, dy, zoom))
    });
    ui.on_editor_size_changed(move |w, h| with_app_id(app_id, |app| app.editor_resized(w, h)));
    ui.on_editor_context(move |action| with_app_id(app_id, |app| app.editor_context(action)));

    // External file drops arrive as winit events the Slint DropArea never
    // sees; forward them to the active terminal as a typed path.
    ui.window().on_winit_window_event(move |_, event| match event {
        // Remember where the key sits on the board: by the time slint hands us
        // the event, only the character the layout types is left.
        winit::event::WindowEvent::KeyboardInput { event, .. } => {
            if event.state == winit::event::ElementState::Pressed {
                LATIN_KEY.set(match event.physical_key {
                    winit::keyboard::PhysicalKey::Code(code) => latin_key(code),
                    _ => None,
                });
            }
            EventResult::Propagate
        }
        winit::event::WindowEvent::DroppedFile(path) => {
            let path = path.clone();
            with_app_id(app_id, move |app| app.file_dropped(path));
            EventResult::PreventDefault
        }
        winit::event::WindowEvent::HoveredFile(_) => {
            with_app_id(app_id, |app| app.file_drop_hover(true));
            EventResult::PreventDefault
        }
        winit::event::WindowEvent::HoveredFileCancelled => {
            with_app_id(app_id, |app| app.file_drop_hover(false));
            EventResult::PreventDefault
        }
        _ => EventResult::Propagate,
    });

    // Kill this window's PTYs now, then drop its registry entry outside the
    // callback (dropping the window inside its own handler is unsound); quit
    // once the last window is gone.
    ui.window().on_close_requested(move || {
        with_app_id(app_id, |app| app.shutdown());
        let _ = slint::invoke_from_event_loop(move || {
            if app::remove_window(app_id) == 0 {
                let _ = slint::quit_event_loop();
            }
        });
        slint::CloseRequestResponse::HideWindow
    });
}

fn main() {
    let (config, malformed_config) = config::load_config();
    let state = config::load_state();

    if malformed_config {
        eprintln!("tigriden: config.toml is malformed; using defaults (file left untouched)");
    }
    app::set_config(config.clone());

    open_window(
        config,
        state.recent_folders.clone(),
        WindowOpts {
            team: None,
            initial_folders: state.folders.clone(),
            is_primary: true,
            split_ratio: state.split_ratio,
            restore_active: state.active,
        },
    );

    slint::run_event_loop().expect("event loop failed");
    // Covers macOS ⌘Q, which quits the loop without a per-window close_requested.
    app::shutdown_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_mods() -> Mods {
        Mods { ctrl: false, alt: false, meta: false, shift: false }
    }

    #[test]
    fn ctrl_combos_resolve_through_a_latin_layout() {
        // A Thai layout types ญ where the key cap says O.
        LATIN_KEY.set(Some('o'));
        let ctrl = Mods { ctrl: true, ..no_mods() };
        assert_eq!(key_text(&"ญ".into(), &ctrl), "o");
        let cmd = Mods { meta: true, ..no_mods() };
        assert_eq!(key_text(&"ญ".into(), &cmd), "o");
        let shifted = Mods { ctrl: true, shift: true, ..no_mods() };
        assert_eq!(key_text(&"ญ".into(), &shifted), "O");
        // Typing Thai is typing Thai: only combos are resolved.
        assert_eq!(key_text(&"ญ".into(), &no_mods()), "ญ");
        // A layout that already types Latin keeps what it typed, wherever the
        // key sits (Dvorak, AZERTY).
        assert_eq!(key_text(&"c".into(), &ctrl), "c");
        // Special keys (arrows, F-keys) and IME bursts pass through.
        let up = term::keys::K_UP.to_string();
        assert_eq!(key_text(&up.as_str().into(), &ctrl), up);
        assert_eq!(key_text(&"ญญ".into(), &ctrl), "ญญ");
    }

    #[test]
    fn latin_key_maps_the_letter_row() {
        use winit::keyboard::KeyCode;
        assert_eq!(latin_key(KeyCode::KeyO), Some('o'));
        assert_eq!(latin_key(KeyCode::Slash), Some('/'));
        assert_eq!(latin_key(KeyCode::F1), None);
    }
}
