//! Session persistence: which files were open (and which was active) when the
//! project was last closed. This is IDE state, not part of `project.toml`; it
//! lives in `.ns-ide/session.json` so reopening a project restores the editor.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::breakpoints::STATE_DIR;

pub const SESSION_FILE: &str = "session.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Open file paths (relative to the project root), in tab order.
    pub open_files: Vec<PathBuf>,
    /// Index into `open_files` of the last active tab.
    pub active: Option<usize>,
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
        };
        session.save(&root).unwrap();

        let loaded = Session::load(&root);
        assert_eq!(loaded, session);

        // Missing file loads as default (empty session).
        let missing = Session::load(&root.join("nope"));
        assert_eq!(missing, Session::default());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
