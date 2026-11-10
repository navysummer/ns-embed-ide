//! Light/dark theme preference, persisted in the user config directory
//! (`theme.json`) and shared across projects.
//!
//! The IDE asks [`ThemeSettings::load`] at startup and calls [`save`] whenever
//! the user toggles the theme, so the choice survives restarts.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::recents::config_dir;

const THEME_FILE: &str = "theme.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Dark,
    Light,
}

impl Default for ThemeMode {
    fn default() -> Self {
        ThemeMode::Dark
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeSettings {
    #[serde(default)]
    pub mode: ThemeMode,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            mode: ThemeMode::Dark,
        }
    }
}

impl ThemeSettings {
    pub fn load() -> Self {
        let Some(path) = theme_path() else {
            return Self::default();
        };
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let Some(dir) = config_dir() else {
            anyhow::bail!("无法确定用户配置目录");
        };
        fs::create_dir_all(&dir)
            .with_context(|| format!("创建配置目录 {} 失败", dir.display()))?;
        let path = dir.join(THEME_FILE);
        let text = serde_json::to_string_pretty(self).context("序列化 theme.json 失败")?;
        fs::write(&path, text).with_context(|| format!("写入 {} 失败", path.display()))
    }
}

fn theme_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(THEME_FILE))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_is_default_when_missing_or_invalid() {
        let settings = ThemeSettings::default();
        assert_eq!(settings.mode, ThemeMode::Dark);
        let invalid: ThemeSettings =
            serde_json::from_str("not json").unwrap_or_default();
        assert_eq!(invalid.mode, ThemeMode::Dark);
    }

    #[test]
    fn light_roundtrips_through_json() {
        let settings = ThemeSettings {
            mode: ThemeMode::Light,
        };
        let text = serde_json::to_string(&settings).unwrap();
        let back: ThemeSettings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.mode, ThemeMode::Light);
    }
}
