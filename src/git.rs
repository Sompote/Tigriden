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

/// Generated directories a snapshot never needs, skipped by the walk and
/// written into the repo's `info/exclude`.
const SKIP_DIRS: [&str; 6] =
    ["node_modules", "target", "dist", "build", ".venv", "__pycache__"];

/// A file this size or larger stays out of the snapshot. Catches the one-off
/// giant: a 309 MB `weight.zip`, a 156 MB `.engine`, a 323 MB database.
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// A directory subtree this big is excluded whole, at the shallowest point
/// that crosses the line.
///
/// The per-file cap alone does not hold the store down, because the mass is
/// never in the giants. One tracked folder carried 4602 PDFs averaging 1.7 MB
/// — a scraped literature corpus — and another 7638 JPGs averaging 0.65 MB, a
/// training set. Both sit under any per-file cap worth setting, and together
/// they were 13 of the 17 GB. What marks them is the directory: a manuscript
/// folder does not hold 4600 PDFs. Excluding the subtree takes those two
/// folders to 152 MB and 65 MB, while a paper's `figures/` is nowhere near
/// the line.
const MAX_DIR_BYTES: u64 = 200 * 1024 * 1024;

/// What a walk of the work tree found: a subtree's size, the oversized files
/// directly relevant to it, and the same for each child directory.
struct Scan {
    /// Repo-relative, slash-separated, `""` for the root.
    rel: String,
    bytes: u64,
    files: Vec<String>,
    dirs: Vec<Scan>,
}

/// Measures the work tree, skipping [`SKIP_DIRS`], hidden directories and
/// symlinks — a symlinked directory would both cost a second pass over its
/// contents and let a cycle run forever.
fn scan(dir: &Path, rel: &str) -> Scan {
    let mut node = Scan { rel: rel.to_string(), bytes: 0, files: Vec::new(), dirs: Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return node };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // read_dir's file type does not follow the link, which is what we
        // want: a symlink is never descended and never measured.
        let Ok(kind) = entry.file_type() else { continue };
        let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if kind.is_dir() {
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let sub = scan(&entry.path(), &child);
            node.bytes += sub.bytes;
            node.dirs.push(sub);
        } else if kind.is_file() {
            let Ok(size) = entry.metadata().map(|m| m.len()) else { continue };
            node.bytes += size;
            if size >= MAX_FILE_BYTES {
                node.files.push(child);
            }
        }
    }
    node
}

/// The paths to keep out of the snapshot: whole directories first, then the
/// oversized files left over outside them.
///
/// A directory over budget is taken at the shallowest point, so one line
/// covers the subtree and the walk below it never reaches the exclude file.
/// The root is never excluded, however big it is — a folder that big still
/// gets its own small files tracked.
fn to_exclude(root: &Path) -> (Vec<String>, Vec<String>) {
    fn collect(node: Scan, dirs: &mut Vec<String>, files: &mut Vec<String>) {
        if !node.rel.is_empty() && node.bytes >= MAX_DIR_BYTES {
            dirs.push(node.rel);
            return;
        }
        files.extend(node.files);
        for child in node.dirs {
            collect(child, dirs, files);
        }
    }
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    collect(scan(root, ""), &mut dirs, &mut files);
    dirs.sort();
    files.sort();
    (dirs, files)
}

/// One `info/exclude` line: anchored at the repo root, with the characters
/// gitignore reads as syntax escaped.
fn exclude_line(rel: &str) -> String {
    let mut line = String::with_capacity(rel.len() + 2);
    line.push('/');
    for ch in rel.chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | '\\' | '!' | '#' | ' ') {
            line.push('\\');
        }
        line.push(ch);
    }
    line
}

/// Writes the repo's `info/exclude`: the generated directories, then whatever
/// was over budget when the baseline was taken.
fn write_excludes(dir: &Path, big_dirs: &[String], big_files: &[String]) {
    let mut text = String::new();
    for name in SKIP_DIRS {
        text.push_str(name);
        text.push_str("/\n");
    }
    text.push_str(".DS_Store\n");
    if !big_dirs.is_empty() {
        text.push_str("\n# Data directories, too big to snapshot.\n");
        for rel in big_dirs {
            text.push_str(&exclude_line(rel));
            text.push_str("/\n");
        }
    }
    if !big_files.is_empty() {
        text.push_str("\n# Single files over the size cap.\n");
        for rel in big_files {
            text.push_str(&exclude_line(rel));
            text.push('\n');
        }
    }
    let _ = std::fs::create_dir_all(dir.join("info"));
    let _ = std::fs::write(dir.join("info/exclude"), text);
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
    }
    let tracking = Tracking::Shadow(dir.to_path_buf());
    let (big_dirs, big_files) = to_exclude(root);
    write_excludes(dir, &big_dirs, &big_files);
    let big: Vec<&String> = big_dirs.iter().chain(big_files.iter()).collect();
    // An exclude only holds an untracked file back. One that grew past the cap
    // since the last baseline is already in the index, so drop it from there —
    // the commit below records that as a deletion, and the file itself is not
    // touched. Batched, because a folder can hold more paths than a command
    // line takes.
    for batch in big.chunks(64) {
        let _ = git_cmd(root, &tracking)
            .args(["rm", "--cached", "-r", "-q", "--ignore-unmatch", "--"])
            .args(batch)
            .status();
    }
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
    // Nothing else ever packs these repos, and a baseline writes one loose
    // object per file. The default threshold is 6700, high enough that a
    // 5499-object repo sat unpacked; ask for the sweep sooner. `--auto`
    // detaches and returns, so this costs the worker thread nothing.
    let _ = Command::new("git")
        .arg("--git-dir")
        .arg(dir)
        .args(["-c", "gc.auto=512", "gc", "--auto", "--quiet"])
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
    fn a_file_over_the_cap_stays_out_of_the_snapshot() {
        let (root, git_dir) = scratch("oversize");
        std::fs::create_dir_all(root.join("weight")).unwrap();
        std::fs::write(root.join("paper.tex"), "\\documentclass{article}").unwrap();
        std::fs::write(root.join("weight/model.pt"), vec![0u8; MAX_FILE_BYTES as usize]).unwrap();

        snapshot_baseline(&root, &git_dir, false);
        let tracking = Tracking::Shadow(git_dir.clone());
        let tracked = Command::new("git")
            .arg("--git-dir")
            .arg(&git_dir)
            .args(["ls-tree", "-r", "--name-only", "HEAD"])
            .output()
            .unwrap();
        let tracked = String::from_utf8_lossy(&tracked.stdout);
        assert!(tracked.contains("paper.tex"), "the manuscript is tracked");
        assert!(!tracked.contains("model.pt"), "the oversized weight was snapshotted");

        // And it stays out of the panel rather than showing as an eternal add.
        std::fs::write(root.join("weight/model.pt"), vec![1u8; MAX_FILE_BYTES as usize]).unwrap();
        assert!(status(&root, &tracking).is_empty(), "the excluded file reached the panel");
    }

    #[test]
    fn a_file_that_grew_past_the_cap_leaves_the_index() {
        let (root, git_dir) = scratch("grew");
        std::fs::write(root.join("run.csv"), "a,b\n1,2\n").unwrap();
        snapshot_baseline(&root, &git_dir, false);

        // The run appends until the file is over the cap; "watch from now"
        // must drop it rather than carry a 5 MB blob into every later commit.
        std::fs::write(root.join("run.csv"), vec![b'x'; MAX_FILE_BYTES as usize]).unwrap();
        snapshot_baseline(&root, &git_dir, true);

        let tracked = Command::new("git")
            .arg("--git-dir")
            .arg(&git_dir)
            .args(["ls-tree", "-r", "--name-only", "HEAD"])
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&tracked.stdout).contains("run.csv"),
            "the grown file stayed in the index"
        );
        assert!(status(&root, &Tracking::Shadow(git_dir)).is_empty());
    }

    #[test]
    fn a_data_directory_over_budget_is_excluded_whole() {
        let (root, git_dir) = scratch("corpus");
        std::fs::write(root.join("paper.tex"), "\\documentclass{article}").unwrap();
        std::fs::create_dir_all(root.join("figures")).unwrap();
        std::fs::write(root.join("figures/fig1.pdf"), vec![0u8; 2 * 1024 * 1024]).unwrap();
        // A corpus of files each well under the per-file cap. Together they
        // are what actually filled the store.
        std::fs::create_dir_all(root.join("harvest/pdfs")).unwrap();
        for i in 0..64 {
            let chunk = vec![i as u8; 4 * 1024 * 1024];
            std::fs::write(root.join(format!("harvest/pdfs/{i}.pdf")), chunk).unwrap();
        }

        snapshot_baseline(&root, &git_dir, false);
        let out = Command::new("git")
            .arg("--git-dir")
            .arg(&git_dir)
            .args(["ls-tree", "-r", "--name-only", "HEAD"])
            .output()
            .unwrap();
        let tracked = String::from_utf8_lossy(&out.stdout);
        assert!(tracked.contains("paper.tex"), "the manuscript is tracked");
        assert!(tracked.contains("figures/fig1.pdf"), "a paper's own figure is tracked");
        assert!(!tracked.contains("harvest/"), "the corpus was snapshotted file by file");

        // Adding to the corpus does not fill the panel either.
        std::fs::write(root.join("harvest/pdfs/new.pdf"), vec![7u8; 1024]).unwrap();
        assert!(status(&root, &Tracking::Shadow(git_dir)).is_empty());
    }

    #[test]
    fn an_exclude_line_escapes_gitignore_syntax() {
        assert_eq!(exclude_line("weight/model.pt"), "/weight/model.pt");
        assert_eq!(exclude_line("runs/log[1].txt"), "/runs/log\\[1\\].txt");
        assert_eq!(exclude_line("a b/c*.bin"), "/a\\ b/c\\*.bin");
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
