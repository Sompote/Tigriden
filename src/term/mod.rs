pub mod colors;
pub mod keys;
pub mod render;

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{test::TermSize, Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::Processor;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use crate::term::colors::to_rgb;

/// Callbacks out of the terminal threads into the app. Both must be cheap and
/// thread-safe; repaint is expected to coalesce.
#[derive(Clone)]
pub struct TermHooks {
    pub repaint: Arc<dyn Fn() + Send + Sync>,
    pub exited: Arc<dyn Fn() + Send + Sync>,
}

pub struct EventProxy {
    input_tx: Sender<Vec<u8>>,
    hooks: TermHooks,
    win_size: Arc<Mutex<WindowSize>>,
    /// Shared with the app so OSC color answerbacks follow theme changes.
    theme: Arc<AtomicU8>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup => (self.hooks.repaint)(),
            Event::PtyWrite(text) => {
                let _ = self.input_tx.send(text.into_bytes());
            }
            Event::ColorRequest(index, format) => {
                let theme = crate::theme::by_index(self.theme.load(Ordering::Relaxed));
                let rgb = to_rgb(colors::osc_color(index, theme));
                let _ = self.input_tx.send(format(rgb).into_bytes());
            }
            Event::TextAreaSizeRequest(format) => {
                let size = *self.win_size.lock().unwrap();
                let _ = self.input_tx.send(format(size).into_bytes());
            }
            Event::ClipboardLoad(_, format) => {
                // Clipboard access from TUIs is not supported; answer empty so
                // the application is not left waiting.
                let _ = self.input_tx.send(format("").into_bytes());
            }
            Event::ChildExit(_) | Event::Exit => (self.hooks.exited)(),
            _ => {}
        }
    }
}

pub struct TermSession {
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    input_tx: Sender<Vec<u8>>,
    master: Option<Box<dyn MasterPty + Send>>,
    /// Taken by `shutdown`, which hands it to a thread that reaps it.
    child: Option<Box<dyn Child + Send + Sync>>,
    reader_handle: Option<JoinHandle<()>>,
    writer_handle: Option<JoinHandle<()>>,
    win_size: Arc<Mutex<WindowSize>>,
    pub cols: u16,
    pub rows: u16,
    pub exited: Arc<AtomicBool>,
}

impl TermSession {
    pub fn spawn(
        root: &Path,
        cols: u16,
        rows: u16,
        cell_px: (u16, u16),
        scrollback: usize,
        theme: Arc<AtomicU8>,
        hooks: TermHooks,
    ) -> Result<Self, String> {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: cols * cell_px.0,
                pixel_height: rows * cell_px.1,
            })
            .map_err(|e| format!("openpty: {e}"))?;

        #[cfg(not(windows))]
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        #[cfg(windows)]
        let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
        let mut cmd = CommandBuilder::new(&shell);
        // Interactive login shell so the user's PATH (where agent CLIs live)
        // is available. (cmd.exe takes no such flags.)
        #[cfg(not(windows))]
        cmd.arg("-il");
        cmd.cwd(root);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let child = pty.slave.spawn_command(cmd).map_err(|e| format!("spawn shell: {e}"))?;
        // The slave must be dropped or the reader never sees EOF.
        drop(pty.slave);

        let mut writer = pty.master.take_writer().map_err(|e| format!("pty writer: {e}"))?;
        let mut reader = pty.master.try_clone_reader().map_err(|e| format!("pty reader: {e}"))?;

        let (input_tx, input_rx) = channel::<Vec<u8>>();
        let win_size = Arc::new(Mutex::new(WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: cell_px.0,
            cell_height: cell_px.1,
        }));

        let proxy = EventProxy {
            input_tx: input_tx.clone(),
            hooks: hooks.clone(),
            win_size: win_size.clone(),
            theme,
        };

        let term_config = TermConfig { scrolling_history: scrollback, ..Default::default() };
        let term = Arc::new(FairMutex::new(Term::new(
            term_config,
            &TermSize::new(cols as usize, rows as usize),
            proxy,
        )));

        let exited = Arc::new(AtomicBool::new(false));

        let writer_handle = std::thread::spawn(move || {
            while let Ok(bytes) = input_rx.recv() {
                if writer.write_all(&bytes).is_err() {
                    break;
                }
                let _ = writer.flush();
            }
        });

        let reader_term = term.clone();
        let reader_exited = exited.clone();
        let reader_hooks = hooks.clone();
        let reader_handle = std::thread::spawn(move || {
            let mut processor: Processor = Processor::new();
            let mut buf = [0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        {
                            let mut term = reader_term.lock();
                            processor.advance(&mut *term, &buf[..n]);
                        }
                        (reader_hooks.repaint)();
                    }
                }
            }
            reader_exited.store(true, Ordering::Release);
            (reader_hooks.exited)();
        });

        Ok(Self {
            term,
            input_tx,
            master: Some(pty.master),
            child: Some(child),
            reader_handle: Some(reader_handle),
            writer_handle: Some(writer_handle),
            win_size,
            cols,
            rows,
            exited,
        })
    }

    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.input_tx.send(bytes);
    }

    /// Applies a new scrollback limit to the running terminal. Growing takes
    /// effect immediately for lines from here on; lines the old, smaller
    /// history has already trimmed are gone.
    pub fn set_scrollback(&self, lines: usize) {
        let config = TermConfig { scrolling_history: lines, ..Default::default() };
        self.term.lock().set_options(config);
    }

    pub fn resize(&mut self, cols: u16, rows: u16, cell_px: (u16, u16)) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        let size = PtySize {
            rows,
            cols,
            pixel_width: cols * cell_px.0,
            pixel_height: rows * cell_px.1,
        };
        *self.win_size.lock().unwrap() = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: cell_px.0,
            cell_height: cell_px.1,
        };
        if let Some(master) = &self.master {
            let _ = master.resize(size);
        }
        self.term.lock().resize(TermSize::new(cols as usize, rows as usize));
    }

    /// Hangs up the shell and everything it started. Never blocks.
    ///
    /// Closing a tab, a session or a window runs this, and it runs on the
    /// event loop that every window shares. So it waits on nothing. It used
    /// to wait on three things:
    ///
    /// - `Child::kill`, which signals the shell alone and then sleeps up to
    ///   250 ms on the calling thread before escalating to SIGKILL;
    /// - `Child::wait`, which blocks in `wait4` until the shell is reaped;
    /// - joining the reader thread, which sits in `read()` on the pty master.
    ///
    /// On macOS the kernel revokes the terminal when the session leader dies,
    /// so the read does end quickly. The two process waits are the ones with
    /// no bound: a shell that traps SIGHUP, or one wedged in the kernel on its
    /// way out, holds the event loop for as long as it takes, and both windows
    /// stop drawing. All three now happen on a thread of their own.
    pub fn shutdown(&mut self) {
        // Read the foreground group before the master goes. Job control gives
        // the command the user was looking at a process group of its own, and
        // the shell's group does not cover it.
        #[cfg(unix)]
        let foreground = self.master.as_ref().and_then(|m| m.process_group_leader());
        let Some(mut child) = self.child.take() else { return };

        #[cfg(unix)]
        let groups: Vec<i32> = {
            let shell = child.process_id().map(|pid| pid as i32);
            let mut groups: Vec<i32> = [foreground, shell].into_iter().flatten().collect();
            groups.dedup();
            // SIGHUP first, and right now: a shell that gets it hangs up its
            // own jobs on the way out, which reaches further than we can.
            for pgid in &groups {
                signal_group(*pgid, SIGHUP);
            }
            groups
        };

        // Closing our end of the pty makes the kernel hang up the foreground
        // group as well, and EOFs the reader thread once the slave is free.
        self.master.take();
        self.reader_handle.take();
        // The writer thread ends when the last sender drops, which happens
        // with `self`.
        self.writer_handle.take();

        // Escalate and reap off the event loop: a process that ignores SIGHUP
        // must not cost the UI a frame.
        std::thread::spawn(move || {
            // Give SIGHUP its chance first. A shell that takes it hangs up its
            // own jobs on the way out, which is both gentler and further
            // reaching than anything we can send.
            for _ in 0..5 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
            }
            // Still up, so the shell's pid is still its own and the group is
            // safe to signal.
            #[cfg(unix)]
            for pgid in groups {
                signal_group(pgid, SIGKILL);
            }
            #[cfg(not(unix))]
            let _ = child.kill();
            let _ = child.wait();
        });
    }
}

#[cfg(unix)]
const SIGHUP: i32 = 1;
#[cfg(unix)]
const SIGKILL: i32 = 9;

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// Signals every process in the group led by `pgid`.
///
/// `Child::kill` is no use here: it signals the shell alone, and sleeps up to
/// 250 ms on the calling thread between its own SIGHUP and SIGKILL. The
/// negative pid is what makes this reach the whole group instead.
#[cfg(unix)]
fn signal_group(pgid: i32, sig: i32) {
    if pgid > 1 {
        // Safety: a plain libc call; an unknown or dead group is an error
        // return, not undefined behaviour.
        unsafe { kill(-pgid, sig) };
    }
}

impl Drop for TermSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A shell that ignores SIGHUP with a job in the foreground: the case the
    /// old teardown paid for, since it signalled, slept, and waited for the
    /// shell to die before returning to the event loop.
    #[test]
    fn shutdown_does_not_wait_for_a_shell_that_ignores_sighup() {
        let hooks = TermHooks { repaint: Arc::new(|| {}), exited: Arc::new(|| {}) };
        let theme = Arc::new(AtomicU8::new(0));
        let Ok(mut session) =
            TermSession::spawn(Path::new("/"), 80, 24, (8, 16), 1000, theme, hooks)
        else {
            return; // no pty available here; nothing to assert about
        };

        // An agent CLI the user left running, in miniature.
        session.write(b"trap '' HUP\rsleep 30\r".to_vec());
        std::thread::sleep(Duration::from_millis(2500));

        let start = Instant::now();
        session.shutdown();
        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_millis(50), "shutdown blocked for {elapsed:?}");
    }
}

