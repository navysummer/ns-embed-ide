//! User-defined external tools / custom shell commands.
//!
//! Tools are project-local configuration stored in `.ns-ide/tools.json`.
//! Each tool is a title plus a shell command (run via `sh -c` / `cmd /C`).
//! The IDE offers them from the command palette and runs them with streaming
//! output in the「工具」output tab.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const STATE_DIR: &str = ".ns-ide";
pub const TOOLS_FILE: &str = "tools.json";

/// One user-defined external tool command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolCommand {
    /// Stable id used to identify the tool (and keep config order).
    pub id: String,
    /// Human-readable title shown in the palette and the tool tab.
    pub title: String,
    /// Shell command line (run via `sh -c` / `cmd /C`).
    pub command: String,
    /// Working directory. Empty = project root.
    pub cwd: String,
}

impl Default for ToolCommand {
    fn default() -> Self {
        Self {
            id: String::new(),
            title: String::new(),
            command: String::new(),
            cwd: String::new(),
        }
    }
}

/// The persisted tool list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolStore {
    pub tools: Vec<ToolCommand>,
}

pub fn tools_path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(TOOLS_FILE)
}

impl ToolStore {
    pub fn load(root: &Path) -> Self {
        let path = tools_path(root);
        let Ok(text) = fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = tools_path(root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录 {} 失败", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).context("序列化工具配置失败")?;
        fs::write(&path, text).with_context(|| format!("写入 {} 失败", path.display()))
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Upsert a tool by id (insert if absent). Returns `true` if a new tool
    /// was added, `false` if an existing one was replaced.
    pub fn upsert(&mut self, tool: ToolCommand) -> bool {
        if let Some(existing) = self.tools.iter_mut().find(|t| t.id == tool.id) {
            *existing = tool;
            false
        } else {
            self.tools.push(tool);
            true
        }
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.tools.len();
        self.tools.retain(|t| t.id != id);
        self.tools.len() != before
    }

    pub fn find(&self, id: &str) -> Option<&ToolCommand> {
        self.tools.iter().find(|t| t.id == id)
    }

    pub fn tools(&self) -> &[ToolCommand] {
        &self.tools
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
            "ns-tools-test-{}-{nanos}-{seq}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn tool(id: &str, title: &str, command: &str) -> ToolCommand {
        ToolCommand {
            id: id.to_string(),
            title: title.to_string(),
            command: command.to_string(),
            cwd: String::new(),
        }
    }

    #[test]
    fn upsert_adds_and_replaces() {
        let mut store = ToolStore::default();
        assert!(store.upsert(tool("a", "Build", "make")));
        assert!(store.upsert(tool("b", "Clean", "make clean")));
        assert_eq!(store.len(), 2);
        assert!(!store.upsert(tool("a", "Build All", "make -j")));
        assert_eq!(store.len(), 2);
        assert_eq!(store.find("a").unwrap().title, "Build All");
    }

    #[test]
    fn remove_and_find() {
        let mut store = ToolStore::default();
        store.upsert(tool("a", "A", "echo a"));
        assert!(store.remove("a"));
        assert!(!store.remove("a"));
        assert!(store.find("a").is_none());
    }

    #[test]
    fn save_load_round_trip() {
        let root = tmp_root();
        let mut store = ToolStore::default();
        store.upsert(tool("a", "Build", "make -j4"));
        store.upsert(tool("b", "Flash", "openocd -f board.cfg"));
        store.save(&root).unwrap();
        let loaded = ToolStore::load(&root);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.find("a").unwrap().command, "make -j4");
        assert_eq!(loaded.find("b").unwrap().cwd, "");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn load_missing_is_default() {
        let root = tmp_root();
        assert!(ToolStore::load(&root).is_empty());
        let _ = fs::remove_dir_all(&root);
    }
}
