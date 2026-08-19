# Tigriden — A Fast, Tiny End-to-End Research Workbench

![Version](https://img.shields.io/badge/version-0.1.7-e8912d) ![License](https://img.shields.io/badge/license-MIT-blue) ![Platform](https://img.shields.io/badge/platform-macOS-lightgrey)

**From the coding agent that runs your experiments to the scientific paper you submit — one window for the whole thing.** An agentic coding workbench (per-folder terminals, a live file panel, change tracking with one-click rollback, a lightweight editor) with a document viewer bolted to the same window: your `.tex` shown as the page it compiles to, your PDFs as real pages, and the figures and data your code just produced next to both.

Run `claude`, `codex`, `gemini` — any terminal agent — each in its own folder, side by side, and watch what they do: the file panel updates as they write, the Changes panel lists every file they touched and reverts any of it, and the viewer renders whatever they produced — code, a chart, a CSV, a manuscript.

Written in pure Rust. No Electron, no webview, no TeX installation. **~10 MB binary, ~40 MB RAM**, and a paper opens the moment you click it.

![Tigriden showing paperIEE.tex typeset: a two-column journal page with a figure, its caption and running text, beside the manuscript folder's file panel](assets/screenshot.png)

*Above: a real session — `paperIEE.tex` set on the two-column page its `\documentclass` produces, figure, caption and justified columns included, straight from the source with no LaTeX run. The manuscript folder is in the panel on the left; the agent's terminal sits under the viewer, here dragged out of the way to read.*

## The research loop, end to end

One folder, one window, the whole cycle — the agent never leaves the terminal, and you never leave the app to look at what came out:

| Stage | What you do | What Tigriden gives you |
|-------|-------------|-------------------------|
| **1. Code** | `claude` / `codex` / `gemini` in the project folder writes and runs the experiment | A real terminal per folder, extra tabs for `pytest`, `git` or a second agent, and a file panel that updates as files appear |
| **2. Check** | Read what the run produced before trusting it | `results.csv` as an aligned table, `loss.png` or a vector PDF plot as an image, the agent's Markdown notes typeset on a page |
| **3. Keep or undo** | Decide what survives the run | The Changes panel lists every file touched, diffs each one, and reverts a file — or the whole run — with git or with invisible snapshots when there is no repo |
| **4. Write** | Ask the agent to draft or revise the manuscript in the same folder | `paper.tex` opens **typeset** on the page your `\documentclass` produces: two-column IEEE, single-column arXiv, equations, tables, figures — no compile |
| **5. Iterate** | Reread, re-prompt, discard what missed | The typeset page re-renders the instant the agent saves; rollback is one right-click |
| **6. Submit** | Build the real artifact | `latexmk` in a second terminal tab, then open the produced `paper.pdf` in the same viewer — pages, text selection, side-by-side with the source |

Nothing in steps 2–5 costs a LaTeX run or a second application.

## Why

**Agentic coding** means running several agents in several folders and checking in on them. A full IDE is overkill for that; a bare terminal multiplexer gives you no file browser, no diff, no editor. Tigriden is the minimal middle: **one session per folder — agent, files, editor and change tracking together.**

**Writing with an agent** has the same shape, plus a document. Today that means three apps — a terminal for the agent, Overleaf or a PDF viewer to see what the text actually looks like, and Finder to move figures around — and every look at the page costs a full LaTeX compile. Tigriden puts the typeset page in the same window: the paper size, margins and column grid your `\documentclass` asks for, rendered by a built-in typesetter, so it appears instantly and works on a machine with no TeX distribution. Compile when you are ready to submit, not to read a paragraph.

## For agentic coding

- **One-click agents** — preset buttons type the agent command into the terminal for you (`claude`, `codex`, `gemini`, `opencode`, or your own, fully configurable).
- **A real terminal** — VTE-compliant ([alacritty_terminal](https://crates.io/crates/alacritty_terminal) + a real PTY), so `vim`, `top` and the Claude Code TUI just work: bracketed paste, truecolor, mouse selection, right-click Copy / Paste / Select All, and a wheel that scrolls inside full-screen apps as well as through history (Shift+PgUp/PgDn/Home/End/↑/↓ page the scrollback). Drop a file from Finder on the terminal and its shell-quoted path is typed in, ready to attach to a prompt.
- **Multiple terminals per folder** — `+` spawns extra shells in the same workspace, so an agent can run while you use a second tab for git, tests, or another agent.
- **Every file the agent touched, and an undo** *(0.1.1)* — **File ▸ Show Changes Panel** lists modified/added/deleted files within ~1 s of a write, with a syntax-highlighted diff per file. **Discard Changes…** reverts one file, **↺** reverts the whole run, both behind a confirmation. Git folders compare against the last commit; folders without git get invisible shadow snapshots, so a scratch project or a manuscript folder is just as safe.
- **Several projects at once** — one session per folder with its own shell, tree and open file; switching is instant. **File ▸ New Window** runs independent windows in parallel, and named `[[teams]]` give each window its own agent buttons.
- **A file panel that manages files** *(0.1.6)* — gitignore-aware and live as the agent works. Drag files in from Finder, Cut/Copy/Paste through the system pasteboard in both directions, Delete to the Trash behind a confirmation, rename/duplicate/reveal from the keyboard or the context menu.
- **Built-in editor** — syntax highlighting for 40+ languages ([cosmic-text](https://crates.io/crates/cosmic-text) + syntect), Cmd+S to save. When the agent rewrites the file you have open, it reloads automatically (or asks, if you have unsaved edits).
- **Settings, themes, persistence** *(0.1.2)* — **File ▸ Settings… (⌘,)**: six themes, accent color, fonts, **separate editor and terminal text sizes** *(0.1.6)*, scrollback, applied live to every window. A native menu bar (Add Folder ⌘O, New Terminal ⌘T, Open Recent, New Window ▸ team, Save ⌘S, Close ⌘W) routes to whichever pane has focus, and folders, layout and the recent list come back on relaunch.
- **Small on purpose** — no webview, no Electron, no C regex libraries; a Slint shell with both panes rasterized straight to pixel buffers, which is where the ~10 MB binary and the instant startup come from.

## For writing and revising

- **LaTeX on the page, not in a text dump** *(0.1.7)* — the viewer reads `\documentclass` (and `geometry`) and sets the file on that page: letter or A4, the class's margins, **two columns for the journal classes** (IEEEtran, ACM/SIG, RevTeX, `[twocolumn]`). It is **paginated** — numbered sheets, paragraphs continuing into the next column, headings kept with their text, `figure*`/`table*` spanning both columns at a page top, and a float that will not fit moving to the top of the next column while the text flows past it.
- **The parts a paper is made of** — `\maketitle` front matter (spanning title block, `\thanks` as the affiliation footnote, raised `\textsuperscript` markers, Abstract and Index Terms), the class's own section numbering (1.1 or I, A, 1), `\cite` printed as **the number its `\bibitem` will carry**, captions labelled "Figure 1:" or "Fig. 1." as the class does, cross-references resolved to numbers even when they point forward.
- **Real math** — display equations get box layout: stacked fractions, radicals with an overline, `\sum`/`\int` carrying their limits, stretched `\left(…\right)`, matrices and `cases`, the number flush right. Inline math is set inline, with true raised and lowered scripts (V_S, x², km s⁻¹) instead of spelled-out `_S`.
- **Figures and tables of real manuscripts** — `\graphicspath` searched, vector **PDF** plots rasterized, pictures scaled by the `minipage` they sit in; `tabular`/`tabularx`/`longtable` set booktabs style. Preamble noise, package options and unknown commands never leak onto the page.
- **PDFs as actual pages** — the compiled paper, a reference you are citing, a datasheet. **Select and copy text straight off the page**, with two-column papers copying one column at a time instead of zig-zagging across the gutter.
- **Markdown on the same white page**, with the same typeset math — for notes, READMEs and agent-written summaries.
- **Everything else in the folder** — images (png/jpg/gif/webp/bmp/tiff) and CSV/TSV as an aligned table, so a plot or a results file the agent just wrote is one click away.
- **Fast, because nothing compiles** — no `pdflatex` run to see a change; PDF pages rasterize and images decode on background threads with the next page prefetched, so scrolling and zooming never stall. Cmd+= / Cmd+- / Cmd+0 zoom, and the LaTeX page re-typesets at the new size.

<details>
<summary><b>Everything the LaTeX view understands</b> (the long list)</summary>

Paper and margins from `\documentclass` options and `geometry`; one or two columns; pagination with page numbers, column breaks inside paragraphs, keep-with-next headings, `[H]`-pinned versus floating figures and tables. `\maketitle` title blocks, `\thanks` footnotes, `\textsuperscript`, `abstract` and `IEEEkeywords` labels (bold run-in for journals, centered over an inset block for articles). Section numbering per class, `\setcounter{secnumdepth}`, `\parindent`/`\parskip` paragraph shape, `center`/`flushleft`/`\centering`, `\LARGE` down to `\scriptsize` — each scoped to its group. `equation`/`align`/`gather`, `\[…\]`, `$$…$$` and starred forms; a display too wide for its column is set a size smaller. Variables italic, operators and `min`/`tanh`/`log` upright. `\ref`/`\eqref`/`\autoref`/`\Cref` resolved by a two-pass parse; `\cite` numbered from `\bibitem`; `\captionsetup{labelformat=empty}` respected. `\includegraphics` with `\graphicspath`, `width=…\linewidth` and `minipage` scaling, including vector PDF figures. `tabular`, `tabularx`, `longtable` and `array` in booktabs style; itemize/enumerate/description; verbatim/lstlisting/minted on a code panel; `quote` and article abstracts inset from both margins; `\href`/`\url` as links. Unknown commands, package options (`[leftmargin=…]`, `[htbp]`, natbib's `[][]`) and the whole preamble stay off the page.

</details>

## Quick install (macOS)

No Rust needed — grab the prebuilt app from the [latest release](https://github.com/Sompote/Tigriden/releases/latest):

1. Download **`Tigriden-0.1.7-macos-universal.app.zip`** (one download for both Apple Silicon and Intel).
2. Unzip and drag **Tigriden.app** into **/Applications**.
3. First launch only: the app isn't notarized, so **right-click → Open → Open**, or run:

   ```sh
   xattr -d com.apple.quarantine /Applications/Tigriden.app
   ```

Prefer a bare binary? The release also ships `tigriden-0.1.7-macos-arm64.tar.gz` (Apple Silicon) and `tigriden-0.1.7-macos-x86_64.tar.gz` (Intel) — untar and run `./tigriden`.

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

### Supervise a coding agent

1. Click **+ Add folder** and pick a project directory — a login shell opens there.
2. Click a preset button (e.g. **claude**) to launch the agent, or type any command.
3. Watch the file panel update as the agent works; click any file to read or edit it.
4. Click **+** in the terminal tab strip for more shells in the same folder (each tab is its own shell; ✕ on hover closes one).
5. Add more folders to run more agents in parallel; switch by clicking a session in the sidebar. The ✕ on a session header removes the folder (its shells stop; the folder stays in Recent).
6. Click **⟳** (bottom of the sidebar) to reopen any previously added folder without the file dialog.

### Revise a paper with an agent

1. **+ Add folder** → pick the manuscript folder (the one with `paper.tex` and `figures/`). A login shell opens there.
2. **File ▸ Show Changes Panel**, so every file the agent touches is listed and revertible before it types a word.
3. Click **claude** (or type any agent command) and ask for what you want: *"tighten Section 3 to 400 words"*, *"make the notation consistent with Table 2"*, *"add a limitations paragraph"*.
4. Click `paper.tex` in the panel — it opens **typeset**, on the page your class produces, so you can read the revision as a reader will see it. No compile, no `latexmk` watch.
5. Not convinced? Right-click the file in **Changes ▸ Discard Changes…** to put it back, or **↺** to revert the whole run, then ask again.
6. Ready to submit? Run `latexmk` (or your build) in a second terminal tab with **+**, and open the resulting `paper.pdf` in the same viewer to check the real thing.

Everything else works the same way: drop new figures in from Finder, click a `.csv` the agent generated to read it as a table, open a reference PDF and copy a quotation straight off the page.

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
| Viewer   | wheel scrolls, right-edge scrollbar drags or click-jumps, PgUp/PgDn page, Home/End jump to the ends, ↑/↓ step; drag to select text — including on PDF pages (double-click = word), Cmd+A selects all, Cmd+C / Cmd+X copy, Esc clears, right-click for the menu; Cmd+C with nothing selected copies a whole PDF; on images, PDFs and the LaTeX page: Cmd+= / Cmd+- zoom, Cmd+0 resets, Ctrl/Cmd+wheel zooms, and a zoomed view pans horizontally |

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

The file viewer paints into the same kind of pixel buffer: Markdown/CSV/LaTeX layout via cosmic-text, PDF pages rasterized by [hayro](https://crates.io/crates/hayro), and images decoded/resampled by the [image](https://crates.io/crates/image) crate — both on per-document worker threads, so the UI thread never waits on them. LaTeX runs through a built-in parser in `src/tex.rs`, which turns math into a tree that `src/mathlayout.rs` measures and positions as boxes — no TeX install needed. The page itself is a second pass: `src/tex.rs` reads the class options into a page geometry (paper, margins, columns), and the viewer packs the blocks into that grid column by column and sheet by sheet, splitting a paragraph at a line boundary when it runs off the bottom and holding full-width floats over to the top of the next page. Because the type size is derived from the page width, a resized pane or a zoom step re-typesets the document rather than reflowing it. Markdown shares both: it renders on the same white sheet and sends its `$…$` bodies through that same parser and box layout, so a formula looks the same whichever file it came from. Only the pages in (or next to) the viewport are kept in memory, requests coalesce so a zoom gesture renders just the final size, and a nearest-neighbor preview covers the gap until each full-quality bitmap arrives.

Because the panes paint themselves, a theme is one definition in `src/theme.rs` feeding three consumers: the Slint `Theme` global (chrome), the ANSI 0-15 palette (terminal, shared with the PTY threads for OSC color queries), and a syntect theme name (editor).

Only the PTY reader threads and the viewer's rasterizer/decoder workers run in the background; rendering and editing happen on the UI thread with coalesced, throttled repaints.

`src/mac.rs` holds the AppKit calls neither Slint nor winit exposes: the pasteboard's *file list* (so the panel's ⌘C/⌘V interoperate with Finder) and the pointer location during an external drag, since winit's drop events carry a path but no coordinates.

`vendor/cosmic-text/` is a verbatim copy of cosmic-text 0.19 with one change: its syntect dependency uses the pure-Rust `fancy-regex` engine instead of the oniguruma C library (smaller binary, no C build dependency).

### Debug builds

`cargo build --features framedump`, then run with `TIGRIDEN_DUMP=/tmp/frames` to dump both panes as PNGs. `TIGRIDEN_TEST_INPUT='claude\r'`, `TIGRIDEN_TEST_OPEN=path`, `TIGRIDEN_TEST_SETTINGS='style=vivid,font-size-step=2'`, `TIGRIDEN_TEST_CHANGES=1` (reports the Changes panel's tracking mode and contents around a write), `TIGRIDEN_TEST_CTXMENU=1` (runs the right-click menu's Copy and Select All against the terminal and reports what reached the clipboard), `TIGRIDEN_TEST_SCROLLBACK=up|down` (wheels the terminal and reports the mode, history size and resulting screen — the way to tell scrollback from alternate-screen scrolling) and `TIGRIDEN_TEST_WHEEL_UI=1` (dispatches a real scroll event through Slint's hit-testing, to prove wheel input still reaches the terminal) script the first session for headless testing. For the typeset LaTeX page there is a faster loop that needs no window: `TEX_DUMP=paper.tex TEX_DUMP_OUT=/tmp/tex TEX_DUMP_PAGES=3 TEX_DUMP_FROM=0 cargo test tex_sheet_dump -- --nocapture` writes one PNG per page.

## Changelog

- **0.1.7** — **LaTeX is set on the page its class asks for.** The viewer reads `\documentclass` (and a `geometry` call) and lays the document out on that page — letter or A4, the class's margins, **two columns for the journal classes** (IEEEtran, ACM/SIG, RevTeX, `[twocolumn]`) — then **paginates** it: numbered sheets, paragraphs continuing into the next column, headings kept with their text, full-width `figure*`/`table*` at the top of a page, and any float that will not fit moving to the top of the next column while the text flows past it (`[H]` stays pinned). The type size follows the page, so a paper reads at page size rather than editor size, and Cmd+= / Cmd+- re-typeset it. Front matter comes out as `\maketitle` sets it — spanning title block, `\thanks` as the affiliation footnote, raised `\textsuperscript` markers, `Abstract`/`Index Terms` labels — sections carry the class's own numbering (1.1 or I, A, 1), or none under `secnumdepth`), `\cite` prints the number its `\bibitem` will carry, and captions are labelled the way the class labels them. **Inline math is finally inline**: real raised and lowered scripts (V_S, km s⁻¹) instead of spelled-out `_S`; a display too wide for its column is set a size smaller. `\graphicspath` figures resolve (vector PDF plots included) and scale with the `minipage` around them, `tabularx` joins `tabular` as a booktabs-style table, `center`/`\centering` and the `\LARGE`…`\scriptsize` sizes apply — and stop at the end of their group, which is what kept leaking a table's `\scriptsize` into the rest of a paper.
- **0.1.6** — the left panel becomes a **file manager**. **Drag files in from Finder** and they are copied into the folder you drop on, with the target row highlighted as you drag (winit hands over the dropped paths but no coordinates, so the pointer is read from AppKit and hit-tested against the row list); drop on the terminal instead and you still get the shell-quoted path. **Cut / Copy / Paste use the system pasteboard**, so files move both directions between Tigriden and Finder — Cut+Paste moves, Copy+Paste copies, and a name clash takes a `" 2"` suffix rather than overwriting. **Delete / ⌘⌫ moves to the Trash** behind a confirmation, which the context menu's *Move to Trash* now asks for too. Rows carry a selection and the keyboard drives it: ↑/↓ walk and scroll into view, ←/→ collapse, expand or step to the parent, Return opens, F2 or ⌘R renames, ⌘D duplicates. **Markdown is set on the same white page as LaTeX** — serif, justified columns, paper margins — and its **math is typeset, not dumped**: `$…$` and `$$…$$` go through the same box layout a .tex file gets, so fractions stack, `\sum`/`\int` carry their limits, and `\tag{1}` is set flush right as the equation number (`math` code fences work too). The **terminal has its own text size**, split from the editor and viewer; a config written before the split keeps one size for both until you move it.
- **0.1.5** — copying text, everywhere it was missing. **Select text in the viewer**: drag across Markdown paragraphs, code blocks, CSV tables and the glyphs of rendered PDF pages, double-click for a word, Cmd+A for all, Esc to clear, Cmd+C / Cmd+X to copy. PDF selection builds a real text layer from the same content stream the renderer draws, so highlights sit on the glyphs — on cropped and rotated pages too — and **two-column papers copy one column at a time** instead of zig-zagging across the gutter; Cmd+C with nothing selected still copies the whole document. **Right-click** the terminal, editor or viewer for **Copy / Paste / Select All** — Copy greys out with nothing selected, Paste is hidden on read-only views, and the right-click no longer clears the selection you just made. The **mouse wheel now scrolls inside full-screen apps**: TUIs run on the alternate screen, which keeps no scrollback of its own, so the wheel did nothing at all in Claude Code, vim or less; apps that asked for mouse reporting now get real wheel events at the pointer's cell, and the rest get arrow keys, xterm's alternate-scroll behaviour. Plus an **opencode** preset button, which configs still carrying the stock three take on once.
- **0.1.4** — viewer performance and polish: PDFs rasterize and images decode on background threads (scrolling and zooming no longer stall the UI), pages prefetch ahead of the scroll, wheel repaints are throttled; a scrollbar on the right of the viewer (draggable thumb, click-to-jump) plus PageUp/PageDown, Home/End and ↑/↓ keys; ⌘ detection for wheel zoom and shortcuts now reads the modifier state straight from the OS; scrollback limit changes apply to already-running terminals.
- **0.1.3** — viewer zoom for images and PDFs (Cmd+=/-/0, Ctrl/Cmd+wheel, header magnifier buttons, panning), PDFs rendered as actual pages, Markdown tables drawn as real grids, terminal scrollback keys (Shift+PageUp/PageDown/Home/End/↑/↓).
- **0.1.2** — Settings UI (⌘,): 6 themes, accent colors, fonts and sizes, scrollback, all applied live to every window.
- **0.1.1** — file change tracking with per-file/all rollback (git or invisible shadow snapshots), multiple windows with per-window agent teams.
- **0.1.0** — first release: per-folder sessions with terminal, file tree, editor, viewers, presets.

## Roadmap / known limitations (v1)

The LaTeX view is a fast reader's approximation of the page, not a TeX engine — for the final artifact, compile:

- Line breaking is TeX-like but not TeX: no hyphenation and no paragraph-wide optimum, so line breaks and page breaks differ from the compiled PDF.
- Side-by-side `minipage` panels stack vertically, each at its declared width.
- Macros you `\newcommand` are not expanded; `\input`/`\include` files are not pulled in (open them directly).
- A journal's affiliation footnote is set under the byline rather than at the foot of the first column.

Elsewhere:

- [ ] Editor undo
- [ ] Mouse reporting to TUIs
- [ ] IME / dead-key composition
- [ ] Editor tabs (currently one open file per session)
- [x] PDF page rendering *(done in 0.1.3)*
- [x] LaTeX typeset on the class's own page *(done in 0.1.7)*
- [ ] Linux / Windows testing

## License

MIT
