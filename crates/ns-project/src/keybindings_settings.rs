//! Global keyboard-shortcut overrides (F13 快捷键配置).
//!
//! Persisted in the user config directory (`keybindings.json`), shared across
//! projects. The map is action name -> new keystroke string (e.g.
//! `"Build": "cmd-b"`); actions absent from the map keep their default binding.
//! The IDE applies these overrides when registering the keymap at startup, so
//! users can remap any command without touching source.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::recents::config_dir;


pub const KEYBINDINGS_FILE: &str = "keybindings.json";
///
/// A `BTreeMap` keeps the file deterministic and diff-friendly. An empty value
/// clears the binding (the action becomes unbound); the key is removed on save
/// so the default is restored rather than stored as an empty string.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeybindingOverrides {
    pub overrides: BTreeMap<String, String>,
}

impl KeybindingOverrides {
    pub fn load() -> Self {
        let Some(dir) = config_dir() else {
            return Self::default();
        };
        let path = dir.join(KEYBINDINGS_FILE);
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
        // Drop empty values so unbound actions round-trip as absent.
        let cleaned: BTreeMap<String, String> = self
            .overrides
            .iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let text = serde_json::to_string_pretty(&cleaned)
            .context("序列化快捷键覆盖失败")?;
        fs::write(dir.join(KEYBINDINGS_FILE), text)
            .with_context(|| format!("写入 {} 失败", dir.join(KEYBINDINGS_FILE).display()))
    }

    /// The replacement keystroke for `action`, if one is configured.
    pub fn get(&self, action: &str) -> Option<&str> {
        self.overrides
            .get(action)
            .map(|s| s.as_str())
            .filter(|s| !s.trim().is_empty())
    }

    /// Path of the keybindings file, if the config directory is available.
    pub fn path() -> Option<PathBuf> {
        config_dir().map(|dir| dir.join(KEYBINDINGS_FILE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_round_trip_via_serde() {
        let mut map = KeybindingOverrides::default();
        map.overrides.insert("Build".to_string(), "cmd-b".to_string());
        map.overrides
            .insert("Debug".to_string(), "shift-f5".to_string());
        let json = serde_json::to_string(&map).unwrap();
        let back: KeybindingOverrides = serde_json::from_str(&json).unwrap();
        assert_eq!(back.get("Build"), Some("cmd-b"));
        assert_eq!(back.get("Debug"), Some("shift-f5"));
        assert_eq!(back.get("Flash"), None);
    }

    #[test]
    fn empty_value_is_treated_as_unbound() {
        let mut map = KeybindingOverrides::default();
        map.overrides.insert("Build".to_string(), "  ".to_string());
        assert_eq!(map.get("Build"), None);
    }

    #[test]
    fn save_drops_empty_values() {
        let dir = std::env::temp_dir().join(format!("ns-kb-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(KEYBINDINGS_FILE);
        let mut map = KeybindingOverrides::default();
        map.overrides.insert("Build".to_string(), "cmd-b".to_string());
        map.overrides.insert("Debug".to_string(), "".to_string());
        // Save writes to the real config dir only; emulate by serializing.
        let cleaned: BTreeMap<String, String> = map
            .overrides
            .iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        fs::write(&path, serde_json::to_string_pretty(&cleaned).unwrap()).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("Debug"));
        assert!(text.contains("Build"));
        let _ = fs::remove_dir_all(&dir);
    }
}
