mod app;
mod config;
mod editor;
mod paint;
mod session;
mod term;
mod tree;

use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;

use app::{with_app, App};
use term::keys::Mods;

slint::include_modules!();

fn mods(ctrl: bool, alt: bool, meta: bool, shift: bool) -> Mods {
    Mods { ctrl, alt, meta, shift }
}

fn main() {
    let (config, malformed_config) = config::load_config();
    let state = config::load_state();

    let ui = MainWindow::new().expect("failed to create window");
    if let Some(ratio) = state.split_ratio {
        ui.set_split_ratio(ratio.clamp(0.15, 0.85));
    }

    let app = Rc::new(RefCell::new(App::new(&ui, config)));
    app::install(app.clone());

    if malformed_config {
        eprintln!("tigriden: config.toml is malformed; using defaults (file left untouched)");
    }

    ui.on_add_folder(|| {
        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
            with_app(|app| app.add_session(folder, true));
        }
    });
    ui.on_row_clicked(|id| with_app(|app| app.row_clicked(id)));
    ui.on_row_toggled(|id| with_app(|app| app.row_toggled(id)));
    ui.on_close_session(|idx| with_app(|app| app.close_session(idx as usize)));
    ui.on_preset_clicked(|idx| with_app(|app| app.preset_clicked(idx as usize)));
    ui.on_term_tab_clicked(|tab| with_app(|app| app.term_tab_clicked(tab as usize)));
    ui.on_new_terminal(|| with_app(|app| app.new_terminal_active()));
    ui.on_close_terminal(|tab| with_app(|app| app.close_terminal(tab as usize)));
    ui.on_split_changed(|| with_app(|app| app.split_changed()));
    ui.on_banner_primary(|| with_app(|app| app.banner_primary()));
    ui.on_banner_secondary(|| with_app(|app| app.banner_secondary()));

    ui.on_term_key(|text, ctrl, alt, meta, shift| {
        let mut handled = false;
        with_app(|app| handled = app.term_key(&text, mods(ctrl, alt, meta, shift)));
        handled
    });
    ui.on_term_wheel(|delta| with_app(|app| app.term_wheel(delta)));
    ui.on_term_mouse(|_, _, _| {}); // selection support is a later milestone
    ui.on_term_size_changed(|w, h| with_app(|app| app.term_resized(w, h)));

    ui.on_editor_key(|text, ctrl, alt, meta, shift| {
        let mut handled = false;
        with_app(|app| handled = app.editor_key(&text, mods(ctrl, alt, meta, shift)));
        handled
    });
    ui.on_editor_mouse(|kind, x, y| with_app(|app| app.editor_mouse(kind, x, y)));
    ui.on_editor_wheel(|delta| with_app(|app| app.editor_wheel(delta)));
    ui.on_editor_size_changed(|w, h| with_app(|app| app.editor_resized(w, h)));

    ui.window().on_close_requested(|| {
        with_app(|app| app.shutdown());
        slint::CloseRequestResponse::HideWindow
    });

    // Restore persisted sessions (silently dropping folders that vanished).
    for folder in &state.folders {
        if folder.is_dir() {
            with_app(|app| app.add_session(folder.clone(), false));
        }
    }
    let restore_active = state.active;
    with_app(|app| app.set_active(restore_active));

    ui.run().expect("event loop failed");
    with_app(|app| app.shutdown());
}
