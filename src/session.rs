use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::editor::EditorState;
use crate::viewer::ViewerState;
use crate::term::TermSession;
use crate::tree::TreeState;

/// One terminal within a session. `id` is globally unique so background
/// threads can route repaint/exit events without ambiguity when several
/// terminals share a folder.
pub struct TermHandle {
    pub id: u64,
    pub term: TermSession,
    /// Coalesces repaint requests from the PTY reader thread.
    pub frame_pending: Arc<AtomicBool>,
}

pub struct Session {
    pub root: PathBuf,
    pub name: String,
    pub terms: Vec<TermHandle>,
    pub active_term: usize,
    pub editor: Option<EditorState>,
    pub viewer: Option<ViewerState>,
    pub tree: TreeState,
    pub tree_visible: bool,
    /// Directories whose listings changed on disk since the last model rebuild.
    pub pending_fs: Vec<PathBuf>,
    /// Set while the changes panel is enabled: real git or shadow snapshot.
    /// Assigned by App after construction so the toggle check stays there.
    pub tracking: Option<crate::git::Tracking>,
    pub changes: Vec<crate::git::Change>,
    /// Generation counters so stale background git results are dropped.
    pub changes_gen: u64,
    pub diff_gen: u64,
    pub changes_visible: bool,
    _watcher: Option<RecommendedWatcher>,
}

impl Session {
    pub fn new(
        root: PathBuf,
        first_term: TermHandle,
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
            terms: vec![first_term],
            active_term: 0,
            editor: None,
            viewer: None,
            tree: TreeState::new(root),
            tree_visible: true,
            pending_fs: Vec::new(),
            tracking: None,
            changes: Vec::new(),
            changes_gen: 0,
            diff_gen: 0,
            changes_visible: true,
            _watcher: watcher,
        }
    }

    pub fn active_term(&self) -> Option<&TermHandle> {
        self.terms.get(self.active_term)
    }

    pub fn active_term_mut(&mut self) -> Option<&mut TermHandle> {
        self.terms.get_mut(self.active_term)
    }

    pub fn relative_name(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.display().to_string())
    }
}
