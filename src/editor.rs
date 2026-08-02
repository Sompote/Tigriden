use std::path::{Path, PathBuf};
use std::time::SystemTime;

use cosmic_text::{
    Action, Attrs, Buffer, Edit, Family, FontSystem, Metrics, Motion, Selection, Shaping,
    SwashCache, SyntaxEditor, SyntaxSystem,
};
use slint::{Rgba8Pixel, SharedPixelBuffer};

use crate::paint::Canvas;
use crate::term::keys::Mods;

// Slint special-key characters relevant to editing.
const K_BACKSPACE: char = '\u{0008}';
const K_TAB: char = '\u{0009}';
const K_RETURN: char = '\u{000a}';
const K_ESCAPE: char = '\u{001b}';
const K_DELETE: char = '\u{007f}';
const K_UP: char = '\u{F700}';
const K_DOWN: char = '\u{F701}';
const K_LEFT: char = '\u{F702}';
const K_RIGHT: char = '\u{F703}';
const K_HOME: char = '\u{F729}';
const K_END: char = '\u{F72B}';
const K_PAGE_UP: char = '\u{F72C}';
const K_PAGE_DOWN: char = '\u{F72D}';

pub fn dark_theme_name() -> &'static str {
    "base16-eighties.dark"
}

pub fn light_theme_name() -> &'static str {
    "InspiredGitHub"
}

/// What the app should do after a key event.
pub enum KeyOutcome {
    Ignored,
    Consumed,
    Save,
}

pub struct EditorState {
    pub editor: SyntaxEditor<'static, 'static>,
    pub path: PathBuf,
    pub dirty: bool,
    /// mtime recorded at load/save, used to suppress self-inflicted watcher
    /// events and detect external changes.
    pub disk_mtime: Option<SystemTime>,
    /// Read-only mode for generated content (diff views); `path` then points
    /// at the underlying file, not at what the buffer holds.
    pub read_only: bool,
    mouse_down: bool,
}

fn mono_attrs(font_family: &'static str) -> Attrs<'static> {
    Attrs::new().family(Family::Name(font_family))
}

impl EditorState {
    pub fn open(
        font_system: &mut FontSystem,
        syntax_system: &'static SyntaxSystem,
        path: &Path,
        font_family: &'static str,
        font_size_px: f32,
        dark: bool,
    ) -> Result<Self, String> {
        let metrics = Metrics::new(font_size_px, (font_size_px * 1.45).round());
        let buffer = Buffer::new(font_system, metrics);
        let theme = if dark { dark_theme_name() } else { light_theme_name() };
        let mut editor = SyntaxEditor::new(buffer, syntax_system, theme)
            .ok_or_else(|| format!("missing syntect theme {theme}"))?;
        editor
            .load_text(font_system, path, mono_attrs(font_family))
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        editor.set_tab_width(4);
        let disk_mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        Ok(Self {
            editor,
            path: path.to_path_buf(),
            dirty: false,
            disk_mtime,
            read_only: false,
            mouse_down: false,
        })
    }

    /// Read-only view of a unified diff, highlighted via the built-in Diff
    /// syntax. `path` is the underlying file (used for the title and to jump
    /// to the editable view).
    pub fn open_diff(
        font_system: &mut FontSystem,
        syntax_system: &'static SyntaxSystem,
        path: &Path,
        diff_text: &str,
        font_family: &'static str,
        font_size_px: f32,
        dark: bool,
    ) -> Result<Self, String> {
        let metrics = Metrics::new(font_size_px, (font_size_px * 1.45).round());
        let buffer = Buffer::new(font_system, metrics);
        let theme = if dark { dark_theme_name() } else { light_theme_name() };
        let mut editor = SyntaxEditor::new(buffer, syntax_system, theme)
            .ok_or_else(|| format!("missing syntect theme {theme}"))?;
        editor.syntax_by_extension("diff");
        editor.with_buffer_mut(|buffer| {
            buffer.set_text(diff_text, &mono_attrs(font_family), Shaping::Advanced, None);
        });
        editor.set_tab_width(4);
        Ok(Self {
            editor,
            path: path.to_path_buf(),
            dirty: false,
            disk_mtime: None,
            read_only: true,
            mouse_down: false,
        })
    }

    pub fn reload(&mut self, font_system: &mut FontSystem, font_family: &'static str) {
        let path = self.path.clone();
        let _ = self.editor.load_text(font_system, &path, mono_attrs(font_family));
        self.dirty = false;
        self.disk_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    }

    pub fn text(&self) -> String {
        self.editor.with_buffer(|buffer| {
            let mut out = String::new();
            for line in buffer.lines.iter() {
                out.push_str(line.text());
                out.push_str(line.ending().as_str());
            }
            out
        })
    }

    pub fn save(&mut self) -> Result<(), String> {
        if self.read_only {
            return Ok(());
        }
        let text = self.text();
        let tmp = self.path.with_extension("tigriden-tmp");
        std::fs::write(&tmp, &text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        self.dirty = false;
        self.disk_mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        Ok(())
    }

    /// Maps a Slint key event to editor actions. Returns Save for Cmd+S so the
    /// app can persist and refresh UI state.
    pub fn handle_key(
        &mut self,
        font_system: &mut FontSystem,
        clipboard: &mut Option<arboard::Clipboard>,
        text: &str,
        mods: &Mods,
    ) -> KeyOutcome {
        let Some(ch) = text.chars().next() else { return KeyOutcome::Ignored };

        if mods.meta {
            if self.read_only {
                // Copy and select-all only; swallow the mutating shortcuts.
                match ch.to_ascii_lowercase() {
                    's' | 'x' | 'v' => return KeyOutcome::Consumed,
                    'a' | 'c' => {}
                    _ => return KeyOutcome::Ignored,
                }
            }
            match ch.to_ascii_lowercase() {
                's' => return KeyOutcome::Save,
                'a' => {
                    self.editor.set_selection(Selection::Normal(cosmic_text::Cursor::new(0, 0)));
                    self.editor.action(font_system, Action::Motion(Motion::BufferEnd));
                    return KeyOutcome::Consumed;
                }
                'c' => {
                    if let (Some(cb), Some(sel)) = (clipboard.as_mut(), self.editor.copy_selection())
                    {
                        let _ = cb.set_text(sel);
                    }
                    return KeyOutcome::Consumed;
                }
                'x' => {
                    if let Some(cb) = clipboard.as_mut() {
                        if let Some(sel) = self.editor.copy_selection() {
                            let _ = cb.set_text(sel);
                        }
                    }
                    if self.editor.delete_selection() {
                        self.dirty = true;
                    }
                    return KeyOutcome::Consumed;
                }
                'v' => {
                    if let Some(cb) = clipboard.as_mut() {
                        if let Ok(pasted) = cb.get_text() {
                            self.editor.insert_string(&pasted, None);
                            self.dirty = true;
                        }
                    }
                    return KeyOutcome::Consumed;
                }
                _ => return KeyOutcome::Ignored,
            }
        }

        // Shift+motion extends the selection (set an anchor before moving).
        let motion = |m: Motion| Some(m);
        let maybe_motion = match ch {
            K_UP => motion(Motion::Up),
            K_DOWN => motion(Motion::Down),
            K_LEFT if mods.alt => motion(Motion::LeftWord),
            K_LEFT => motion(Motion::Left),
            K_RIGHT if mods.alt => motion(Motion::RightWord),
            K_RIGHT => motion(Motion::Right),
            K_HOME => motion(Motion::Home),
            K_END => motion(Motion::End),
            K_PAGE_UP => motion(Motion::PageUp),
            K_PAGE_DOWN => motion(Motion::PageDown),
            _ => None,
        };
        if let Some(m) = maybe_motion {
            if mods.shift {
                if self.editor.selection() == Selection::None {
                    self.editor.set_selection(Selection::Normal(self.editor.cursor()));
                }
            } else {
                self.editor.set_selection(Selection::None);
            }
            self.editor.action(font_system, Action::Motion(m));
            return KeyOutcome::Consumed;
        }

        if self.read_only {
            // Escape still clears the selection; everything else is inert.
            if ch == K_ESCAPE {
                self.editor.action(font_system, Action::Escape);
                return KeyOutcome::Consumed;
            }
            return KeyOutcome::Ignored;
        }

        let mutating = match ch {
            K_RETURN => Some(Action::Enter),
            K_BACKSPACE => Some(Action::Backspace),
            K_DELETE => Some(Action::Delete),
            K_TAB if mods.shift => Some(Action::Unindent),
            K_TAB => Some(Action::Indent),
            K_ESCAPE => {
                self.editor.action(font_system, Action::Escape);
                return KeyOutcome::Consumed;
            }
            c if !c.is_control() && !mods.ctrl => Some(Action::Insert(c)),
            _ => None,
        };
        match mutating {
            Some(action) => {
                self.editor.action(font_system, action);
                self.dirty = true;
                KeyOutcome::Consumed
            }
            None => KeyOutcome::Ignored,
        }
    }

    /// kind: 0 = down, 1 = up, 2 = move, 3 = double-click (physical px).
    pub fn handle_mouse(&mut self, font_system: &mut FontSystem, kind: i32, x: i32, y: i32) {
        match kind {
            0 => {
                self.mouse_down = true;
                self.editor.action(font_system, Action::Click { x, y });
            }
            1 => self.mouse_down = false,
            2 if self.mouse_down => self.editor.action(font_system, Action::Drag { x, y }),
            3 => self.editor.action(font_system, Action::DoubleClick { x, y }),
            _ => {}
        }
    }

    pub fn scroll(&mut self, font_system: &mut FontSystem, delta_px: f32) {
        self.editor.action(font_system, Action::Scroll { pixels: -delta_px });
    }

    pub fn set_viewport(&mut self, font_system: &mut FontSystem, width_px: f32, height_px: f32) {
        self.editor.with_buffer_mut(|buffer| {
            let _ = font_system;
            buffer.set_size(Some(width_px), Some(height_px));
        });
    }

    pub fn render(
        &mut self,
        font_system: &mut FontSystem,
        swash_cache: &mut SwashCache,
        width_px: u32,
        height_px: u32,
    ) -> SharedPixelBuffer<Rgba8Pixel> {
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width_px.max(1), height_px.max(1));
        let bg = self.editor.background_color();
        let (w, h) = (buffer.width() as i32, buffer.height() as i32);
        let mut canvas = Canvas { pixels: buffer.make_mut_slice(), width: w, height: h };
        canvas.fill([bg.r(), bg.g(), bg.b()]);
        self.editor.shape_as_needed(font_system, true);
        self.editor.draw(font_system, swash_cache, |x, y, rw, rh, color| {
            canvas.blend_rect(x, y, rw as i32, rh as i32, color);
        });
        buffer
    }
}
