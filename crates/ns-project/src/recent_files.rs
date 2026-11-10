//! Per-project recent-file history (files opened in the editor, most recent
//! first). This is IDE state, not part of `project.toml`; it lives in
//! `.ns-ide/recent_files.json` so reopening a project offers a quick "recent
//! files" list even for files that are no longer open as tabs.
//!
//! Distinct from [`crate::recents`] (recent *projects*, global) — this tracks
//! files *inside* the current project.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::breakpoints::STATE_DIR;

pub const RECENT_FILES_FILE: &str = "recent_files.json";
/// Cap on how many files the history retains.
pub const MAX_RECENT_FILES: usize = 100;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecentFiles {
    /// Project-relative paths, most recent first. De-duplicated on push.
    #[serde(default)]
    pub files: Vec<PathBuf>,
}

pub fn recent_files_path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(RECENT_FILES_FILE)
}

impl RecentFiles {
    /// Load the recent-file history for a project. A missing/corrupt file
    /// yields an empty history (non-fatal).
    pub fn load(root: &Path) -> Self {
        let path = recent_files_path(root);
        let Ok(text) = fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Record `path` (relative to `root`) as just-opened: move it to the front
    /// of the history, de-duplicating and trimming to [`MAX_RECENT_FILES`].
    pub fn push(&mut self, path: PathBuf) {
        self.files.retain(|existing| existing != &path);
        self.files.insert(0, path);
        self.files.truncate(MAX_RECENT_FILES);
    }

    /// Persist the history to `.ns-ide/recent_files.json`.
    pub fn save(&self, root: &Path) -> Result<()> {
        let dir = root.join(STATE_DIR);
        fs::create_dir_all(&dir).with_context(|| format!("创建 {}", dir.display()))?;
        let json = serde_json::to_string_pretty(self)?;
        fs::write(recent_files_path(root), json).context("写入最近文件历史")
    }

    /// Drop files that no longer exist on disk (they may have been renamed or
    /// deleted since the history was recorded). Returns the pruned list.
    pub fn retain_existing(&mut self, root: &Path) {
        self.files.retain(|path| root.join(path).is_file());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root(test_name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ns-recent-files-test-{}-{test_name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn push_moves_to_front_and_dedupes() {
        let root = fixture_root("push_moves_to_front_and_dedupes");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.c"), "int main() {}\n").unwrap();
        fs::write(root.join("src/timer.c"), "void t() {}\n").unwrap();

        let mut recents = RecentFiles::default();
        recents.push(PathBuf::from("src/main.c"));
        recents.push(PathBuf::from("src/timer.c"));
        // Re-opening main.c moves it back to the front without duplicating.
        recents.push(PathBuf::from("src/main.c"));
        assert_eq!(
            recents.files,
            vec![PathBuf::from("src/main.c"), PathBuf::from("src/timer.c")]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn save_load_round_trips_and_prunes_missing() {
        let root = fixture_root("save_load_round_trips");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.c"), "int main() {}\n").unwrap();
        fs::write(root.join("src/gone.c"), "int gone;\n").unwrap();

        let mut recents = RecentFiles::default();
        recents.push(PathBuf::from("src/main.c"));
        recents.push(PathBuf::from("src/gone.c"));
        recents.save(&root).unwrap();

        let loaded = RecentFiles::load(&root);
        assert_eq!(loaded.files.len(), 2);

        // Delete one file and prune: the missing entry drops out.
        fs::remove_file(root.join("src/gone.c")).unwrap();
        let mut pruned = loaded;
        pruned.retain_existing(&root);
        assert_eq!(pruned.files, vec![PathBuf::from("src/main.c")]);
        let _ = fs::remove_dir_all(root);
    }
}
