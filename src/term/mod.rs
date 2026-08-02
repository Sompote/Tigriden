pub mod colors;
pub mod keys;
pub mod render;

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
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
    dark: bool,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup => (self.hooks.repaint)(),
            Event::PtyWrite(text) => {
                let _ = self.input_tx.send(text.into_bytes());
            }
            Event::ColorRequest(index, format) => {
                let rgb = to_rgb(colors::osc_color(index, self.dark));
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
    child: Box<dyn Child + Send + Sync>,
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
        dark: bool,
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
            dark,
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
            child,
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

    pub fn shutdown(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Dropping the master closes the PTY, which EOFs the reader thread.
        self.master.take();
        if let Some(handle) = self.reader_handle.take() {
            let _ = handle.join();
        }
        // Writer thread exits once every sender is gone; ours is dropped with
        // self, so don't join it here.
        self.writer_handle.take();
    }
}

impl Drop for TermSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
