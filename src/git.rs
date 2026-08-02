use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Creates the shadow repo if missing and commits the folder's current state
/// as the baseline everything is compared against. Never call on the event
/// loop — the first snapshot of a big folder takes a while.
pub fn snapshot_baseline(root: &Path, dir: &Path) {
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
