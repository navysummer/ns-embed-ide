//! Project-local breakpoint state.
//!
//! Breakpoints are IDE state, not part of the authoritative `project.toml`.
//! They live in `.ns-ide/breakpoints.json` so they survive restarts without
//! polluting the user's toolchain configuration.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const STATE_DIR: &str = ".ns-ide";
pub const BREAKPOINTS_FILE: &str = "breakpoints.json";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakpointKind {
    #[default]
    Line,
    Conditional,
    Log,
    Hardware,
}

impl BreakpointKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Line => "行断点",
            Self::Conditional => "条件断点",
            Self::Log => "日志断点",
            Self::Hardware => "硬件断点",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Breakpoint {
    /// Path relative to the project root, using forward slashes.
    pub path: String,
    /// 1-based source line.
    pub line: usize,
    pub enabled: bool,
    pub kind: BreakpointKind,
    pub condition: String,
}

impl Default for Breakpoint {
    fn default() -> Self {
        Self {
            path: String::new(),
            line: 1,
            enabled: true,
            kind: BreakpointKind::Line,
            condition: String::new(),
        }
    }
}

impl Breakpoint {
    pub fn label(&self) -> String {
        format!("{}:{}", self.path, self.line)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BreakpointStore {
    pub breakpoints: Vec<Breakpoint>,
}

impl BreakpointStore {
    pub fn load(root: &Path) -> Self {
        let path = breakpoints_path(root);
        let Ok(text) = fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = breakpoints_path(root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录 {} 失败", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).context("序列化断点失败")?;
        fs::write(&path, text).with_context(|| format!("写入 {} 失败", path.display()))
    }

    pub fn len(&self) -> usize {
        self.breakpoints.len()
    }

    pub fn is_empty(&self) -> bool {
        self.breakpoints.is_empty()
    }

    pub fn all(&self) -> &[Breakpoint] {
        &self.breakpoints
    }

    pub fn find(&self, path: &str, line: usize) -> Option<&Breakpoint> {
        self.breakpoints
            .iter()
            .find(|breakpoint| breakpoint.path == path && breakpoint.line == line)
    }

    /// Add a breakpoint if absent, remove it if present. Returns `true` when
    /// the breakpoint now exists.
    pub fn toggle(&mut self, path: &str, line: usize) -> bool {
        if let Some(index) = self.position(path, line) {
            self.breakpoints.remove(index);
            false
        } else {
            self.breakpoints.push(Breakpoint {
                path: path.to_string(),
                line,
                ..Breakpoint::default()
            });
            self.sort();
            true
        }
    }

    pub fn set(&mut self, path: &str, line: usize, enabled: bool) -> bool {
        if let Some(breakpoint) = self
            .breakpoints
            .iter_mut()
            .find(|breakpoint| breakpoint.path == path && breakpoint.line == line)
        {
            breakpoint.enabled = enabled;
            true
        } else if enabled {
            self.toggle(path, line)
        } else {
            false
        }
    }

    /// Update (or create) a breakpoint's condition. A non-empty condition makes it a
    /// conditional breakpoint; clearing it reverts to a plain line breakpoint.
    pub fn set_condition(&mut self, path: &str, line: usize, condition: &str) -> bool {
        let condition = condition.trim().to_string();
        if let Some(breakpoint) = self
            .breakpoints
            .iter_mut()
            .find(|breakpoint| breakpoint.path == path && breakpoint.line == line)
        {
            breakpoint.enabled = true;
            breakpoint.condition = condition.clone();
            breakpoint.kind = if condition.is_empty() {
                BreakpointKind::Line
            } else {
                BreakpointKind::Conditional
            };
            true
        } else if condition.is_empty() {
            false
        } else {
            self.breakpoints.push(Breakpoint {
                path: path.to_string(),
                line,
                enabled: true,
                kind: BreakpointKind::Conditional,
                condition,
            });
            self.sort();
            true
        }
    }

    pub fn remove(&mut self, path: &str, line: usize) -> bool {
        if let Some(index) = self.position(path, line) {
            self.breakpoints.remove(index);
            true
        } else {
            false
        }
    }

    pub fn remove_at(&mut self, index: usize) -> Option<Breakpoint> {
        (index < self.breakpoints.len()).then(|| self.breakpoints.remove(index))
    }

    pub fn clear(&mut self) {
        self.breakpoints.clear();
    }

    pub fn clear_file(&mut self, path: &str) {
        self.breakpoints.retain(|breakpoint| breakpoint.path != path);
    }

    pub fn lines(&self, path: &str) -> BTreeSet<usize> {
        self.breakpoints
            .iter()
            .filter(|breakpoint| breakpoint.path == path && breakpoint.enabled)
            .map(|breakpoint| breakpoint.line)
            .collect()
    }

    pub fn enabled_count(&self) -> usize {
        self.breakpoints
            .iter()
            .filter(|breakpoint| breakpoint.enabled)
            .count()
    }

    fn position(&self, path: &str, line: usize) -> Option<usize> {
        self.breakpoints
            .iter()
            .position(|breakpoint| breakpoint.path == path && breakpoint.line == line)
    }

    fn sort(&mut self) {
        self.breakpoints
            .sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    }
}

pub fn breakpoints_path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(BREAKPOINTS_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_adds_and_removes() {
        let mut store = BreakpointStore::default();
        assert!(store.toggle("src/main.c", 12));
        assert_eq!(store.len(), 1);
        assert!(store.find("src/main.c", 12).is_some());
        assert!(!store.toggle("src/main.c", 12));
        assert!(store.is_empty());
    }

    #[test]
    fn lines_only_include_enabled_breakpoints() {
        let mut store = BreakpointStore::default();
        store.toggle("src/main.c", 3);
        store.toggle("src/main.c", 8);
        store.set("src/main.c", 8, false);
        assert_eq!(store.lines("src/main.c"), BTreeSet::from([3]));
        assert_eq!(store.enabled_count(), 1);
    }

    #[test]
    fn roundtrip_through_state_file() {
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-breakpoints-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut store = BreakpointStore::default();
        store.toggle("src/main.c", 21);
        store.save(&root).unwrap();

        let loaded = BreakpointStore::load(&root);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.all()[0].line, 21);
        assert!(breakpoints_path(&root).is_file());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn set_condition_creates_and_reverts() {
        let mut store = BreakpointStore::default();
        assert!(store.set_condition("src/main.c", 7, "count == 10"));
        let bp = store.find("src/main.c", 7).unwrap();
        assert_eq!(bp.kind, BreakpointKind::Conditional);
        assert_eq!(bp.condition, "count == 10");
        assert!(bp.enabled);

        assert!(store.set_condition("src/main.c", 7, ""));
        let bp = store.find("src/main.c", 7).unwrap();
        assert_eq!(bp.kind, BreakpointKind::Line);
        assert_eq!(bp.condition, "");

        // Clearing the condition of a non-existent breakpoint does nothing.
        assert!(!store.set_condition("src/other.c", 9, ""));
    }

    #[test]
    fn clear_file_keeps_other_files() {
        let mut store = BreakpointStore::default();
        store.toggle("src/main.c", 1);
        store.toggle("src/other.c", 2);
        store.clear_file("src/main.c");
        assert_eq!(store.len(), 1);
        assert_eq!(store.all()[0].path, "src/other.c");
    }
}
