use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

/// A flattened row handed to the Slint model. `kind`: 0 = session header,
/// 1 = directory, 2 = file.
#[derive(Debug, Clone)]
pub struct FlatRow {
    pub kind: i32,
    pub indent: i32,
    pub name: String,
    pub expanded: bool,
    pub path: PathBuf,
}

pub struct TreeState {
    root: PathBuf,
    children: HashMap<PathBuf, Vec<Entry>>,
    expanded: HashSet<PathBuf>,
    gitignore: Gitignore,
}

impl TreeState {
    pub fn new(root: PathBuf) -> Self {
        let mut builder = GitignoreBuilder::new(&root);
        builder.add(root.join(".gitignore"));
        let gitignore = builder.build().unwrap_or_else(|_| Gitignore::empty());
        Self { root, children: HashMap::new(), expanded: HashSet::new(), gitignore }
    }

    fn hidden(&self, path: &Path, is_dir: bool) -> bool {
        if path.file_name().is_some_and(|n| n == ".git") {
            return true;
        }
        self.gitignore.matched_path_or_any_parents(path, is_dir).is_ignore()
    }

    fn list(&mut self, dir: &Path) -> &[Entry] {
        if !self.children.contains_key(dir) {
            let mut entries: Vec<Entry> = std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| {
                    let path = e.path();
                    let is_dir = e.file_type().ok()?.is_dir();
                    if self.hidden(&path, is_dir) {
                        return None;
                    }
                    let name = path.file_name()?.to_string_lossy().into_owned();
                    Some(Entry { path, name, is_dir })
                })
                .collect();
            entries.sort_by(|a, b| {
                b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            self.children.insert(dir.to_path_buf(), entries);
        }
        self.children.get(dir).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn toggle(&mut self, dir: &Path) {
        if !self.expanded.remove(dir) {
            self.expanded.insert(dir.to_path_buf());
        }
    }

    pub fn is_expanded(&self, dir: &Path) -> bool {
        self.expanded.contains(dir)
    }

    /// Drops the cached listings for `dirs` so the next flatten re-reads them.
    pub fn invalidate(&mut self, dirs: impl IntoIterator<Item = PathBuf>) {
        for dir in dirs {
            self.children.remove(&dir);
        }
    }

    /// Flattens the expanded tree into rows, root itself excluded (the session
    /// header row is added by the caller).
    pub fn flatten(&mut self) -> Vec<FlatRow> {
        let mut rows = Vec::new();
        self.walk(&self.root.clone(), 1, &mut rows);
        rows
    }

    fn walk(&mut self, dir: &Path, depth: i32, rows: &mut Vec<FlatRow>) {
        for entry in self.list(dir).to_vec() {
            rows.push(FlatRow {
                kind: if entry.is_dir { 1 } else { 2 },
                indent: depth,
                name: entry.name.clone(),
                expanded: entry.is_dir && self.is_expanded(&entry.path),
                path: entry.path.clone(),
            });
            if entry.is_dir && self.is_expanded(&entry.path) {
                self.walk(&entry.path, depth + 1, rows);
            }
        }
    }
}
