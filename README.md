# Tigriden — Terminal for Agentic Coding

**A tiny desktop IDE built for one job: supervising AI coding agents.**

Run `claude`, `codex`, `gemini` — any terminal agent — each in its own folder, side by side. Every workspace gets an embedded terminal, a live file tree, and a lightweight code editor so you can watch and steer what your agents build. No run/debug tooling, no chat panel, no LSP: the agents do the heavy lifting, Tigriden gives you eyes and hands.

Written in pure Rust. **~10 MB binary, ~40 MB RAM.**

```
┌──────────┬─────────────────────────────┐
│ AGENTS   │  editor (click a file)      │
│ ▸ proj-a │                             │
│ ▾ proj-b │  fn main() {                │
│   src/   │      ...                    │
│   lib.rs │  }                          │
│          ├─────────────────────────────┤
│          │ [1][2][+] [claude] [codex]  │
│          │  ❯ claude                   │
│ [+ Add]  │  ⏺ Working on task...      │
└──────────┴─────────────────────────────┘
```

## Why

Agentic coding means running several agents in several folders and checking in on them. A full IDE is overkill for that; a bare terminal multiplexer gives you no file browser and no editor. Tigriden is the minimal middle: **one window, one session per folder, agent + files + editor together.**

## Features

- **One-click agents** — preset buttons type the agent command into the terminal for you (fully configurable).
- **Multiple terminals per folder** — the `+` tab spawns extra shells in the same workspace, so one agent can run while you use a second terminal for git, tests, or another agent.
- **Real terminal** — VTE-compliant emulation ([alacritty_terminal](https://crates.io/crates/alacritty_terminal) + a real PTY). TUIs like `vim`, `top`, and the Claude Code interface just work, including bracketed paste and truecolor.
- **Live file tree** — gitignore-aware, refreshes automatically as agents create and delete files.
- **Built-in editor** — syntax highlighting for 40+ languages ([cosmic-text](https://crates.io/crates/cosmic-text) + syntect), edit and Cmd+S save. When an agent edits the open file on disk, it reloads automatically (or asks, if you have unsaved changes).
- **Per-folder sessions** — each workspace keeps its own shell, tree, and open file; switching is instant.
- **Persistent** — folders, active session, and layout are restored on relaunch (fresh shells each time, by design).
- **Small on purpose** — no webview, no Electron, no C regex libraries. Slint UI with both panes rasterized straight to pixel buffers.

## Install

```sh
git clone <this-repo>
cd Tigriden
cargo build --release
./target/release/tigriden
```

Requires stable Rust. macOS is the primary target (Linux/Windows untested but the stack is cross-platform).

## Usage

1. Click **+ Add folder** and pick a project directory — a login shell opens there.
2. Click a preset button (e.g. **claude**) to launch the agent, or type any command.
3. Watch the file tree update as the agent works; click any file to inspect or tweak it.
4. Click **+** in the terminal tab strip to open more terminals in the same folder (each tab is its own shell; ✕ on hover closes one).
5. Add more folders to run more agents in parallel; switch by clicking a session in the sidebar.

### Keys

| Context  | Keys |
|----------|------|
| Terminal | everything a terminal expects (Ctrl+C, arrows, TUIs); Cmd+V pastes (bracketed); wheel scrolls history |
| Editor   | typing, arrows / Home / End / PgUp / PgDn (+Shift selects, +Alt jumps words), Cmd+A / C / X / V, Cmd+S saves |

## Configuration

`~/Library/Application Support/tigriden/config.toml`, created on first run:

```toml
theme = "dark"          # "dark" | "light"
font_family = "Menlo"
font_size = 13.0
scrollback = 10000

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
```

Runtime state (restored folders, split position) lives next to it in `state.toml`.

## Architecture

Slint provides only the chrome (sidebar, layout, splitter). The two hard parts are custom-rendered pixel panes on the CPU:

| Pane | Engine | Rendering |
|------|--------|-----------|
| Terminal | headless `alacritty_terminal` grid fed by `portable-pty` | glyphs rasterized per cell via cosmic-text's swash cache |
| Editor | cosmic-text `SyntaxEditor` (syntect highlighting) | draws itself into the same pixel-buffer canvas |

Only the PTY reader threads run in the background; rendering and editing happen on the UI thread with coalesced repaints.

`vendor/cosmic-text/` is a verbatim copy of cosmic-text 0.19 with one change: its syntect dependency uses the pure-Rust `fancy-regex` engine instead of the oniguruma C library (smaller binary, no C build dependency).

### Debug builds

`cargo build --features framedump`, then run with `TIGRIDEN_DUMP=/tmp/frames` to dump both panes as PNGs. `TIGRIDEN_TEST_INPUT='claude\r'` and `TIGRIDEN_TEST_OPEN=path` script the first session for headless testing.

## Roadmap / known limitations (v1)

- [ ] Editor undo
- [ ] Terminal text selection & copy
- [ ] Mouse reporting to TUIs
- [ ] IME / dead-key composition
- [ ] Editor tabs (currently one open file per session)
- [ ] Linux / Windows testing

## License

MIT
