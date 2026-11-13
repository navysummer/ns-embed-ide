//! Global IDE preferences: editor behavior shared across projects.
//!
//! Persisted in the user config directory (`ide.json`). Preferences here are
//! about *how the IDE acts* (format-on-save, save-before-build), not about a
//! project's toolchain — those live in `project.toml`.

use std::fs;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::recents::config_dir;

const IDE_FILE: &str = "ide.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IdeSettings {
    /// When saving an editor tab, ask the LSP to format the document first
    /// (only for languages with an active language server). Off by default so
    /// saving is instantaneous and never rewrites a user's formatting.
    pub format_on_save: bool,
    /// Save all dirty editor tabs before build/run/flash/debug. When false,
    /// the build proceeds with the on-disk source (which may be stale) and
    /// the status bar warns instead of silently forcing a save. Default on:
    /// building stale buffers is the most common source of "why didn't my
    /// change take effect" confusion in embedded work.
    pub save_before_build: bool,
}

impl Default for IdeSettings {
    fn default() -> Self {
        Self {
            format_on_save: false,
            save_before_build: true,
        }
    }
}

impl IdeSettings {
    pub fn load() -> Self {
        let Some(dir) = config_dir() else {
            return Self::default();
        };
        let path = dir.join(IDE_FILE);
        let Ok(text) = fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let Some(dir) = config_dir() else {
            anyhow::bail!("无法定位用户配置目录");
        };
        fs::create_dir_all(&dir)
            .with_context(|| format!("创建配置目录 {} 失败", dir.display()))?;
        let text = serde_json::to_string_pretty(self).context("序列化 IDE 设置失败")?;
        fs::write(dir.join(IDE_FILE), text)
            .with_context(|| format!("写入 {} 失败", dir.join(IDE_FILE).display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ide_settings_round_trip_via_json() {
        let settings = IdeSettings {
            format_on_save: true,
            save_before_build: false,
        };
        let text = serde_json::to_string(&settings).unwrap();
        let parsed: IdeSettings = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, settings);
    }

    #[test]
    fn ide_settings_defaults_are_safe() {
        // Default: no auto-format (never rewrites user code on save), but
        // save-before-build on (building stale code is the common foot-gun).
        let settings = IdeSettings::default();
        assert!(!settings.format_on_save);
        assert!(settings.save_before_build);
    }

    #[test]
    fn ide_settings_missing_fields_default() {
        // Forward compatibility: a JSON file written by an older version that
        // lacks these keys still parses with safe defaults.
        let parsed: IdeSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, IdeSettings::default());
    }
}
