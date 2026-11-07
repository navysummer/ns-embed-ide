//! Global toolchain configuration: per-tool path overrides that take
//! precedence over PATH auto-discovery, plus a tool registry mapping language
//! to the available compiler list (for wizard cascade selection).
//!
//! The overrides live in the user config directory (`toolchain.json`), shared
//! across projects. The registry is derived from PATH discovery plus any
//! overrides.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::language::Language;
use crate::recents::config_dir;

const TOOLCHAIN_FILE: &str = "toolchain.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolchainConfig {
    /// Tool name → explicit path. Empty/absent means auto-discover from PATH.
    pub overrides: BTreeMap<String, String>,
}

impl ToolchainConfig {
    pub fn load() -> Self {
        let Some(dir) = config_dir() else {
            return Self::default();
        };
        let path = dir.join(TOOLCHAIN_FILE);
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
        let text = serde_json::to_string_pretty(self).context("序列化工具链配置失败")?;
        fs::write(dir.join(TOOLCHAIN_FILE), text)
            .with_context(|| format!("写入 {} 失败", dir.join(TOOLCHAIN_FILE).display()))
    }

    pub fn override_path(&self, name: &str) -> Option<PathBuf> {
        self.overrides
            .get(name)
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from)
    }

    pub fn set_override(&mut self, name: &str, path: &str) {
        let path = path.trim();
        if path.is_empty() {
            self.overrides.remove(name);
        } else {
            self.overrides.insert(name.to_string(), path.to_string());
        }
    }
}

/// Resolve a tool's path honoring the global override first, then PATH.
/// A configured override wins even if the file is currently missing (the IDE
/// reports that state honestly rather than silently falling back).
pub fn resolve_tool(config: &ToolchainConfig, name: &str) -> Option<PathBuf> {
    if let Some(path) = config.override_path(name) {
        return Some(path);
    }
    super::toolchain::find_command(name)
}

/// Language → available compiler list (used by the wizard's compiler step for
/// cascade selection). Honours global overrides and PATH discovery.
pub fn registry(config: &ToolchainConfig, language: Language) -> Vec<crate::toolchain::Tool> {
    language
        .default_compilers()
        .iter()
        .map(|name| crate::toolchain::Tool {
            name: (*name).to_string(),
            path: resolve_tool(config, name),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_takes_precedence_over_path() {
        let mut config = ToolchainConfig::default();
        config.set_override("gcc", "/nonexistent/gcc");
        assert_eq!(
            config.override_path("gcc"),
            Some(PathBuf::from("/nonexistent/gcc"))
        );
        config.set_override("gcc", "");
        assert_eq!(config.override_path("gcc"), None);
    }

    #[test]
    fn unknown_override_is_empty() {
        let config = ToolchainConfig::default();
        assert_eq!(config.override_path("never-set"), None);
    }

    #[test]
    fn registry_honours_override() {
        let mut config = ToolchainConfig::default();
        config.set_override("clang", "/nonexistent/clang");
        let tools = registry(&config, Language::C);
        let clang = tools.iter().find(|tool| tool.name == "clang").unwrap();
        assert_eq!(
            clang.path,
            Some(PathBuf::from("/nonexistent/clang"))
        );
    }

    #[test]
    fn config_serde_round_trips() {
        let mut config = ToolchainConfig::default();
        config.set_override("openocd", "/opt/bin/openocd");
        config.set_override("pyocd", "/usr/local/bin/pyocd");
        let text = serde_json::to_string(&config).unwrap();
        let loaded: ToolchainConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(loaded, config);
        assert_eq!(
            loaded.override_path("openocd"),
            Some(PathBuf::from("/opt/bin/openocd"))
        );
    }
}
