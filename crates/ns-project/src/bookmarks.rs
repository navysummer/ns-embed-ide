//! Project-local source bookmarks (F2 / gutter toggles).
//!
//! Bookmarks are IDE state, not part of the authoritative `project.toml`.
//! They live in `.ns-ide/bookmarks.json` so they survive restarts without
//! polluting the user's toolchain configuration.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub const STATE_DIR: &str = ".ns-ide";
pub const BOOKMARKS_FILE: &str = "bookmarks.json";

/// A single source bookmark: a file path (project-relative) plus a 1-based
/// line number and an optional user label.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Bookmark {
    pub path: String,
    pub line: usize,
    pub label: String,
    /// Monotonic creation order, used to keep the list stable and to allow
    /// restoring relative order after a load/save round trip.
    pub seq: u64,
}

impl Default for Bookmark {
    fn default() -> Self {
        Self {
            path: String::new(),
            line: 0,
            label: String::new(),
            seq: 0,
        }
    }
}

impl Bookmark {
    /// A short one-line description for panels and status messages.
    pub fn label_text(&self) -> String {
        if self.label.is_empty() {
            format!("{}:{}", self.path, self.line)
        } else {
            format!("{}:{} {}", self.path, self.line, self.label)
        }
    }
}

/// The bookmark store, persisted to `.ns-ide/bookmarks.json`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct BookmarkStore {
    pub bookmarks: Vec<Bookmark>,
    #[serde(skip)]
    next_seq: u64,
}

pub fn bookmarks_path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(BOOKMARKS_FILE)
}

impl BookmarkStore {
    pub fn load(root: &Path) -> Self {
        let path = bookmarks_path(root);
        let Ok(text) = fs::read_to_string(&path) else {
            return Self::default();
        };
        let mut store: BookmarkStore = serde_json::from_str(&text).unwrap_or_default();
        store.rebuild_seq();
        store
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = bookmarks_path(root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录 {} 失败", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).context("序列化书签失败")?;
        fs::write(&path, text).with_context(|| format!("写入 {} 失败", path.display()))
    }

    fn rebuild_seq(&mut self) {
        self.next_seq = self
            .bookmarks
            .iter()
            .map(|b| b.seq)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
    }

    fn next_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        seq
    }

    pub fn len(&self) -> usize {
        self.bookmarks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bookmarks.is_empty()
    }

    pub fn all(&self) -> &[Bookmark] {
        &self.bookmarks
    }

    /// All bookmarks for `path`, sorted by line.
    pub fn for_path(&self, path: &str) -> Vec<&Bookmark> {
        let mut v: Vec<&Bookmark> = self
            .bookmarks
            .iter()
            .filter(|b| b.path == path)
            .collect();
        v.sort_by_key(|b| b.line);
        v
    }

    pub fn find(&self, path: &str, line: usize) -> Option<&Bookmark> {
        self.bookmarks
            .iter()
            .find(|b| b.path == path && b.line == line)
    }

    /// Toggle the bookmark at `(path, line)`. Returns `true` if it was added,
    /// `false` if it was removed.
    pub fn toggle(&mut self, path: &str, line: usize) -> bool {
        if let Some(pos) = self
            .bookmarks
            .iter()
            .position(|b| b.path == path && b.line == line)
        {
            self.bookmarks.remove(pos);
            false
        } else {
            let seq = self.next_seq();
            self.bookmarks.push(Bookmark {
                path: path.to_string(),
                line,
                label: String::new(),
                seq,
            });
            true
        }
    }

    pub fn remove(&mut self, path: &str, line: usize) -> bool {
        let before = self.bookmarks.len();
        self.bookmarks
            .retain(|b| !(b.path == path && b.line == line));
        self.bookmarks.len() != before
    }

    pub fn set_label(&mut self, path: &str, line: usize, label: &str) -> bool {
        let Some(b) = self
            .bookmarks
            .iter_mut()
            .find(|b| b.path == path && b.line == line)
        else {
            return false;
        };
        b.label = label.to_string();
        true
    }

    pub fn clear(&mut self) {
        self.bookmarks.clear();
    }

    /// Remove bookmarks whose file no longer exists under `root`. Returns the
    /// number of bookmarks pruned.
    pub fn prune_missing(&mut self, root: &Path) -> usize {
        let before = self.bookmarks.len();
        self.bookmarks.retain(|b| root.join(&b.path).is_file());
        before - self.bookmarks.len()
    }
    pub fn add(&mut self, path: &str, line: usize, label: &str) -> bool {
        if self
            .bookmarks
            .iter()
            .any(|b| b.path == path && b.line == line)
        {
            return false;
        }
        let seq = self.next_seq();
        self.bookmarks.push(Bookmark {
            path: path.to_string(),
            line,
            label: label.to_string(),
            seq,
        });
        true
    }

    /// Group bookmarks by file path (preserving first-seen file order), for
    /// the bookmarks panel. Returns `(path, Vec<&Bookmark>)` groups.
    pub fn grouped(&self) -> Vec<(String, Vec<&Bookmark>)> {
        let mut order: Vec<String> = Vec::new();
        let mut map: BTreeMap<String, Vec<&Bookmark>> = BTreeMap::new();
        for b in &self.bookmarks {
            if !map.contains_key(&b.path) {
                order.push(b.path.clone());
            }
            map.entry(b.path.clone()).or_default().push(b);
        }
        order
            .into_iter()
            .map(|path| {
                let mut v = map.remove(&path).unwrap_or_default();
                v.sort_by_key(|b| b.line);
                (path, v)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TMP_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn tmp_root() -> PathBuf {
        let seq = TMP_SEQ.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ns-bookmarks-test-{}-{nanos}-{seq}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn toggle_adds_and_removes() {
        let mut store = BookmarkStore::default();
        assert!(store.toggle("src/main.c", 10));
        assert_eq!(store.len(), 1);
        assert!(store.find("src/main.c", 10).is_some());
        assert!(!store.toggle("src/main.c", 10));
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn add_returns_false_when_present() {
        let mut store = BookmarkStore::default();
        assert!(store.add("src/main.c", 5, "init"));
        assert!(!store.add("src/main.c", 5, "again"));
        assert_eq!(store.for_path("src/main.c").len(), 1);
    }

    #[test]
    fn set_label_and_remove() {
        let mut store = BookmarkStore::default();
        store.add("src/app.rs", 42, "");
        assert!(store.set_label("src/app.rs", 42, "main loop"));
        assert_eq!(store.find("src/app.rs", 42).unwrap().label, "main loop");
        assert!(store.remove("src/app.rs", 42));
        assert!(store.find("src/app.rs", 42).is_none());
    }

    #[test]
    fn grouped_groups_by_file_sorted_by_line() {
        let mut store = BookmarkStore::default();
        store.add("src/b.rs", 30, "");
        store.add("src/a.rs", 5, "");
        store.add("src/b.rs", 10, "");
        let groups = store.grouped();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "src/b.rs");
        assert_eq!(groups[0].1[0].line, 10);
        assert_eq!(groups[0].1[1].line, 30);
        assert_eq!(groups[1].0, "src/a.rs");
    }

    #[test]
    fn save_load_round_trip() {
        let root = tmp_root();
        let mut store = BookmarkStore::default();
        store.add("src/main.c", 10, "first");
        store.add("src/main.c", 20, "");
        store.add("src/uart.c", 7, "rx");
        store.save(&root).unwrap();
        let loaded = BookmarkStore::load(&root);
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded.find("src/main.c", 10).unwrap().label, "first");
        assert_eq!(loaded.find("src/uart.c", 7).unwrap().label, "rx");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn load_missing_file_is_default() {
        let root = tmp_root();
        let store = BookmarkStore::load(&root);
        assert!(store.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_missing_removes_deleted_files() {
        let root = tmp_root();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.c"), "int x;\n").unwrap();
        let mut store = BookmarkStore::default();
        store.add("src/main.c", 1, "");
        store.add("src/gone.c", 1, "");
        assert_eq!(store.prune_missing(&root), 1);
        assert_eq!(store.len(), 1);
        let _ = fs::remove_dir_all(&root);
    }
}
