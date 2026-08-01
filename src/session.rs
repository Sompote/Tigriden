use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::editor::EditorState;
use crate::term::TermSession;
use crate::tree::TreeState;

pub struct Session {
    pub root: PathBuf,
    pub name: String,
    pub term: TermSession,
    pub editor: Option<EditorState>,
    pub tree: TreeState,
    pub tree_visible: bool,
    /// Coalesces repaint requests from the PTY reader thread.
    pub frame_pending: Arc<AtomicBool>,
    /// Directories whose listings changed on disk since the last model rebuild.
    pub pending_fs: Vec<PathBuf>,
    _watcher: Option<RecommendedWatcher>,
}

impl Session {
    pub fn new(
        root: PathBuf,
        term: TermSession,
        frame_pending: Arc<AtomicBool>,
        on_fs_event: impl Fn(Vec<PathBuf>) + Send + 'static,
    ) -> Self {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());

        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                if !event.paths.is_empty() {
                    on_fs_event(event.paths);
                }
            }
        })
        .ok()
        .and_then(|mut w| w.watch(&root, RecursiveMode::Recursive).ok().map(|_| w));

        Self {
            root: root.clone(),
            name,
            term,
            editor: None,
            tree: TreeState::new(root),
            tree_visible: true,
            frame_pending,
            pending_fs: Vec::new(),
            _watcher: watcher,
        }
    }

    pub fn relative_name(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.display().to_string())
    }
}
