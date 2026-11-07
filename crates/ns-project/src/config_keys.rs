//! Cross-platform configuration keys and path variable expansion.
//!
//! Paths in `project.toml` may reference:
//!
//! 1. **Global config keys** — user-defined, platform-specific values stored in
//!    the user config directory (`config.json`), e.g. `cc_bin_path`. Referenced
//!    from `project.toml` as `$config["cc_bin_path"]`.
//! 2. **Built-in variables** — derived from the environment, e.g. `$home`,
//!    `$project_dir`, `$os`, `$arch`, `$toolchain_dir`. These make a config
//!    file portable across machines and platforms.
//!
//! `expand` substitutes both forms in a string, so a config like
//! `path = "$config["cc_bin_path"]"` resolves to the same absolute path on any
//! OS once the key is defined there.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::recents::config_dir;

const CONFIG_KEYS_FILE: &str = "config.json";

/// User-defined global config keys, keyed by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigKeys {
    /// Key name → value (usually a path or a platform-specific string).
    #[serde(default)]
    pub keys: BTreeMap<String, String>,
}

impl ConfigKeys {
    pub fn load() -> Self {
        let Some(path) = config_keys_path() else {
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
        let path = dir.join(CONFIG_KEYS_FILE);
        let text = serde_json::to_string_pretty(self)
            .context("序列化 config.json 失败")?;
        fs::write(&path, text)
            .with_context(|| format!("写入 {} 失败", path.display()))
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.keys.get(key).map(|value| value.as_str())
    }

    pub fn set(&mut self, key: &str, value: &str) {
        if value.trim().is_empty() {
            self.keys.remove(key);
        } else {
            self.keys.insert(key.to_string(), value.to_string());
        }
    }
}

fn config_keys_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(CONFIG_KEYS_FILE))
}

/// Built-in variable values that do not come from the config file.
#[derive(Debug, Clone, Default)]
pub struct BuiltinVars {
    pub home: PathBuf,
    pub project_dir: PathBuf,
    pub os: String,
    pub arch: String,
    pub toolchain_dir: PathBuf,
}

impl BuiltinVars {
    /// Build the variable set for a project root.
    pub fn for_project(project_dir: impl AsRef<Path>) -> Self {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."));
        let project_dir = project_dir.as_ref().to_path_buf();
        let toolchain_dir = env::var_os("NS_TOOLCHAIN_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".ns-embed-ide").join("toolchains"));
        Self {
            home,
            project_dir,
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            toolchain_dir,
        }
    }

    /// Look up a built-in variable by name.
    pub fn get(&self, name: &str) -> Option<String> {
        match name {
            "home" | "HOME" => Some(self.home.display().to_string()),
            "project_dir" | "PROJECT_DIR" => Some(self.project_dir.display().to_string()),
            "os" | "OS" => Some(self.os.clone()),
            "arch" | "ARCH" => Some(self.arch.clone()),
            "toolchain_dir" | "TOOLCHAIN_DIR" => Some(self.toolchain_dir.display().to_string()),
            _ => None,
        }
    }
}

/// Expand `$config["key"]` and `$var` references in `text`.
///
/// `$config["key"]` looks up a global config key; `$var` (without brackets)
/// looks up a built-in variable. Unknown references are left verbatim so the
/// caller can report them.
pub fn expand(text: &str, config: &ConfigKeys, vars: &BuiltinVars) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('$') {
        output.push_str(&rest[..index]);
        let after = &rest[index..];
        // `$config["key"]`
        if after.starts_with("$config[") {
            if let Some(end) = after.find(']') {
                let inner = &after["$config[".len()..end];
                let key = inner.trim_matches(|ch| ch == '"' || ch == '\'');
                let value = config.get(key);
                match value {
                    Some(value) => output.push_str(value),
                    None => {
                        // Keep the reference so validation can flag it.
                        output.push_str(&after[..end + 1]);
                    }
                }
                rest = &after[end + 1..];
                continue;
            }
        }
        // `$var` (built-in, single word until a non-identifier char).
        if let Some(name_end) = after[1..]
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        {
            let name = &after[1..1 + name_end];
            if let Some(value) = vars.get(name) {
                output.push_str(&value);
                rest = &after[1 + name_end..];
                continue;
            }
        } else {
            let name = &after[1..];
            if let Some(value) = vars.get(name) {
                output.push_str(&value);
                rest = "";
                continue;
            }
        }
        output.push('$');
        rest = &after[1..];
    }
    output.push_str(rest);
    output
}

/// Expand all path-like fields of a `ProjectConfig`-agnostic string map.
///
/// Provided for the IDE's config page and `project.toml` consumers.
pub fn expand_field(
    value: &str,
    config: &ConfigKeys,
    vars: &BuiltinVars,
) -> String {
    expand(value, config, vars)
}

/// The set of keys referenced by `$config["key"]` in `text` (for validation).
pub fn referenced_keys(text: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = text;
    while let Some(index) = rest.find("$config[") {
        let after = &rest[index..];
        if let Some(end) = after.find(']') {
            let inner = &after["$config[".len()..end];
            let key = inner.trim_matches(|ch| ch == '"' || ch == '\'');
            keys.push(key.to_string());
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> BuiltinVars {
        BuiltinVars {
            home: PathBuf::from("/home/user"),
            project_dir: PathBuf::from("/work/blinky"),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            toolchain_dir: PathBuf::from("/opt/toolchains"),
        }
    }

    #[test]
    fn expands_config_key() {
        let mut config = ConfigKeys::default();
        config.set("cc_bin_path", "/usr/bin/gcc");
        let vars = vars();
        assert_eq!(
            expand("$config[\"cc_bin_path\"] --version", &config, &vars),
            "/usr/bin/gcc --version"
        );
    }

    #[test]
    fn expands_builtin_vars() {
        let config = ConfigKeys::default();
        let vars = vars();
        assert_eq!(
            expand("$home/.cargo", &config, &vars),
            "/home/user/.cargo"
        );
        assert_eq!(expand("$os/$arch", &config, &vars), "linux/x86_64");
        assert_eq!(
            expand("$project_dir/build", &config, &vars),
            "/work/blinky/build"
        );
    }

    #[test]
    fn unknown_key_left_verbatim() {
        let config = ConfigKeys::default();
        let vars = vars();
        let text = "$config[\"missing\"]/bin";
        assert_eq!(expand(text, &config, &vars), text);
    }

    #[test]
    fn referenced_keys_collected() {
        let text = "a $config[\"x\"] b $config['y'] c";
        assert_eq!(referenced_keys(text), vec!["x", "y"]);
    }

    #[test]
    fn empty_key_is_removed() {
        let mut config = ConfigKeys::default();
        config.set("k", "value");
        config.set("k", "");
        assert!(config.get("k").is_none());
    }
}
