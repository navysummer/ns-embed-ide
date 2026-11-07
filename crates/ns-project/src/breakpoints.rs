//! Project-local breakpoint state.
//!
//! Breakpoints are IDE state, not part of the authoritative `project.toml`.
//! They live in `.ns-ide/breakpoints.json` so they survive restarts without
//! polluting the user's toolchain configuration.

use std::collections::{BTreeSet, HashMap};
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
    /// Condition expression; only meaningful for [`BreakpointKind::Conditional`].
    pub condition: String,
    /// Hit-count limit: pause on the N-th hit; empty for no limit.
    pub hit_count: String,
    /// Log message (with `{expr}` placeholders); only meaningful for [`BreakpointKind::Log`].
    pub log_message: String,
    /// Whether the debugger confirmed the breakpoint position, when last synced.
    /// `None` = never synced (not yet verified), `Some(true)` = verified,
    /// `Some(false)` = pending/unverified with `reason` explaining why.
    pub verified: Option<bool>,
    /// Why a breakpoint is pending, when reported by the debugger.
    pub reason: String,
    /// The stored source location no longer exists (file removed/renamed or the
    /// file is now too short). Recomputed from the filesystem on every sync, so
    /// this is runtime-only state and not persisted.
    #[serde(skip)]
    pub stale: bool,
    /// The debugger is currently stopped at this breakpoint. Cleared as soon as
    /// the debuggee continues; runtime-only and not persisted.
    #[serde(skip)]
    pub hit: bool,
}

impl Default for Breakpoint {
    fn default() -> Self {
        Self {
            path: String::new(),
            line: 1,
            enabled: true,
            kind: BreakpointKind::Line,
            condition: String::new(),
            hit_count: String::new(),
            log_message: String::new(),
            verified: None,
            reason: String::new(),
            stale: false,
            hit: false,
        }
    }
}

impl Breakpoint {
    pub fn label(&self) -> String {
        format!("{}:{}", self.path, self.line)
    }

    /// Short state word for the breakpoint panel, e.g. "已验证"/"未绑定"/"失效".
    pub fn state_label(&self) -> &'static str {
        if !self.enabled {
            return "已禁用";
        }
        if self.stale {
            return "失效";
        }
        if self.hit {
            return "命中";
        }
        match self.verified {
            Some(true) => "已验证",
            Some(false) => "未绑定",
            None => "未同步",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchAccess {
    Read,
    #[default]
    Write,
    ReadWrite,
}

impl WatchAccess {
    pub fn label(self) -> &'static str {
        match self {
            Self::Read => "读取",
            Self::Write => "写入",
            Self::ReadWrite => "读写",
        }
    }

    /// Value sent as the DAP `accessType`.
    pub fn dap_value(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::ReadWrite => "readWrite",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Read => Self::Write,
            Self::Write => Self::ReadWrite,
            Self::ReadWrite => Self::Read,
        }
    }
}

/// A data breakpoint (watchpoint) on an expression such as `marker` or `obj.field`.
/// Unlike line breakpoints it has no source path/line; the expression is
/// re-resolved to an address by the adapter whenever the debuggee stops.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Watchpoint {
    /// Expression evaluated in the selected frame, e.g. `marker` or `s.value`.
    pub expression: String,
    pub access: WatchAccess,
    pub enabled: bool,
    /// Adapter verification of the last data-breakpoint sync.
    pub verified: Option<bool>,
    /// Why the watchpoint could not be bound, when unverified.
    pub reason: String,
    /// The debuggee is currently stopped at this watchpoint (runtime-only).
    #[serde(skip)]
    pub hit: bool,
    /// Adapter-provided `dataId` (address + size) from `dataBreakpointInfo`.
    /// Runtime-only: re-resolved after every stop, never persisted.
    #[serde(skip)]
    pub data_id: String,
    /// Adapter id of the bound data breakpoint (runtime-only; not stable
    /// across sessions, so it is re-resolved after every stop).
    #[serde(skip)]
    pub adapter_id: Option<u64>,
}

impl Default for Watchpoint {
    fn default() -> Self {
        Self {
            expression: String::new(),
            access: WatchAccess::default(),
            enabled: true,
            verified: None,
            reason: String::new(),
            hit: false,
            data_id: String::new(),
            adapter_id: None,
        }
    }
}

impl Watchpoint {
    /// Short state word for the breakpoint panel.
    pub fn state_label(&self) -> &'static str {
        if !self.enabled {
            return "已禁用";
        }
        if self.hit {
            return "命中";
        }
        match self.verified {
            Some(true) => "已验证",
            Some(false) => "未绑定",
            None => "未同步",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BreakpointStore {
    pub breakpoints: Vec<Breakpoint>,
    pub watchpoints: Vec<Watchpoint>,
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

    pub fn watchpoints(&self) -> &[Watchpoint] {
        &self.watchpoints
    }

    pub fn find_watchpoint(&self, expression: &str) -> Option<&Watchpoint> {
        self.watchpoints
            .iter()
            .find(|watchpoint| watchpoint.expression == expression)
    }

    /// Add a watchpoint, or update the access type of an existing one. An
    /// existing disabled watchpoint is re-enabled. Returns `false` for an empty
    /// expression.
    pub fn add_watchpoint(&mut self, expression: &str, access: WatchAccess) -> bool {
        let expression = expression.trim().to_string();
        if expression.is_empty() {
            return false;
        }
        if let Some(watchpoint) = self
            .watchpoints
            .iter_mut()
            .find(|watchpoint| watchpoint.expression == expression)
        {
            watchpoint.access = access;
            watchpoint.enabled = true;
            watchpoint.verified = None;
            watchpoint.reason.clear();
            watchpoint.data_id.clear();
        } else {
            self.watchpoints.push(Watchpoint {
                expression,
                access,
                ..Watchpoint::default()
            });
            self.sort();
        }
        true
    }

    pub fn remove_watchpoint(&mut self, expression: &str) -> bool {
        let before = self.watchpoints.len();
        self.watchpoints
            .retain(|watchpoint| watchpoint.expression != expression);
        self.watchpoints.len() != before
    }

    pub fn set_watchpoint_enabled(&mut self, expression: &str, enabled: bool) -> bool {
        let Some(watchpoint) = self
            .watchpoints
            .iter_mut()
            .find(|watchpoint| watchpoint.expression == expression)
        else {
            return false;
        };
        watchpoint.enabled = enabled;
        if !enabled {
            watchpoint.hit = false;
            watchpoint.adapter_id = None;
            watchpoint.data_id.clear();
        }
        true
    }

    /// Record the adapter outcome for a watchpoint sync.
    pub fn mark_watchpoint_verified(
        &mut self,
        expression: &str,
        verified: bool,
        reason: &str,
        adapter_id: Option<u64>,
    ) {
        let Some(watchpoint) = self
            .watchpoints
            .iter_mut()
            .find(|watchpoint| watchpoint.expression == expression)
        else {
            return;
        };
        watchpoint.verified = Some(verified);
        watchpoint.adapter_id = adapter_id;
        if verified {
            watchpoint.reason.clear();
        } else {
            watchpoint.data_id.clear();
            watchpoint.reason = reason.to_string();
        }
    }

    /// Record the adapter `dataId` resolved for a watchpoint expression. The
    /// id is only meaningful until the debuggee resumes, so it is cleared on
    /// every stop before the set is re-resolved.
    pub fn set_watchpoint_data_id(&mut self, expression: &str, data_id: &str) {
        let Some(watchpoint) = self
            .watchpoints
            .iter_mut()
            .find(|watchpoint| watchpoint.expression == expression)
        else {
            return;
        };
        watchpoint.data_id = data_id.to_string();
    }

    pub fn mark_watchpoint_hit(&mut self, expression: &str) {
        if let Some(watchpoint) = self
            .watchpoints
            .iter_mut()
            .find(|watchpoint| watchpoint.expression == expression)
        {
            watchpoint.hit = true;
        }
    }

    pub fn clear_watchpoint_hits(&mut self) {
        for watchpoint in &mut self.watchpoints {
            watchpoint.hit = false;
        }
    }

    pub fn clear_watchpoints(&mut self) {
        self.watchpoints.clear();
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
    /// Changing the condition invalidates any previous verification state.
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
            breakpoint.verified = None;
            if !breakpoint.stale {
                breakpoint.reason.clear();
            }
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
                ..Breakpoint::default()
            });
            self.sort();
            true
        }
    }
    /// Toggle a breakpoint between software and hardware execution.
    /// Returns the new `hardware` state, or `None` if there is no breakpoint
    /// at `path:line`. Changing the kind invalidates any previous verification.
    pub fn set_hardware(&mut self, path: &str, line: usize) -> Option<bool> {
        let breakpoint = self
            .breakpoints
            .iter_mut()
            .find(|breakpoint| breakpoint.path == path && breakpoint.line == line)?;
        let hardware = match breakpoint.kind {
            BreakpointKind::Hardware => {
                // Back to a plain software breakpoint (preserving any
                // condition/log message the user had set).
                breakpoint.kind = if breakpoint.log_message.is_empty() {
                    if breakpoint.condition.is_empty() {
                        BreakpointKind::Line
                    } else {
                        BreakpointKind::Conditional
                    }
                } else {
                    BreakpointKind::Log
                };
                false
            }
            _ => {
                breakpoint.kind = BreakpointKind::Hardware;
                true
            }
        };
        breakpoint.verified = None;
        if !breakpoint.stale {
            breakpoint.reason.clear();
        }
        Some(hardware)
    }

    /// Set the hit-count limit (`N`): pause on the N-th hit. Empty clears the limit.
    /// Changing it invalidates any previous verification state.
    pub fn set_hit_count(&mut self, path: &str, line: usize, hit_count: &str) -> bool {
        let hit_count = hit_count.trim().to_string();
        let Some(breakpoint) = self
            .breakpoints
            .iter_mut()
            .find(|breakpoint| breakpoint.path == path && breakpoint.line == line)
        else {
            return false;
        };
        breakpoint.hit_count = hit_count;
        breakpoint.verified = None;
        if !breakpoint.stale {
            breakpoint.reason.clear();
        }
        true
    }

    /// Set the log message: when non-empty the breakpoint does not pause, it
    /// prints the message (with `{expr}` placeholders) to the debug console.
    pub fn set_log_message(&mut self, path: &str, line: usize, log_message: &str) -> bool {
        let log_message = log_message.trim().to_string();
        let Some(breakpoint) = self
            .breakpoints
            .iter_mut()
            .find(|breakpoint| breakpoint.path == path && breakpoint.line == line)
        else {
            return false;
        };
        breakpoint.log_message = log_message.clone();
        breakpoint.kind = if log_message.is_empty() {
            match breakpoint.kind {
                BreakpointKind::Log => {
                    if breakpoint.condition.is_empty() {
                        BreakpointKind::Line
                    } else {
                        BreakpointKind::Conditional
                    }
                }
                _ => breakpoint.kind,
            }
        } else {
            BreakpointKind::Log
        };
        breakpoint.verified = None;
        if !breakpoint.stale {
            breakpoint.reason.clear();
        }
        true
    }
    /// Record the last sync result: whether the debugger bound the breakpoint
    /// (`verified`) and why it couldn't (`reason`, when unverified).
    pub fn mark_verified(&mut self, path: &str, line: usize, verified: bool, reason: &str) {
        let Some(breakpoint) = self
            .breakpoints
            .iter_mut()
            .find(|breakpoint| breakpoint.path == path && breakpoint.line == line)
        else {
            return;
        };
        breakpoint.verified = Some(verified);
        if verified {
            breakpoint.reason.clear();
        } else {
            breakpoint.stale = false;
            breakpoint.hit = false;
            breakpoint.reason = reason.to_string();
        }
    }

    /// Record that the debugger stopped at this breakpoint: the location is
    /// confirmed bound, so verification is `true` and any pending reason is
    /// cleared. (The DAP adapter enforces the hit-count limit itself via
    /// `hitCondition`; the store only tracks verification state.)
    pub fn mark_hit(&mut self, path: &str, line: usize) {
        if let Some(breakpoint) = self
            .breakpoints
            .iter_mut()
            .find(|breakpoint| breakpoint.path == path && breakpoint.line == line)
        {
            breakpoint.verified = Some(true);
            breakpoint.stale = false;
            breakpoint.hit = true;
            breakpoint.reason.clear();
        }
    }

    /// Mark a breakpoint whose stored source location no longer exists: the
    /// file is gone/renamed or the line is past the end of the file. It is not
    /// sent to the adapter while stale, so `verified` is cleared.
    pub fn mark_stale(&mut self, path: &str, line: usize, reason: &str) {
        if let Some(breakpoint) = self
            .breakpoints
            .iter_mut()
            .find(|breakpoint| breakpoint.path == path && breakpoint.line == line)
        {
            breakpoint.stale = true;
            breakpoint.hit = false;
            breakpoint.verified = None;
            breakpoint.reason = reason.to_string();
        }
    }

    /// Clear a previously stale marker once the source location exists again.
    /// Verification is reset so the breakpoint is re-synced with the adapter.
    pub fn mark_source_valid(&mut self, path: &str, line: usize) {
        if let Some(breakpoint) = self
            .breakpoints
            .iter_mut()
            .find(|breakpoint| breakpoint.path == path && breakpoint.line == line)
            && breakpoint.stale
        {
            breakpoint.stale = false;
            breakpoint.verified = None;
            breakpoint.reason.clear();
        }
    }

    /// Clear the transient "hit" marker after the debuggee resumes.
    pub fn clear_hits(&mut self) {
        for breakpoint in &mut self.breakpoints {
            breakpoint.hit = false;
        }
    }

    /// Recompute the `stale` marker for every breakpoint against `root`.
    ///
    /// A breakpoint is stale when its file was removed/renamed, cannot be read,
    /// or its line is past the end of the file. Stale breakpoints are not sent
    /// to the adapter, and their reason is shown in the breakpoint panel.
    /// Locations that exist again are re-armed for the next sync.
    pub fn revalidate_sources(&mut self, root: &Path) {
        let mut line_counts: HashMap<String, Result<usize, String>> = HashMap::new();
        for index in 0..self.breakpoints.len() {
            let (path, line) = {
                let breakpoint = &self.breakpoints[index];
                (breakpoint.path.clone(), breakpoint.line)
            };
            let status = line_counts.entry(path.clone()).or_insert_with(|| {
                let absolute = root.join(&path);
                match fs::read_to_string(&absolute) {
                    Ok(text) => Ok(text.lines().count()),
                    Err(_) if !absolute.exists() => Err("源文件不存在".to_string()),
                    Err(_) => Err("源文件无法读取".to_string()),
                }
            });
            match status {
                Err(reason) => self.mark_stale(&path, line, reason),
                Ok(_) if line == 0 => self.mark_stale(&path, line, "断点行号无效"),
                Ok(count) if line > *count => {
                    let reason = format!("源码行已不存在（文件共 {count} 行）");
                    self.mark_stale(&path, line, &reason);
                }
                Ok(_) => self.mark_source_valid(&path, line),
            }
        }
    }

    /// Drop every stored verification/pending state (e.g. on session start when
    /// the breakpoint set is about to be re-synced, or after a rebuild).
    pub fn reset_verification(&mut self) {
        for breakpoint in &mut self.breakpoints {
            breakpoint.verified = None;
            breakpoint.reason.clear();
            breakpoint.stale = false;
            breakpoint.hit = false;
        }
        for watchpoint in &mut self.watchpoints {
            watchpoint.verified = None;
            watchpoint.reason.clear();
            watchpoint.hit = false;
            watchpoint.data_id.clear();
            watchpoint.adapter_id = None;
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
        self.watchpoints.clear();
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
        self.watchpoints
            .sort_by(|a, b| a.expression.cmp(&b.expression));
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
        store.watchpoints.push(Watchpoint {
            expression: "marker".into(),
            access: WatchAccess::ReadWrite,
            ..Watchpoint::default()
        });
        store.save(&root).unwrap();

        let loaded = BreakpointStore::load(&root);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.all()[0].line, 21);
        assert_eq!(loaded.watchpoints().len(), 1);
        assert_eq!(loaded.watchpoints()[0].expression, "marker");
        assert_eq!(loaded.watchpoints()[0].access, WatchAccess::ReadWrite);
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
    fn set_hardware_toggles_kind_and_preserves_metadata() {
        let mut store = BreakpointStore::default();
        store.toggle("src/main.c", 7);
        assert!(store.set_condition("src/main.c", 7, "count == 10"));
        assert!(store.set_log_message("src/main.c", 7, "count = {count}"));
        store.mark_verified("src/main.c", 7, true, "");

        // Switch to a hardware breakpoint: kind flips, condition/log preserved,
        // verification invalidated so the adapter re-syncs it.
        assert_eq!(store.set_hardware("src/main.c", 7), Some(true));
        let bp = store.find("src/main.c", 7).unwrap();
        assert_eq!(bp.kind, BreakpointKind::Hardware);
        assert_eq!(bp.condition, "count == 10");
        assert_eq!(bp.log_message, "count = {count}");
        assert_eq!(bp.verified, None);

        // Switch back to software: a log message keeps the Log kind.
        assert_eq!(store.set_hardware("src/main.c", 7), Some(false));
        let bp = store.find("src/main.c", 7).unwrap();
        assert_eq!(bp.kind, BreakpointKind::Log);

        // Absent breakpoints report None.
        assert_eq!(store.set_hardware("src/other.c", 9), None);
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

    #[test]
    fn set_hit_count_updates_and_invalidates_verification() {
        let mut store = BreakpointStore::default();
        store.toggle("src/main.c", 5);
        store.mark_verified("src/main.c", 5, true, "");
        assert!(store.set_hit_count("src/main.c", 5, "3"));
        let bp = store.find("src/main.c", 5).unwrap();
        assert_eq!(bp.hit_count, "3");
        assert_eq!(bp.verified, None);
        // Absent breakpoints report false.
        assert!(!store.set_hit_count("src/other.c", 9, "1"));
    }

    #[test]
    fn set_log_message_switches_kind_and_reverts() {
        let mut store = BreakpointStore::default();
        store.toggle("src/main.c", 7);
        assert!(store.set_log_message("src/main.c", 7, "count = {count}"));
        let bp = store.find("src/main.c", 7).unwrap();
        assert_eq!(bp.kind, BreakpointKind::Log);
        assert_eq!(bp.log_message, "count = {count}");
        // A log message alone keeps the Log kind even with a condition set.
        assert!(store.set_condition("src/main.c", 7, "x == 1"));
        assert!(store.set_log_message("src/main.c", 7, ""));
        let bp = store.find("src/main.c", 7).unwrap();
        assert_eq!(bp.kind, BreakpointKind::Conditional);
    }

    #[test]
    fn mark_verified_stores_state_and_reason() {
        let mut store = BreakpointStore::default();
        store.toggle("src/main.c", 9);
        store.mark_verified("src/main.c", 9, false, "source not found");
        let bp = store.find("src/main.c", 9).unwrap();
        assert_eq!(bp.verified, Some(false));
        assert_eq!(bp.reason, "source not found");
        store.mark_hit("src/main.c", 9);
        let bp = store.find("src/main.c", 9).unwrap();
        assert_eq!(bp.verified, Some(true));
        assert!(bp.reason.is_empty());
        assert!(bp.hit);
        assert!(matches!(bp.state_label(), "命中"));
        store.clear_hits();
        let bp = store.find("src/main.c", 9).unwrap();
        assert!(!bp.hit);
        assert!(matches!(bp.state_label(), "已验证"));
    }

    #[test]
    fn stale_state_explains_reason_and_clears_when_source_returns() {
        let mut store = BreakpointStore::default();
        store.toggle("src/main.c", 9);
        store.mark_verified("src/main.c", 9, true, "");
        store.mark_stale("src/main.c", 9, "源码行已不存在（文件共 3 行）");
        let bp = store.find("src/main.c", 9).unwrap();
        assert!(bp.stale);
        assert_eq!(bp.verified, None);
        assert_eq!(bp.reason, "源码行已不存在（文件共 3 行）");
        assert_eq!(bp.state_label(), "失效");
        // Re-validating the source clears the stale marker and forces a re-sync.
        store.mark_source_valid("src/main.c", 9);
        let bp = store.find("src/main.c", 9).unwrap();
        assert!(!bp.stale);
        assert_eq!(bp.verified, None);
        assert!(bp.reason.is_empty());
        assert_eq!(bp.state_label(), "未同步");
    }

    #[test]
    fn reset_verification_clears_all_state() {
        let mut store = BreakpointStore::default();
        store.toggle("src/main.c", 1);
        store.toggle("src/main.c", 2);
        store.mark_verified("src/main.c", 1, true, "");
        store.mark_verified("src/main.c", 2, false, "pending");
        store.mark_stale("src/main.c", 2, "source not found");
        store.mark_hit("src/main.c", 1);
        store.reset_verification();
        for breakpoint in store.all() {
            assert_eq!(breakpoint.verified, None);
            assert!(breakpoint.reason.is_empty());
            assert!(!breakpoint.stale);
            assert!(!breakpoint.hit);
        }
    }

    #[test]
    fn state_label_reflects_enabled_and_verified() {
        let mut store = BreakpointStore::default();
        store.toggle("src/main.c", 1);
        let mut bp = store.all()[0].clone();
        assert_eq!(bp.state_label(), "未同步");
        bp.verified = Some(true);
        assert_eq!(bp.state_label(), "已验证");
        bp.verified = Some(false);
        assert_eq!(bp.state_label(), "未绑定");
        bp.hit = true;
        assert_eq!(bp.state_label(), "命中");
        bp.stale = true;
        assert_eq!(bp.state_label(), "失效");
        bp.enabled = false;
        assert_eq!(bp.state_label(), "已禁用");
    }

    #[test]
    fn watchpoints_add_update_and_remove() {
        let mut store = BreakpointStore::default();
        assert!(!store.add_watchpoint("   ", WatchAccess::Write));
        assert!(store.add_watchpoint("marker", WatchAccess::Write));
        assert!(store.add_watchpoint("marker", WatchAccess::ReadWrite));
        assert_eq!(store.watchpoints().len(), 1, "same expression is deduplicated");
        assert_eq!(store.watchpoints()[0].access, WatchAccess::ReadWrite);
        assert!(store.add_watchpoint("obj.field", WatchAccess::Read));
        // Alphabetical ordering keeps the panel stable.
        assert_eq!(store.watchpoints()[0].expression, "marker");
        assert_eq!(store.watchpoints()[1].expression, "obj.field");
        assert!(store.remove_watchpoint("marker"));
        assert!(!store.remove_watchpoint("marker"));
        assert_eq!(store.watchpoints().len(), 1);
    }

    #[test]
    fn watchpoint_state_label_and_runtime_reset() {
        let mut store = BreakpointStore::default();
        store.add_watchpoint("marker", WatchAccess::Write);
        let watchpoint = &store.watchpoints()[0];
        assert_eq!(watchpoint.state_label(), "未同步");
        store.mark_watchpoint_verified("marker", true, "", Some(7));
        let watchpoint = store.find_watchpoint("marker").unwrap();
        assert_eq!(watchpoint.state_label(), "已验证");
        assert_eq!(watchpoint.adapter_id, Some(7));
        store.mark_watchpoint_hit("marker");
        assert_eq!(store.find_watchpoint("marker").unwrap().state_label(), "命中");
        store.clear_watchpoint_hits();
        assert_eq!(store.find_watchpoint("marker").unwrap().state_label(), "已验证");
        store.set_watchpoint_enabled("marker", false);
        assert_eq!(store.find_watchpoint("marker").unwrap().state_label(), "已禁用");
        store.set_watchpoint_enabled("marker", true);
        store.reset_verification();
        let watchpoint = store.find_watchpoint("marker").unwrap();
        assert_eq!(watchpoint.verified, None);
        assert_eq!(watchpoint.adapter_id, None);
        assert!(!watchpoint.hit);
        assert!(watchpoint.reason.is_empty());
    }

    #[test]
    fn watchpoint_verification_reason_and_access_values() {
        let mut store = BreakpointStore::default();
        store.add_watchpoint("missing", WatchAccess::Read);
        store.mark_watchpoint_verified("missing", false, "无法解析该表达式", None);
        let watchpoint = store.find_watchpoint("missing").unwrap();
        assert_eq!(watchpoint.state_label(), "未绑定");
        assert_eq!(watchpoint.reason, "无法解析该表达式");
        assert_eq!(watchpoint.access.dap_value(), "read");
        assert_eq!(WatchAccess::Write.dap_value(), "write");
        assert_eq!(WatchAccess::ReadWrite.dap_value(), "readWrite");
        assert_eq!(WatchAccess::ReadWrite.next(), WatchAccess::Read);
    }

    #[test]
    fn revalidate_sources_marks_missing_and_short_locations() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SLOT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "ns-project-bp-{}-{}",
            std::process::id(),
            SLOT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.c"), "a\nb\nc\n").unwrap();

        let mut store = BreakpointStore::default();
        store.toggle("src/main.c", 2);
        store.toggle("src/main.c", 9);
        store.toggle("src/gone.c", 1);
        store.revalidate_sources(&root);

        let valid = store.find("src/main.c", 2).unwrap();
        assert!(!valid.stale);
        let short = store.find("src/main.c", 9).unwrap();
        assert!(short.stale);
        assert_eq!(short.state_label(), "失效");
        assert!(short.reason.contains("3 行"), "reason was {:?}", short.reason);
        let missing = store.find("src/gone.c", 1).unwrap();
        assert!(missing.stale);
        assert_eq!(missing.reason, "源文件不存在");

        // Extending the file re-arms the previously short location.
        fs::write(
            root.join("src/main.c"),
            (1..=10).map(|n| format!("line {n}\n")).collect::<String>(),
        )
        .unwrap();
        store.revalidate_sources(&root);
        let recovered = store.find("src/main.c", 9).unwrap();
        assert!(!recovered.stale);
        assert_eq!(recovered.verified, None);
        assert!(recovered.reason.is_empty());

        // Removing the file marks every location in it stale.
        fs::remove_file(root.join("src/main.c")).unwrap();
        store.revalidate_sources(&root);
        for line in [2, 9] {
            let breakpoint = store.find("src/main.c", line).unwrap();
            assert!(breakpoint.stale);
            assert_eq!(breakpoint.reason, "源文件不存在");
        }

        let _ = fs::remove_dir_all(root);
    }
}
