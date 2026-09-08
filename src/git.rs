use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};

/// Serializes work on one folder's repository.
///
/// Two windows on the same folder resolve to the same repository: a real one
/// has a single index, and [`detect`] keys the shadow repo by folder path. Git
/// takes `index.lock` for the length of an `add`, `commit` or `checkout`, and
/// the process that loses the race fails. Those failures are swallowed on
/// purpose (the next refresh tells the truth), so a collision showed up as an
/// empty changes panel rather than an error.
fn repo_lock(root: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks.lock().unwrap_or_else(|e| e.into_inner());
    locks.entry(root.to_path_buf()).or_default().clone()
}

/// Runs `job` with exclusive use of the folder's repository. [`Worker`] wraps
/// every job in one call, so the locks never nest.
fn with_repo<T>(root: &Path, job: impl FnOnce() -> T) -> T {
    let lock = repo_lock(root);
    // A poisoned lock only means some other job panicked, which says nothing
    // about the repository.
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    job()
}

/// How a session's changes are tracked.
#[derive(Clone, Debug, PartialEq)]
pub enum Tracking {
    /// The folder has its own .git repository.
    Git,
    /// Tigriden's hidden snapshot repository; the git dir lives in the app's
    /// data folder, so the project folder itself stays untouched.
    Shadow(PathBuf),
}

impl Tracking {
    fn shadow_dir(&self) -> Option<&Path> {
        match self {
            Tracking::Git => None,
            Tracking::Shadow(dir) => Some(dir),
        }
    }
}

/// Picks the tracking mode for a folder: its real repo when present,
/// otherwise a per-folder shadow repo under the app's config dir.
pub fn detect(root: &Path) -> Option<Tracking> {
    if root.join(".git").exists() {
        return Some(Tracking::Git);
    }
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = dirs::config_dir()?
        .join("tigriden")
        .join("snapshots")
        .join(format!("{name}-{:016x}", fnv1a(&root.to_string_lossy())));
    Some(Tracking::Shadow(dir))
}

fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn git_cmd(root: &Path, tracking: &Tracking) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).arg("--no-optional-locks");
    if let Some(dir) = tracking.shadow_dir() {
        cmd.arg("--git-dir").arg(dir).arg("--work-tree").arg(root);
    }
    cmd
}

/// Whether the shadow repo already holds a baseline commit.
///
/// `git init` writes HEAD before anything is committed, so the file existing
/// proves nothing: an interrupted first snapshot leaves a repo with no commit
/// that every later run would then take for finished.
fn has_baseline(dir: &Path) -> bool {
    Command::new("git")
        .arg("--git-dir")
        .arg(dir)
        .args(["rev-parse", "-q", "--verify", "HEAD"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Creates the shadow repo if missing and commits the folder's current state
/// as the baseline everything is compared against. Never call on the event
/// loop — the first snapshot of a big folder takes a while.
///
/// `force` is the explicit "watch from now" gesture. Without it an existing
/// baseline is left alone: two windows can share a folder, and re-snapshotting
/// under the other one would drop every change it still offers to roll back.
pub fn snapshot_baseline(root: &Path, dir: &Path, force: bool) {
    if !force && has_baseline(dir) {
        return;
    }
    if !dir.join("HEAD").exists() {
        let _ = std::fs::create_dir_all(dir);
        let _ = Command::new("git").arg("--git-dir").arg(dir).args(["init", "-q"]).status();
        // Keep bulky generated dirs out of snapshots.
        let _ = std::fs::create_dir_all(dir.join("info"));
        let _ = std::fs::write(
            dir.join("info/exclude"),
            "node_modules/\ntarget/\ndist/\nbuild/\n.venv/\n__pycache__/\n.DS_Store\n",
        );
    }
    let tracking = Tracking::Shadow(dir.to_path_buf());
    let _ = git_cmd(root, &tracking).args(["add", "-A"]).status();
    let _ = git_cmd(root, &tracking)
        .args([
            "-c",
            "user.name=tigriden",
            "-c",
            "user.email=snapshot@tigriden.local",
            "commit",
            "-q",
            "-m",
            "snapshot baseline",
        ])
        .status();
}

/// One changed file in a session's working tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// Repo-relative path exactly as git prints it (slash-separated).
    pub rel: String,
    /// Combined status letter: M / A / D.
    pub status: char,
}

impl Change {
    pub fn abs(&self, root: &Path) -> PathBuf {
        root.join(&self.rel)
    }
}

/// Combined working-tree changes vs HEAD; untracked files count as added.
/// Never call on the event-loop thread.
pub fn status(root: &Path, tracking: &Tracking) -> Vec<Change> {
    let out = match git_cmd(root, tracking)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out);
    let mut fields = text.split('\0');
    let mut changes = Vec::new();
    while let Some(entry) = fields.next() {
        if entry.len() < 4 {
            continue; // trailing empty field after the last NUL
        }
        let (xy, rel) = entry.split_at(3); // "XY " + path
        let x = xy.as_bytes()[0] as char;
        let y = xy.as_bytes()[1] as char;
        if x == 'R' || x == 'C' {
            let _ = fields.next(); // with -z the origin path follows as its own field
        }
        let status = match (x, y) {
            ('?', _) => 'A',
            (x, y) if x == 'D' || y == 'D' => 'D',
            ('A', _) => 'A',
            _ => 'M',
        };
        changes.push(Change { rel: rel.to_string(), status });
    }
    changes.sort_by(|a, b| a.rel.cmp(&b.rel));
    changes
}

/// Unified diff of one file vs HEAD; synthesizes an all-added diff for
/// untracked files (and unborn HEAD). Never call on the event-loop thread.
pub fn diff_file(root: &Path, tracking: &Tracking, path: &Path) -> String {
    if let Ok(o) =
        git_cmd(root, tracking).args(["diff", "--no-color", "HEAD", "--"]).arg(path).output()
    {
        if o.status.success() && !o.stdout.is_empty() {
            return String::from_utf8_lossy(&o.stdout).into_owned();
        }
    }
    // Untracked / unborn HEAD. --no-index exits 1 when the files differ, so
    // gate on stdout only, not the exit status.
    match Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--no-color", "--no-index", "--", "/dev/null"])
        .arg(path)
        .output()
    {
        Ok(o) if !o.stdout.is_empty() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => "(no differences)".to_string(),
    }
}

/// Reverts every listed change back to the baseline: restores modified and
/// deleted files in one batch, removes added/untracked ones. Errors are
/// ignored — the follow-up status refresh shows the truth. Never call on the
/// event loop.
pub fn discard_all(root: &Path, tracking: &Tracking, changes: &[Change]) {
    let restore: Vec<&str> =
        changes.iter().filter(|c| c.status != 'A').map(|c| c.rel.as_str()).collect();
    if !restore.is_empty() {
        let mut cmd = git_cmd(root, tracking);
        cmd.args(["checkout", "HEAD", "--"]);
        for rel in restore {
            cmd.arg(rel);
        }
        let _ = cmd.status();
    }
    for change in changes.iter().filter(|c| c.status == 'A') {
        let _ = git_cmd(root, tracking).args(["reset", "-q", "HEAD", "--"]).arg(&change.rel).status();
        let _ = std::fs::remove_file(change.abs(root));
    }
}

/// Reverts one file to its last-committed (or snapshot) state. Errors are
/// ignored — the follow-up status refresh shows the truth. Never call on the
/// event loop.
pub fn discard(root: &Path, tracking: &Tracking, path: &Path, status: char) {
    if status == 'A' {
        // Unstage if staged (harmless otherwise), then remove from disk.
        let _ = git_cmd(root, tracking).args(["reset", "-q", "HEAD", "--"]).arg(path).status();
        let _ = std::fs::remove_file(path);
    } else {
        let _ = git_cmd(root, tracking).args(["checkout", "HEAD", "--"]).arg(path).status();
    }
}

// ----- the per-session worker -----

/// What one file's rollback should revert.
pub enum Revert {
    One(PathBuf, char),
    All(Vec<Change>),
}

/// A request to the session's git thread.
pub enum Job {
    /// Refresh the changes list. Latest-wins: a newer request replaces one
    /// still queued, since both would report the same tree.
    Status { generation: u64, rebaseline: bool },
    /// Diff one file vs HEAD. Latest-wins for the same reason.
    Diff { generation: u64, path: PathBuf },
    /// Roll back, then report the tree that leaves. Never coalesced.
    Revert { revert: Revert, generation: u64 },
}

/// What the git thread sends back. `generation` is the caller's; a session
/// drops any answer that is not from its newest request.
pub enum Done {
    Status(u64, Vec<Change>),
    Diff(u64, PathBuf, String),
}

/// One git thread per session, for the life of the session.
///
/// A thread per request used to be spawned on every 250 ms batch of file
/// system events. The generation counter dropped the stale *results*, not the
/// processes, so an agent writing files left a `git status` over the whole
/// tree running every quarter second — doubled when a second window watched
/// the same folder. Here the requests queue instead, and the ones that a
/// newer request has already answered are dropped before git ever runs.
///
/// Dropping the worker hangs up the channel, which ends the thread after its
/// current job. Nothing is joined: the caller is the event loop.
pub struct Worker {
    tx: Sender<Job>,
}

impl Worker {
    pub fn spawn(
        root: PathBuf,
        tracking: Tracking,
        on_done: impl Fn(Done) + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut pending_status: Option<(u64, bool)> = None;
                let mut pending_diff: Option<(u64, PathBuf)> = None;
                let mut reverts: Vec<(Revert, u64)> = Vec::new();
                queue(first, &mut pending_status, &mut pending_diff, &mut reverts);
                loop {
                    for job in rx.try_iter() {
                        queue(job, &mut pending_status, &mut pending_diff, &mut reverts);
                    }
                    // Rollbacks first: they change what a status would report,
                    // and the user is waiting on the file coming back.
                    if !reverts.is_empty() {
                        let (revert, generation) = reverts.remove(0);
                        with_repo(&root, || match &revert {
                            Revert::One(path, st) => discard(&root, &tracking, path, *st),
                            Revert::All(changes) => discard_all(&root, &tracking, changes),
                        });
                        // The session drops any answer older than its newest
                        // request, so never step the generation back: a
                        // refresh queued behind this rollback would have its
                        // result thrown away.
                        let generation =
                            pending_status.map_or(generation, |(g, _)| g.max(generation));
                        let force = pending_status.is_some_and(|(_, f)| f);
                        pending_status = Some((generation, force));
                        continue;
                    }
                    if let Some((generation, rebaseline)) = pending_status.take() {
                        let changes = with_repo(&root, || {
                            if let Tracking::Shadow(dir) = &tracking {
                                snapshot_baseline(&root, dir, rebaseline);
                            }
                            status(&root, &tracking)
                        });
                        on_done(Done::Status(generation, changes));
                        continue;
                    }
                    let Some((generation, path)) = pending_diff.take() else { break };
                    let text = with_repo(&root, || diff_file(&root, &tracking, &path));
                    on_done(Done::Diff(generation, path, text));
                }
            }
        });
        Self { tx }
    }

    /// Queues a job. Dropped silently once the thread is gone, which only
    /// happens when the worker itself is being dropped.
    pub fn send(&self, job: Job) {
        let _ = self.tx.send(job);
    }
}

fn queue(
    job: Job,
    pending_status: &mut Option<(u64, bool)>,
    pending_diff: &mut Option<(u64, PathBuf)>,
    reverts: &mut Vec<(Revert, u64)>,
) {
    match job {
        // Keep the newer generation, and any request to re-baseline: dropping
        // that would turn an explicit "watch from now" into a plain refresh.
        Job::Status { generation, rebaseline } => {
            let force = rebaseline || pending_status.is_some_and(|(_, f)| f);
            *pending_status = Some((generation, force));
        }
        Job::Diff { generation, path } => *pending_diff = Some((generation, path)),
        Job::Revert { revert, generation } => reverts.push((revert, generation)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A work tree and, beside it, the snapshot repo. Real snapshot repos sit
    /// under the config dir, never inside the folder they track — one inside
    /// would show up in its own `git status`.
    fn scratch(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("tigriden-git-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("work");
        std::fs::create_dir_all(&root).unwrap();
        (root, dir.join("snapshot.git"))
    }

    #[test]
    fn an_existing_baseline_survives_a_second_window_opening_the_folder() {
        let (root, git_dir) = scratch("baseline");
        std::fs::write(root.join("a.txt"), "one").unwrap();

        // First window: no baseline yet, so one is taken.
        snapshot_baseline(&root, &git_dir, false);
        assert!(has_baseline(&git_dir));
        let tracking = Tracking::Shadow(git_dir.clone());
        assert!(status(&root, &tracking).is_empty(), "a fresh baseline has no changes");

        // The agent edits the file; the panel offers to roll it back.
        std::fs::write(root.join("a.txt"), "two").unwrap();
        assert_eq!(status(&root, &tracking).len(), 1);

        // Second window opens the same folder. That must not re-snapshot:
        // doing so used to discard the change above, rollback and all.
        snapshot_baseline(&root, &git_dir, false);
        assert_eq!(status(&root, &tracking).len(), 1, "opening again wiped the baseline");

        // "Watch from now" still does re-snapshot.
        snapshot_baseline(&root, &git_dir, true);
        assert!(status(&root, &tracking).is_empty());
    }

    #[test]
    fn an_init_with_no_commit_is_not_a_baseline() {
        let (root, git_dir) = scratch("unborn");
        std::fs::create_dir_all(&git_dir).unwrap();
        // What an interrupted first snapshot leaves behind: HEAD on disk, no
        // commit under it.
        let _ = Command::new("git").arg("--git-dir").arg(&git_dir).args(["init", "-q"]).status();
        assert!(git_dir.join("HEAD").exists());
        assert!(!has_baseline(&git_dir), "HEAD existing is not a commit");

        std::fs::write(root.join("a.txt"), "one").unwrap();
        snapshot_baseline(&root, &git_dir, false);
        assert!(has_baseline(&git_dir), "the unfinished repo is finished, not skipped");
    }

    #[test]
    fn queued_refreshes_collapse_to_the_newest() {
        let mut status = None;
        let mut diff = None;
        let mut reverts = Vec::new();
        for generation in 1..=5 {
            let job = Job::Status { generation, rebaseline: false };
            queue(job, &mut status, &mut diff, &mut reverts);
        }
        // Five file system batches, one `git status`.
        assert_eq!(status, Some((5, false)));

        // A re-baseline anywhere in the run survives the collapse: it is an
        // explicit gesture, not a repeat of the same question.
        let forced = Job::Status { generation: 6, rebaseline: true };
        queue(forced, &mut status, &mut diff, &mut reverts);
        let plain = Job::Status { generation: 7, rebaseline: false };
        queue(plain, &mut status, &mut diff, &mut reverts);
        assert_eq!(status, Some((7, true)));
    }

    /// The status the worker runs after a rollback has to answer the newest
    /// request, or `changes_ready` drops it as stale and the panel keeps
    /// showing the file the user just rolled back.
    #[test]
    fn a_rollback_does_not_step_the_generation_back() {
        let mut status = Some((6u64, false));
        let revert_generation = 5u64;
        let generation = status.map_or(revert_generation, |(g, _)| g.max(revert_generation));
        let force = status.is_some_and(|(_, f)| f);
        status = Some((generation, force));
        assert_eq!(status, Some((6, false)));
    }

    #[test]
    fn rollbacks_queue_rather_than_collapse() {
        let mut status = None;
        let mut diff = None;
        let mut reverts = Vec::new();
        let all = Job::Revert { revert: Revert::All(Vec::new()), generation: 1 };
        queue(all, &mut status, &mut diff, &mut reverts);
        let one = Job::Revert { revert: Revert::One(PathBuf::from("a.txt"), 'M'), generation: 2 };
        queue(one, &mut status, &mut diff, &mut reverts);
        assert_eq!(reverts.len(), 2, "a rollback the user asked for is never dropped");
    }
}
