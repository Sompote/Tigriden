# Tigriden — Terminal for Agentic Coding

![Version](https://img.shields.io/badge/version-0.1.2-e8912d) ![License](https://img.shields.io/badge/license-MIT-blue) ![Platform](https://img.shields.io/badge/platform-macOS-lightgrey)

**A tiny desktop IDE built for one job: supervising AI coding agents.**

Run `claude`, `codex`, `gemini` — any terminal agent — each in its own folder, side by side. Every workspace gets an embedded terminal, a live file tree, and a lightweight code editor so you can watch and steer what your agents build. No run/debug tooling, no chat panel, no LSP: the agents do the heavy lifting, Tigriden gives you eyes and hands.

Written in pure Rust. **~10 MB binary, ~40 MB RAM.**

![Tigriden supervising an agent: the viewer shows a chart the agent produced while the agent CLI runs in one of three terminal tabs below](assets/screenshot.png)

*Above: a real session — the agent's workspace file tree on the left, the built-in viewer inspecting a chart the agent just generated, and the agent CLI running in one of three terminal tabs below.*

## Why

Agentic coding means running several agents in several folders and checking in on them. A full IDE is overkill for that; a bare terminal multiplexer gives you no file browser and no editor. Tigriden is the minimal middle: **one session per folder — agent, files, editor, and change tracking together.**

## Features

- **One-click agents** — preset buttons type the agent command into the terminal for you (fully configurable).
- **Multiple terminals per folder** — the `+` tab spawns extra shells in the same workspace, so one agent can run while you use a second terminal for git, tests, or another agent.
- **Real terminal** — VTE-compliant emulation ([alacritty_terminal](https://crates.io/crates/alacritty_terminal) + a real PTY). TUIs like `vim`, `top`, and the Claude Code interface just work, including bracketed paste and truecolor. Select with the mouse and Cmd+C to copy out; Cmd+V pastes text in, and image paste into Claude Code works with Ctrl+V (the agent reads your clipboard directly).
- **Live file tree** — gitignore-aware, refreshes automatically as agents create and delete files. Right-click any entry for New File/Folder, Reveal in Finder, Open in Default App, Copy (Relative) Path, Duplicate, Rename, and Move to Trash.
- **File change tracking & rollback** *(new in 0.1.1)* — **File ▸ Show Changes Panel** adds a live **Changes (N)** list under each folder showing every file the agent has modified/added/deleted since the baseline, updated automatically within ~1 s of a write. Click a row for a syntax-highlighted diff; right-click ▸ **Discard Changes…** reverts one file, the **↺** button (or **Discard All Changes…**) reverts everything — always behind a confirmation. Two modes, picked automatically: folders with git compare against the last commit; folders **without git get invisible shadow snapshots** (stored in the app's data dir — your folder stays untouched, the agent never sees them). Off by default with zero overhead; toggling on snapshots "now" as the baseline.
- **Multiple windows & agent teams** *(new in 0.1.1)* — **File ▸ New Window** opens an independent window with its own folders, running in parallel. Define named preset groups (`[[teams]]` in config.toml) and pick one per window to give different windows different agent buttons.
- **Drag & drop files** — drop any file from Finder onto the window and its (shell-quoted) path is typed into the terminal, so you can attach files to an agent prompt the same way as in a native terminal.
- **Built-in editor** — syntax highlighting for 40+ languages ([cosmic-text](https://crates.io/crates/cosmic-text) + syntect), edit and Cmd+S save. When an agent edits the open file on disk, it reloads automatically (or asks, if you have unsaved changes).
- **File viewers** — images (png/jpg/gif/webp/bmp/tiff), Markdown rendered with headings, code blocks and inline pictures, CSV/TSV as an aligned table, and PDF text extraction. A header button toggles Markdown/CSV between the rendered view and editable source.
- **Per-folder sessions** — each workspace keeps its own shell, tree, and open file; switching is instant.
- **Recent folders** — every folder you add is remembered permanently; reopen from the ⟳ button or **File ▸ Open Recent**, even after removing it from the workbench.
- **Settings UI** *(new in 0.1.2)* — **File ▸ Settings… (⌘,)** picks the theme (Dark/Light × Classic/Minimal/Vivid), an accent color, the terminal/editor font and size, the interface text size, terminal scrollback, and whether new windows start with the Changes panel. Every change applies immediately to all open windows — chrome, terminal palette and editor highlighting together — and is saved to config.toml.
- **Native menu bar** — File (Add Folder ⌘O, New Terminal ⌘T, Show/Hide Changes Panel, Open Recent, New Window ▸ team, Save ⌘S, Settings ⌘,, Close Terminal ⌘W, Close Folder ⇧⌘W) and Edit (Copy/Paste/Select All) menus, routed to whichever pane has focus.
- **Persistent** — folders, active session, and layout are restored on relaunch (fresh shells each time, by design).
- **Small on purpose** — no webview, no Electron, no C regex libraries. Slint UI with both panes rasterized straight to pixel buffers.

## Quick install (macOS)

No Rust needed — grab the prebuilt app from the [latest release](https://github.com/Sompote/Tigriden/releases/latest):

1. Download **`Tigriden-0.1.2-macos-universal.app.zip`** (one download for both Apple Silicon and Intel).
2. Unzip and drag **Tigriden.app** into **/Applications**.
3. First launch only: the app isn't notarized, so **right-click → Open → Open**, or run:

   ```sh
   xattr -d com.apple.quarantine /Applications/Tigriden.app
   ```

Prefer a bare binary? The release also ships `tigriden-0.1.2-macos-arm64.tar.gz` (Apple Silicon) and `tigriden-0.1.2-macos-x86_64.tar.gz` (Intel) — untar and run `./tigriden`.

<details>
<summary><b>Build from source</b> (stable Rust required)</summary>

```sh
git clone https://github.com/Sompote/Tigriden.git
cd Tigriden
cargo build --release
./target/release/tigriden        # or ./bundle/make-app.sh for dist/Tigriden.app
```

macOS is the primary target; Linux/Windows are untested but the stack is cross-platform.
</details>

## Usage

1. Click **+ Add folder** and pick a project directory — a login shell opens there.
2. Click a preset button (e.g. **claude**) to launch the agent, or type any command.
3. Watch the file tree update as the agent works; click any file to inspect or tweak it.
4. Click **+** in the terminal tab strip to open more terminals in the same folder (each tab is its own shell; ✕ on hover closes one).
5. Add more folders to run more agents in parallel; switch by clicking a session in the sidebar. The ✕ on a session header removes the folder from the workbench (its shells are stopped; the folder stays in Recent).
6. Click **⟳** (bottom of the sidebar) to reopen any previously added folder without the file dialog.

### Track & roll back what the agent changes

1. **File ▸ Show Changes Panel** — a **Changes (N)** section appears under each folder in the sidebar (off by default; it always starts off on launch).
2. Tracking mode is chosen automatically per folder:
   - **Git folders** compare against the last commit — commit in the terminal to accept work and reset the list to zero.
   - **Folders without git** get an invisible **shadow snapshot** taken the moment you enable the panel (stored under `~/Library/Application Support/tigriden/snapshots/`; no `.git` appears in your project). Re-enabling the panel re-snapshots "now".
3. As the agent works, changed files appear within ~1 s as `M` (modified) / `A` (added) / `D` (deleted) rows — the count is files, not edits.
4. **Click a row** to see the accumulated diff against the baseline; the **File** chip jumps to the editable file.
5. Don't like a change? **Right-click the row ▸ Discard Changes…** restores that file to the baseline (new files are deleted, deleted files come back). The **↺** button on the Changes header — or right-click ▸ **Discard All Changes…** — reverts the whole run. Both ask for confirmation first.

Detection is watcher-driven (no polling): bursts of writes are coalesced for 250 ms, `git status` runs on a background thread, and nothing at all runs while the panel is off or the agent is idle.

### Keys

| Context  | Keys |
|----------|------|
| Terminal | everything a terminal expects: Ctrl+C/Z/D/R…, arrows, F1–F12, TUIs; drag to select (double-click = word), Cmd+C copies, Cmd+V pastes (bracketed); wheel scrolls history |
| Editor   | typing, arrows / Home / End / PgUp / PgDn (+Shift selects, +Alt jumps words), Cmd+A / C / X / V, Cmd+S saves |

## Configuration

Most of it is editable in **File ▸ Settings… (⌘,)**; the file is
`~/Library/Application Support/tigriden/config.toml`, created on first run:

```toml
# Theme: classic-|minimal-|vivid- × -dark|-light ("dark"/"light" still work).
theme = "classic-dark"
accent = ""             # "" = theme accent, or "#rrggbb"
font_family = "Menlo"   # terminal + editor
font_size = 13.0
ui_font_size = 13.0     # sidebar, tabs, dialogs (10-18)
scrollback = 10000
show_changes = false    # start new windows with the Changes panel on

[[presets]]
label = "claude"
command = "claude"
send_enter = true

[[presets]]
label = "codex"
command = "codex"

[[presets]]
label = "gemini"
command = "gemini"

# Optional: named preset groups for File ▸ New Window ▸ <team>.
[[teams]]
name = "reviewers"
[[teams.presets]]
label = "claude-review"
command = "claude /review"
send_enter = true
```

Presets and teams are file-only — the Settings dialog links to config.toml for those. Saving from Settings rewrites the whole file, so comments you add by hand are not preserved.

Runtime state (restored folders, split position) lives next to it in `state.toml`; shadow snapshots for the Changes panel live in `snapshots/`.

## Architecture

Slint provides only the chrome (sidebar, layout, splitter). The two hard parts are custom-rendered pixel panes on the CPU:

| Pane | Engine | Rendering |
|------|--------|-----------|
| Terminal | headless `alacritty_terminal` grid fed by `portable-pty` | glyphs rasterized per cell via cosmic-text's swash cache |
| Editor | cosmic-text `SyntaxEditor` (syntect highlighting) | draws itself into the same pixel-buffer canvas |

Because the two panes paint themselves, a theme is one definition in `src/theme.rs` feeding three consumers: the Slint `Theme` global (chrome), the ANSI 0-15 palette (terminal, shared with the PTY threads for OSC color queries), and a syntect theme name (editor).

Only the PTY reader threads run in the background; rendering and editing happen on the UI thread with coalesced repaints.

`vendor/cosmic-text/` is a verbatim copy of cosmic-text 0.19 with one change: its syntect dependency uses the pure-Rust `fancy-regex` engine instead of the oniguruma C library (smaller binary, no C build dependency).

### Debug builds

`cargo build --features framedump`, then run with `TIGRIDEN_DUMP=/tmp/frames` to dump both panes as PNGs. `TIGRIDEN_TEST_INPUT='claude\r'`, `TIGRIDEN_TEST_OPEN=path`, `TIGRIDEN_TEST_SETTINGS='style=vivid,font-size-step=2'` and `TIGRIDEN_TEST_CHANGES=1` (reports the Changes panel's tracking mode and contents around a write) script the first session for headless testing.

## Roadmap / known limitations (v1)

- [ ] Editor undo
- [ ] Mouse reporting to TUIs
- [ ] IME / dead-key composition
- [ ] Editor tabs (currently one open file per session)
- [ ] PDF page rendering (currently text extraction)
- [ ] Linux / Windows testing

## License

MIT
