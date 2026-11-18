use std::collections::HashSet;
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

/// Changed paths gathered on the watcher thread and handed to the event loop
/// as one batch.
///
/// A dataset write can raise hundreds of thousands of events in a few
/// minutes. One event-loop closure per event, each deduplicating against a
/// list, froze every window for tens of minutes; a set with a cap keeps both
/// the queue and the work bounded.
#[derive(Default)]
pub struct FsBatch {
    pub paths: HashSet<PathBuf>,
    /// More changed than is worth tracking one by one: treat every cached
    /// listing as stale.
    pub overflow: bool,
    /// A closure to deliver this batch is already queued on the event loop.
    pub posted: bool,
}

/// Paths a batch tracks before it gives up and rescans everything.
const FS_BATCH_MAX: usize = 4096;

impl FsBatch {
    pub fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        if self.overflow {
            return;
        }
        for path in paths {
            // Directory events (create/delete inside) arrive as the dir itself;
            // a file event stands for a change to its parent's listing.
            if let Some(parent) = path.parent() {
                if !self.paths.contains(parent) {
                    self.paths.insert(parent.to_path_buf());
                }
            }
            self.paths.insert(path);
        }
        if self.paths.len() > FS_BATCH_MAX {
            self.overflow = true;
            self.paths = HashSet::new();
        }
    }

    pub fn merge(&mut self, other: FsBatch) {
        if other.overflow {
            self.overflow = true;
            self.paths = HashSet::new();
        } else {
            self.add(other.paths);
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.overflow && self.paths.is_empty()
    }
}

pub struct Session {
    pub root: PathBuf,
    pub name: String,
    pub terms: Vec<TermHandle>,
    pub active_term: usize,
    pub editor: Option<EditorState>,
    pub viewer: Option<ViewerState>,
    /// For an HTML file: whether the pane shows the live WebKit view rather
    /// than the typeset reader. The chip cycles Rendered / Reader / Source.
    pub html_web: bool,
    pub tree: TreeState,
    pub tree_visible: bool,
    /// Paths that changed on disk since the last model rebuild.
    pub pending_fs: FsBatch,
    /// The viewer file's mtime when a change storm last checked it, so an
    /// overflowed batch reloads the page only if the file really changed.
    pub viewer_seen_mtime: Option<std::time::SystemTime>,
    /// Set while the changes panel is enabled: real git or shadow snapshot.
    /// Assigned by App after construction so the toggle check stays there.
    pub tracking: Option<crate::git::Tracking>,
    /// The thread every git call for this folder goes through, alive as long
    /// as `tracking` is. Dropping it ends the thread without blocking.
    pub git: Option<crate::git::Worker>,
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
        watch: bool,
        on_fs_event: impl Fn(Vec<PathBuf>) + Send + 'static,
    ) -> Self {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());

        let watcher = watch.then(|| Self::spawn_watcher(&root, on_fs_event)).flatten();

        Self {
            root: root.clone(),
            name,
            terms: vec![first_term],
            active_term: 0,
            editor: None,
            viewer: None,
            html_web: true,
            tree: TreeState::new(root),
            tree_visible: true,
            pending_fs: FsBatch::default(),
            viewer_seen_mtime: None,
            tracking: None,
            git: None,
            changes: Vec::new(),
            changes_gen: 0,
            diff_gen: 0,
            changes_visible: true,
            _watcher: watcher,
        }
    }

    /// A recursive watch on `root`, or None if the platform refused one.
    fn spawn_watcher(
        root: &Path,
        on_fs_event: impl Fn(Vec<PathBuf>) + Send + 'static,
    ) -> Option<RecommendedWatcher> {
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                if !event.paths.is_empty() {
                    on_fs_event(event.paths);
                }
            }
        })
        .ok()
        .and_then(|mut w| w.watch(root, RecursiveMode::Recursive).ok().map(|_| w))
    }

    /// Starts or stops the watch without disturbing the rest of the session,
    /// so the setting takes effect on folders already open.
    pub fn set_watch(
        &mut self,
        on: bool,
        on_fs_event: impl Fn(Vec<PathBuf>) + Send + 'static,
    ) {
        match (on, self._watcher.is_some()) {
            (false, _) => self._watcher = None,
            (true, true) => {}
            (true, false) => self._watcher = Self::spawn_watcher(&self.root, on_fs_event),
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

#[cfg(test)]
mod tests {
    use super::{FsBatch, FS_BATCH_MAX};
    use std::path::PathBuf;

    #[test]
    fn batch_dedupes_and_records_parents() {
        let mut batch = FsBatch::default();
        batch.add([PathBuf::from("/r/a/x.txt"), PathBuf::from("/r/a/y.txt")]);
        batch.add([PathBuf::from("/r/a/x.txt")]);
        assert!(batch.paths.contains(&PathBuf::from("/r/a")));
        assert_eq!(batch.paths.len(), 3);
        assert!(!batch.overflow);
    }

    #[test]
    fn a_change_storm_collapses_to_overflow() {
        let mut batch = FsBatch::default();
        batch.add((0..FS_BATCH_MAX + 10).map(|i| PathBuf::from(format!("/r/data/{i}.png"))));
        assert!(batch.overflow);
        assert!(batch.paths.is_empty());
        assert!(!batch.is_empty());
        // Once overflowed, more events cost nothing.
        batch.add([PathBuf::from("/r/data/more.png")]);
        assert!(batch.paths.is_empty());
        let mut main = FsBatch::default();
        main.add([PathBuf::from("/r/a.txt")]);
        main.merge(batch);
        assert!(main.overflow && main.paths.is_empty());
    }
}
