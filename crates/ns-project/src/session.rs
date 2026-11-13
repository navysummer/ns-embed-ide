//! Session persistence: which files were open (and which was active) when the
//! project was last closed. This is IDE state, not part of `project.toml`; it
//! lives in `.ns-ide/session.json` so reopening a project restores the editor.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::breakpoints::STATE_DIR;

pub const SESSION_FILE: &str = "session.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// Open file paths (relative to the project root), in tab order.
    pub open_files: Vec<PathBuf>,
    /// Index into `open_files` of the last active tab.
    pub active: Option<usize>,
    /// Per-file folded regions, keyed by project-relative path. Restored so
    /// reopening a project keeps the editor's fold state.
    pub folded: Vec<FoldedFile>,
    /// Per-file editor view state (cursor + scroll), keyed by path.
    #[serde(default)]
    pub views: Vec<FileView>,
    /// Unix timestamp (seconds) when the session was last saved, for
    /// crash-recovery ordering. 0 when unset.
    #[serde(default)]
    pub saved_at: u64,
    /// Project-relative path of the file shown in the split editor pane
    /// (if any), restored on reopen. `None` when the split view was closed.
    #[serde(default)]
    pub split: Option<PathBuf>,
}

/// Editor view state for one file, restored from the session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileView {
    /// Project-relative path of the file.
    pub path: PathBuf,
    /// 1-based cursor (line, column).
    pub cursor: Option<(usize, usize)>,
    /// Vertical scroll position as a 0.0-1.0 ratio of the document height.
    #[serde(default)]
    pub scroll_ratio: f32,
    /// Editor zoom factor (1.0 = default), restored with the view.
    #[serde(default)]
    pub zoom: f32,
    /// Editor indent width in spaces (2/4/8), restored with the view.
    #[serde(default)]
    pub indent_width: usize,
    /// Whether soft word wrap was enabled for this view.
    #[serde(default)]
    pub word_wrap: bool,
 }

/// Folded regions for one file, restored from the session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldedFile {
    /// Project-relative path of the file.
    pub path: PathBuf,
    /// Folded (start line, end line) pairs, 0-based inclusive.
    pub regions: Vec<(usize, usize)>,
}

pub fn session_path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(SESSION_FILE)
}

impl Session {
    pub fn load(root: &Path) -> Self {
        let Ok(text) = fs::read_to_string(&session_path(root)) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = session_path(root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录 {} 失败", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).context("序列化会话失败")?;
        fs::write(&path, text).with_context(|| format!("写入 {} 失败", path.display()))
    }

    /// Save with `saved_at` stamped to the current wall-clock time, so crash
    /// recovery can tell the freshest snapshot apart from an older one.
    pub fn save_stamped(&mut self, root: &Path) -> Result<()> {
        self.saved_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        self.save(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("ns-session-test-{}", std::process::id()));
        let root = dir.join("proj");
        std::fs::create_dir_all(&root).unwrap();

        let session = Session {
            open_files: vec![PathBuf::from("src/main.c"), PathBuf::from("src/lib.rs")],
            active: Some(1),
            folded: vec![FoldedFile {
                path: PathBuf::from("src/main.c"),
                regions: vec![(0, 5), (8, 12)],
            }],
            views: vec![FileView {
                path: PathBuf::from("src/main.c"),
                cursor: Some((12, 4)),
                scroll_ratio: 0.42,
                zoom: 1.25,
                indent_width: 2,
                word_wrap: true,
            }],
            saved_at: 0,
            split: Some(PathBuf::from("src/startup.s")),
        };
        session.save(&root).unwrap();

        let loaded = Session::load(&root);
        assert_eq!(loaded, session);

        // Missing file loads as default (empty session).
        let missing = Session::load(&root.join("nope"));
        assert_eq!(missing, Session::default());

        // `save_stamped` records a wall-clock timestamp for crash recovery.
        let mut stamped = Session::default();
        stamped.save_stamped(&root).unwrap();
        let reloaded = Session::load(&root);
        assert!(reloaded.saved_at > 0, "save_stamped must set saved_at");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
