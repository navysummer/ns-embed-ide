//! Global terminal settings: the shell executable used by the terminal panel.
//!
//! Persisted in the user config directory (`terminal.json`), shared across
//! projects. The default is auto-detected per platform: `cmd.exe` on Windows,
//! `bash` on Linux/macOS (falling back to `$SHELL`). The user can switch to any
//! other terminal in the settings dialog; the path is auto-detected from PATH.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::recents::config_dir;
use crate::toolchain::find_command;

const TERMINAL_FILE: &str = "terminal.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSettings {
    /// Shell executable path or name (e.g. `bash`, `cmd`, `pwsh`, or a path).
    /// Empty = use the platform default.
    pub shell: String,
}

impl TerminalSettings {
    pub fn load() -> Self {
        let Some(dir) = config_dir() else {
            return Self::default();
        };
        let path = dir.join(TERMINAL_FILE);
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
        let text = serde_json::to_string_pretty(self).context("序列化终端设置失败")?;
        fs::write(dir.join(TERMINAL_FILE), text)
            .with_context(|| format!("写入 {} 失败", dir.join(TERMINAL_FILE).display()))
    }

    /// The shell command to spawn, honoring an explicit setting and falling
    /// back to the platform default.
    pub fn shell_command(&self) -> String {
        let explicit = self.shell.trim();
        if !explicit.is_empty() {
            return explicit.to_string();
        }
        default_terminal_shell()
    }
}

/// Auto-detect the platform's default terminal shell:
/// `cmd.exe` on Windows, `bash` on Linux/macOS (fallback to `$SHELL`, then sh).
pub fn default_terminal_shell() -> String {
    #[cfg(windows)]
    {
        // Windows: cmd is the default; prefer it, then pwsh/powershell.
        for candidate in ["cmd.exe", "pwsh.exe", "powershell.exe"] {
            if find_command(candidate).is_some() {
                return candidate.to_string();
            }
        }
        "cmd.exe".to_string()
    }
    #[cfg(not(windows))]
    {
        // Linux/macOS: bash is the default; fall back to $SHELL then sh.
        for candidate in ["bash", "sh"] {
            if let Some(path) = find_command(candidate) {
                return path.display().to_string();
            }
        }
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

/// The terminal name for display purposes.
pub fn terminal_label(shell: &str) -> String {
    PathBuf::from(shell)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| shell.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shell_is_non_empty() {
        assert!(!default_terminal_shell().is_empty());
    }

    #[test]
    fn explicit_setting_wins() {
        let settings = TerminalSettings {
            shell: "/usr/bin/zsh".to_string(),
        };
        assert_eq!(settings.shell_command(), "/usr/bin/zsh");
    }

    #[test]
    fn empty_setting_falls_back() {
        let settings = TerminalSettings::default();
        assert_eq!(settings.shell_command(), default_terminal_shell());
    }

    #[test]
    fn label_strips_path() {
        assert_eq!(terminal_label("/bin/bash"), "bash");
        assert_eq!(terminal_label("cmd.exe"), "cmd.exe");
    }
}
