# Tigriden — Terminal for Agentic Coding

![Version](https://img.shields.io/badge/version-0.1.6-e8912d) ![License](https://img.shields.io/badge/license-MIT-blue) ![Platform](https://img.shields.io/badge/platform-macOS-lightgrey)

**A tiny desktop IDE built for one job: supervising AI coding agents.**

Run `claude`, `codex`, `gemini` — any terminal agent — each in its own folder, side by side. Every workspace gets an embedded terminal, a live file panel you can actually manage files in, and a lightweight code editor so you can watch and steer what your agents build. No run/debug tooling, no chat panel, no LSP: the agents do the heavy lifting, Tigriden gives you eyes and hands.

Written in pure Rust. **~10 MB binary, ~40 MB RAM.**

![Tigriden supervising an agent: the viewer shows a chart the agent produced while the agent CLI runs in one of three terminal tabs below](assets/screenshot.png)

*Above: a real session — the agent's workspace file tree on the left, the built-in viewer inspecting a chart the agent just generated, and the agent CLI running in one of three terminal tabs below.*

## Why

Agentic coding means running several agents in several folders and checking in on them. A full IDE is overkill for that; a bare terminal multiplexer gives you no file browser and no editor. Tigriden is the minimal middle: **one session per folder — agent, files, editor, and change tracking together.**

## Features

- **One-click agents** — preset buttons type the agent command into the terminal for you (fully configurable).
- **Multiple terminals per folder** — the `+` tab spawns extra shells in the same workspace, so one agent can run while you use a second terminal for git, tests, or another agent.
- **Real terminal** — VTE-compliant emulation ([alacritty_terminal](https://crates.io/crates/alacritty_terminal) + a real PTY). TUIs like `vim`, `top`, and the Claude Code interface just work, including bracketed paste and truecolor. Select with the mouse and Cmd+C to copy out; Cmd+V pastes text in, and image paste into Claude Code works with Ctrl+V (the agent reads your clipboard directly). *(new in 0.1.5)* **Right-click for Copy / Paste / Select All**, and the **wheel scrolls inside full-screen apps** as well as through history. *(new in 0.1.3)* Keyboard scrollback: Shift+PageUp/PageDown page through history, Shift+Home/End jump to its ends, Shift+↑/↓ go line by line — the unshifted keys still reach the shell, and full-screen apps are left alone.
- **File panel, not just a tree** *(new in 0.1.6)* — gitignore-aware and refreshes automatically as agents create and delete files, and it manages those files too. **Drag files in from Finder** and they are copied into the folder you drop them on (highlighted as you hover; drop below the last row to land in the workspace root). **Cut / Copy / Paste** are the real system pasteboard, so files move both ways between Tigriden and Finder — Cut then Paste moves, Copy then Paste duplicates, and a name clash gets a " 2" suffix rather than overwriting. **Delete** (or ⌘⌫) moves the selection to the Trash after a confirmation. Arrow keys walk the tree (←/→ collapse and expand, and scroll the selection back into view), Return opens, F2 or ⌘R renames, ⌘D duplicates. Right-click any entry for New File/Folder, Cut/Copy/Paste, Reveal in Finder, Open in Default App, Copy (Relative) Path, Duplicate, Rename, and Move to Trash.
- **File change tracking & rollback** *(new in 0.1.1)* — **File ▸ Show Changes Panel** adds a live **Changes (N)** list under each folder showing every file the agent has modified/added/deleted since the baseline, updated automatically within ~1 s of a write. Click a row for a syntax-highlighted diff; right-click ▸ **Discard Changes…** reverts one file, the **↺** button (or **Discard All Changes…**) reverts everything — always behind a confirmation. Two modes, picked automatically: folders with git compare against the last commit; folders **without git get invisible shadow snapshots** (stored in the app's data dir — your folder stays untouched, the agent never sees them). Off by default with zero overhead; toggling on snapshots "now" as the baseline.
- **Multiple windows & agent teams** *(new in 0.1.1)* — **File ▸ New Window** opens an independent window with its own folders, running in parallel. Define named preset groups (`[[teams]]` in config.toml) and pick one per window to give different windows different agent buttons.
- **Drag & drop files** — drop a file from Finder onto the **terminal** and its (shell-quoted) path is typed in, so you can attach files to an agent prompt the same way as in a native terminal; drop it onto the **file panel** instead and it is copied into that folder.
- **Built-in editor** — syntax highlighting for 40+ languages ([cosmic-text](https://crates.io/crates/cosmic-text) + syntect), edit and Cmd+S save. When an agent edits the open file on disk, it reloads automatically (or asks, if you have unsaved changes).
- **File viewers** *(upgraded in 0.1.3, fast & async in 0.1.4, selectable in 0.1.5, Markdown typeset in 0.1.6)* — images (png/jpg/gif/webp/bmp/tiff), **Markdown set on the same white page as LaTeX** — serif face, justified columns, generous margins — with headings, code blocks, inline pictures, **real tables** (grid lines, shaded header row, wrapped cells) and **typeset math**: `$…$` and `$$…$$` go through the same box layout as a .tex file, so fractions stack, `\sum`/`\int` carry their limits and `\tag{1}` is set flush right as the equation number (`math` code fences work too). CSV/TSV as an aligned table, and **PDFs shown as actual pages** — with text extraction as the fallback for files that can't be parsed. PDF pages rasterize and images decode on **background worker threads** with the next page prefetched, so scrolling and zooming never stall the UI: you get an instant preview that sharpens the moment the full-quality bitmap lands. **Select and copy text** anywhere in the viewer — including straight off rendered PDF pages, where a text layer built from the page's own content stream puts the highlight on the glyphs and reads two-column papers one column at a time. Zoom with Cmd+= / Cmd+- / Cmd+0, Ctrl/Cmd+wheel, or the magnifier buttons in the header, and pan in every direction while zoomed in. A **scrollbar** on the right (drag the thumb or click the track) plus PageUp/PageDown, Home/End and ↑/↓ navigate long documents. **LaTeX files (.tex/.latex/.ltx) are typeset, not dumped** — they render on a white page in a serif face with justified columns, a centered `\maketitle` title block and numbered section headings, so a paper looks the way its compiled PDF does (no TeX installation involved). **Display equations get real box layout**: numerators stacked over fraction bars, radicals with an overline spanning the argument, `\sum`/`\int` carrying their limits above and below, stretched `\left(…\right)` delimiters, matrices and `cases`, TeX's own spacing around relations and operators, and the equation number set flush right — `equation`/`align`/`gather`, `\[…\]` and `$$…$$`, with starred forms left unnumbered. Variables are italic while numbers, operators and function names (`min`, `tanh`, `log`) stay upright, in display and inline math alike. **Cross-references resolve to numbers** — a two-pass parse means a forward `\ref` still works, so `\ref` prints the number, `\eqref` parenthesizes it and `\autoref`/`\Cref` name the thing ("Figure 1", "Section 3"); a key defined in a file you didn't open keeps showing rather than vanishing. **Figures render**, including the vector **PDF** plots papers actually ship (rasterized through the same hayro renderer the PDF viewer uses), sized by `\includegraphics[width=…\linewidth]`, centered, with numbered "Figure 1:" / "Table 2:" captions. Also handled: itemize/enumerate/description lists, `tabular` as a real grid, verbatim/lstlisting on a code panel, and `\cite`/`\href` as colored links. The preamble stays hidden, and package options (`[leftmargin=…]`, `[htbp]`, natbib's `[][]`) never leak into the page. Inline math falls back to Unicode (x², xᵢ, α → ∞) since it has to flow inside a line of running text. A header button toggles Markdown/CSV/LaTeX between the rendered view and editable source.
- **Per-folder sessions** — each workspace keeps its own shell, tree, and open file; switching is instant.
- **Recent folders** — every folder you add is remembered permanently; reopen from the ⟳ button or **File ▸ Open Recent**, even after removing it from the workbench.
- **Settings UI** *(new in 0.1.2)* — **File ▸ Settings… (⌘,)** picks the theme (Dark/Light × Classic/Minimal/Vivid), an accent color, the shared font, **separate editor and terminal text sizes** *(0.1.6)*, the interface text size, terminal scrollback (since 0.1.4 applied to already-running terminals too), and whether new windows start with the Changes panel. Every change applies immediately to all open windows — chrome, terminal palette and editor highlighting together — and is saved to config.toml.
- **Native menu bar** — File (Add Folder ⌘O, New Terminal ⌘T, Show/Hide Changes Panel, Open Recent, New Window ▸ team, Save ⌘S, Settings ⌘,, Close Terminal ⌘W, Close Folder ⇧⌘W) and Edit (Copy/Paste/Select All) menus, routed to whichever pane has focus.
- **Persistent** — folders, active session, and layout are restored on relaunch (fresh shells each time, by design).
- **Small on purpose** — no webview, no Electron, no C regex libraries. Slint UI with both panes rasterized straight to pixel buffers.

## Quick install (macOS)

No Rust needed — grab the prebuilt app from the [latest release](https://github.com/Sompote/Tigriden/releases/latest):

1. Download **`Tigriden-0.1.6-macos-universal.app.zip`** (one download for both Apple Silicon and Intel).
2. Unzip and drag **Tigriden.app** into **/Applications**.
3. First launch only: the app isn't notarized, so **right-click → Open → Open**, or run:

   ```sh
   xattr -d com.apple.quarantine /Applications/Tigriden.app
   ```

Prefer a bare binary? The release also ships `tigriden-0.1.6-macos-arm64.tar.gz` (Apple Silicon) and `tigriden-0.1.6-macos-x86_64.tar.gz` (Intel) — untar and run `./tigriden`.

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
| Terminal | everything a terminal expects: Ctrl+C/Z/D/R…, arrows, F1–F12, TUIs; drag to select (double-click = word), Cmd+C copies, Cmd+V pastes (bracketed), **right-click for Copy / Paste / Select All**; wheel scrolls history — and scrolls **inside full-screen apps** (Claude Code, vim, less) too — Shift+PgUp/PgDn pages it, Shift+Home/End jump to the ends, Shift+↑/↓ go line by line |
| File panel | ↑/↓ walk the rows, ←/→ collapse / expand (or step to the parent), Return opens, F2 or ⌘R renames, ⌘D duplicates, ⌘X / ⌘C / ⌘V cut, copy and paste files through the system pasteboard, Delete or ⌘⌫ moves to the Trash (after a confirmation); right-click for the full menu |
| Editor   | typing, arrows / Home / End / PgUp / PgDn (+Shift selects, +Alt jumps words), Cmd+A / C / X / V, Cmd+S saves, right-click for Copy / Paste / Select All |
| Viewer   | wheel scrolls, right-edge scrollbar drags or click-jumps, PgUp/PgDn page, Home/End jump to the ends, ↑/↓ step; drag to select text — including on PDF pages (double-click = word), Cmd+A selects all, Cmd+C / Cmd+X copy, Esc clears, right-click for the menu; Cmd+C with nothing selected copies a whole PDF; on images & PDFs: Cmd+= / Cmd+- zoom, Cmd+0 resets, Ctrl/Cmd+wheel zooms, and a zoomed view pans horizontally |

## Configuration

Most of it is editable in **File ▸ Settings… (⌘,)**, where the editor and the
terminal size independently; the file is
`~/Library/Application Support/tigriden/config.toml`, created on first run:

```toml
# Theme: classic-|minimal-|vivid- × -dark|-light ("dark"/"light" still work).
theme = "classic-dark"
accent = ""             # "" = theme accent, or "#rrggbb"
font_family = "Menlo"   # terminal, editor + code blocks
font_size = 13.0        # editor + viewer (8-28)
term_font_size = 13.0   # terminal, sized on its own (8-28)
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

[[presets]]
label = "opencode"
command = "opencode"

# Optional: named preset groups for File ▸ New Window ▸ <team>.
[[teams]]
name = "reviewers"
[[teams.presets]]
label = "claude-review"
command = "claude /review"
send_enter = true
```

Presets and teams are file-only — the Settings dialog links to config.toml for those. Saving from Settings rewrites the whole file, so comments you add by hand are not preserved.

When a release adds an agent button, a config still holding an older build's stock list picks it up once, recorded as `presets_version`. Add or remove a single preset and the list becomes yours: later releases leave it alone.

Runtime state (restored folders, split position) lives next to it in `state.toml`; shadow snapshots for the Changes panel live in `snapshots/`.

## Architecture

Slint provides only the chrome (sidebar, layout, splitter). The two hard parts are custom-rendered pixel panes on the CPU:

| Pane | Engine | Rendering |
|------|--------|-----------|
| Terminal | headless `alacritty_terminal` grid fed by `portable-pty` | glyphs rasterized per cell via cosmic-text's swash cache |
| Editor | cosmic-text `SyntaxEditor` (syntect highlighting) | draws itself into the same pixel-buffer canvas |

The file viewer paints into the same kind of pixel buffer: Markdown/CSV/LaTeX layout via cosmic-text, PDF pages rasterized by [hayro](https://crates.io/crates/hayro), and images decoded/resampled by the [image](https://crates.io/crates/image) crate — both on per-document worker threads, so the UI thread never waits on them. LaTeX runs through a built-in parser in `src/tex.rs`, which turns math into a tree that `src/mathlayout.rs` measures and positions as boxes — no TeX install needed. Markdown shares both: it renders on the same white sheet and sends its `$…$` bodies through that same parser and box layout, so a formula looks the same whichever file it came from. Only the pages in (or next to) the viewport are kept in memory, requests coalesce so a zoom gesture renders just the final size, and a nearest-neighbor preview covers the gap until each full-quality bitmap arrives.

Because the panes paint themselves, a theme is one definition in `src/theme.rs` feeding three consumers: the Slint `Theme` global (chrome), the ANSI 0-15 palette (terminal, shared with the PTY threads for OSC color queries), and a syntect theme name (editor).

Only the PTY reader threads and the viewer's rasterizer/decoder workers run in the background; rendering and editing happen on the UI thread with coalesced, throttled repaints.

`src/mac.rs` holds the AppKit calls neither Slint nor winit exposes: the pasteboard's *file list* (so the panel's ⌘C/⌘V interoperate with Finder) and the pointer location during an external drag, since winit's drop events carry a path but no coordinates.

`vendor/cosmic-text/` is a verbatim copy of cosmic-text 0.19 with one change: its syntect dependency uses the pure-Rust `fancy-regex` engine instead of the oniguruma C library (smaller binary, no C build dependency).

### Debug builds

`cargo build --features framedump`, then run with `TIGRIDEN_DUMP=/tmp/frames` to dump both panes as PNGs. `TIGRIDEN_TEST_INPUT='claude\r'`, `TIGRIDEN_TEST_OPEN=path`, `TIGRIDEN_TEST_SETTINGS='style=vivid,font-size-step=2'`, `TIGRIDEN_TEST_CHANGES=1` (reports the Changes panel's tracking mode and contents around a write), `TIGRIDEN_TEST_CTXMENU=1` (runs the right-click menu's Copy and Select All against the terminal and reports what reached the clipboard), `TIGRIDEN_TEST_SCROLLBACK=up|down` (wheels the terminal and reports the mode, history size and resulting screen — the way to tell scrollback from alternate-screen scrolling) and `TIGRIDEN_TEST_WHEEL_UI=1` (dispatches a real scroll event through Slint's hit-testing, to prove wheel input still reaches the terminal) script the first session for headless testing.

## Changelog

- **0.1.6** — the left panel becomes a **file manager**. **Drag files in from Finder** and they are copied into the folder you drop on, with the target row highlighted as you drag (winit hands over the dropped paths but no coordinates, so the pointer is read from AppKit and hit-tested against the row list); drop on the terminal instead and you still get the shell-quoted path. **Cut / Copy / Paste use the system pasteboard**, so files move both directions between Tigriden and Finder — Cut+Paste moves, Copy+Paste copies, and a name clash takes a `" 2"` suffix rather than overwriting. **Delete / ⌘⌫ moves to the Trash** behind a confirmation, which the context menu's *Move to Trash* now asks for too. Rows carry a selection and the keyboard drives it: ↑/↓ walk and scroll into view, ←/→ collapse, expand or step to the parent, Return opens, F2 or ⌘R renames, ⌘D duplicates. **Markdown is set on the same white page as LaTeX** — serif, justified columns, paper margins — and its **math is typeset, not dumped**: `$…$` and `$$…$$` go through the same box layout a .tex file gets, so fractions stack, `\sum`/`\int` carry their limits, and `\tag{1}` is set flush right as the equation number (`math` code fences work too). The **terminal has its own text size**, split from the editor and viewer; a config written before the split keeps one size for both until you move it.
- **0.1.5** — copying text, everywhere it was missing. **Select text in the viewer**: drag across Markdown paragraphs, code blocks, CSV tables and the glyphs of rendered PDF pages, double-click for a word, Cmd+A for all, Esc to clear, Cmd+C / Cmd+X to copy. PDF selection builds a real text layer from the same content stream the renderer draws, so highlights sit on the glyphs — on cropped and rotated pages too — and **two-column papers copy one column at a time** instead of zig-zagging across the gutter; Cmd+C with nothing selected still copies the whole document. **Right-click** the terminal, editor or viewer for **Copy / Paste / Select All** — Copy greys out with nothing selected, Paste is hidden on read-only views, and the right-click no longer clears the selection you just made. The **mouse wheel now scrolls inside full-screen apps**: TUIs run on the alternate screen, which keeps no scrollback of its own, so the wheel did nothing at all in Claude Code, vim or less; apps that asked for mouse reporting now get real wheel events at the pointer's cell, and the rest get arrow keys, xterm's alternate-scroll behaviour. Plus an **opencode** preset button, which configs still carrying the stock three take on once.
- **0.1.4** — viewer performance and polish: PDFs rasterize and images decode on background threads (scrolling and zooming no longer stall the UI), pages prefetch ahead of the scroll, wheel repaints are throttled; a scrollbar on the right of the viewer (draggable thumb, click-to-jump) plus PageUp/PageDown, Home/End and ↑/↓ keys; ⌘ detection for wheel zoom and shortcuts now reads the modifier state straight from the OS; scrollback limit changes apply to already-running terminals.
- **0.1.3** — viewer zoom for images and PDFs (Cmd+=/-/0, Ctrl/Cmd+wheel, header magnifier buttons, panning), PDFs rendered as actual pages, Markdown tables drawn as real grids, terminal scrollback keys (Shift+PageUp/PageDown/Home/End/↑/↓).
- **0.1.2** — Settings UI (⌘,): 6 themes, accent colors, fonts and sizes, scrollback, all applied live to every window.
- **0.1.1** — file change tracking with per-file/all rollback (git or invisible shadow snapshots), multiple windows with per-window agent teams.
- **0.1.0** — first release: per-folder sessions with terminal, file tree, editor, viewers, presets.

## Roadmap / known limitations (v1)

- [ ] Editor undo
- [ ] Mouse reporting to TUIs
- [ ] IME / dead-key composition
- [ ] Editor tabs (currently one open file per session)
- [x] PDF page rendering *(done in 0.1.3)*
- [ ] Linux / Windows testing

## License

MIT
