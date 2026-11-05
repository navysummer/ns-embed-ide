use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::language::Language;

pub const CONFIG_FILE: &str = "project.toml";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProjectConfig {
    pub project: ProjectSection,
    pub compiler: CompilerSection,
    pub debugger: DebuggerSection,
    pub flasher: FlasherSection,
    pub build: BuildSection,
    pub sim: SimSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectSection {
    pub name: String,
    pub version: String,
    /// C | C++ | Rust | Zig | ZR
    pub language: Language,
    /// Target MCU, for example `stm32f103c8`.
    pub target: String,
    pub compiler: String,
    pub simulator: String,
    pub debugger: String,
    pub flasher: String,
}

impl Default for ProjectSection {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: "0.1.0".to_string(),
            language: Language::C,
            target: String::new(),
            compiler: String::new(),
            simulator: String::new(),
            debugger: String::new(),
            flasher: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompilerSection {
    /// Empty means: discover a compiler from PATH.
    pub path: String,
    pub std: String,
    pub opt: String,
    pub defines: Vec<String>,
    pub includes: Vec<String>,
}

impl Default for CompilerSection {
    fn default() -> Self {
        Self {
            path: String::new(),
            std: String::new(),
            opt: "-O2".to_string(),
            defines: Vec::new(),
            includes: vec!["include".to_string()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DebuggerSection {
    pub path: String,
    pub interface: String,
    pub server: String,
}

impl Default for DebuggerSection {
    fn default() -> Self {
        Self {
            path: String::new(),
            interface: "swd".to_string(),
            server: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FlasherSection {
    pub path: String,
    pub interface: String,
}

impl Default for FlasherSection {
    fn default() -> Self {
        Self {
            path: String::new(),
            interface: "swd".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildSection {
    /// debug | release
    pub config: String,
    pub out_dir: String,
}

impl Default for BuildSection {
    fn default() -> Self {
        Self {
            config: "debug".to_string(),
            out_dir: "build".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SimSection {
    pub cpu: String,
    pub period: u64,
}

#[derive(Debug, Clone)]
pub struct NewProjectSpec {
    pub name: String,
    /// Directory that will contain the new project directory.
    pub location: PathBuf,
    pub language: Language,
    pub compiler: String,
    pub simulator: String,
    pub debugger: String,
    pub flasher: String,
    pub target: String,
}

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub config: ProjectConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    pub field: String,
    pub message: String,
}

impl fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl Project {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut root = path.as_ref().to_path_buf();
        if root.is_file() {
            root = root
                .parent()
                .map(Path::to_path_buf)
                .context("无法定位项目目录")?;
        }
        let root = root
            .canonicalize()
            .with_context(|| format!("项目目录不存在: {}", root.display()))?;
        let config_path = root.join(CONFIG_FILE);
        if !config_path.is_file() {
            bail!(
                "{} 中没有 project.toml，无法作为 ns-embed-ide 项目打开",
                root.display()
            );
        }
        let text = fs::read_to_string(&config_path)
            .with_context(|| format!("读取 {} 失败", config_path.display()))?;
        let config: ProjectConfig = toml::from_str(&text)
            .with_context(|| format!("解析 {} 失败", config_path.display()))?;
        let project = Self { root, config };
        if let Some(issue) = project.validate().into_iter().next() {
            bail!("project.toml 校验失败: {issue}");
        }
        Ok(project)
    }

    pub fn create(spec: NewProjectSpec) -> Result<Self> {
        let name = spec.name.trim();
        if name.is_empty() {
            bail!("项目名称不能为空");
        }
        if name
            .chars()
            .any(|ch| matches!(ch, '/' | '\\' | ':' | '\0'))
        {
            bail!("项目名称不能包含路径分隔符");
        }

        fs::create_dir_all(&spec.location)
            .with_context(|| format!("创建目录 {} 失败", spec.location.display()))?;
        let root = spec.location.join(name);
        if root.exists() {
            let mut entries = fs::read_dir(&root)
                .with_context(|| format!("读取 {} 失败", root.display()))?;
            if entries.next().is_some() {
                bail!("目标目录已存在且不为空: {}", root.display());
            }
        }
        fs::create_dir_all(root.join("src"))?;
        fs::create_dir_all(root.join("include"))?;
        fs::create_dir_all(root.join("build"))?;

        let mut config = ProjectConfig::default();
        config.project.name = name.to_string();
        config.project.version = "0.1.0".to_string();
        config.project.language = spec.language;
        config.project.target = spec.target;
        config.project.compiler = spec.compiler;
        config.project.simulator = spec.simulator;
        config.project.debugger = spec.debugger;
        config.project.flasher = spec.flasher;
        config.compiler.std = spec.language.default_std().to_string();

        let (source_path, source) = spec.language.starter_source();
        write_file(&root.join(source_path), source)?;

        if let Some((path, contents)) = spec.language.starter_build_file() {
            let contents = if spec.language == Language::Rust {
                cargo_toml(name)
            } else {
                contents.to_string()
            };
            write_file(&root.join(path), &contents)?;
        }

        let toml_text = toml::to_string_pretty(&config).context("序列化 project.toml 失败")?;
        write_file(&root.join(CONFIG_FILE), &toml_text)?;
        write_file(&root.join("README.md"), &project_readme(&config))?;
        write_file(
            &root.join(".gitignore"),
            "build/\ntarget/\n.ns-ide/\n.DS_Store\n",
        )?;

        let project = Self {
            root: root.canonicalize()?,
            config,
        };
        Ok(project)
    }

    pub fn save_config(&self) -> Result<()> {
        let text = toml::to_string_pretty(&self.config).context("序列化 project.toml 失败")?;
        write_file(&self.root.join(CONFIG_FILE), &text)
    }

    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        if self.config.project.name.trim().is_empty() {
            issues.push(ValidationIssue {
                field: "project.name".into(),
                message: "项目名称不能为空".into(),
            });
        }
        if self.config.project.version.trim().is_empty() {
            issues.push(ValidationIssue {
                field: "project.version".into(),
                message: "项目版本不能为空".into(),
            });
        }
        if self.config.build.out_dir.trim().is_empty() {
            issues.push(ValidationIssue {
                field: "build.out_dir".into(),
                message: "构建输出目录不能为空".into(),
            });
        }
        issues
    }

    pub fn entry_file(&self) -> PathBuf {
        let (relative, _) = self.config.project.language.starter_source();
        self.root.join(relative)
    }

    pub fn build_dir(&self) -> PathBuf {
        self.root.join(&self.config.build.out_dir)
    }

    pub fn artifact_path(&self) -> PathBuf {
        let name = artifact_name(&self.config.project.name);
        let config = &self.config.build.config;
        match self.config.project.language {
            Language::C | Language::Cpp | Language::Zig | Language::ZR => {
                self.build_dir().join(config).join(format!("{name}.elf"))
            }
            Language::Rust => self.build_dir().join(config).join(name),
        }
    }

    pub fn source_files(&self) -> Result<Vec<PathBuf>> {
        let mut result = Vec::new();
        let src = self.root.join("src");
        collect_sources(&src, self.config.project.language, &mut result)?;
        result.sort();
        Ok(result)
    }

    pub fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    pub fn last_opened(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
    }
}

fn cargo_toml(name: &str) -> String {
    format!(
        r#"[package]
name = "{}"
version = "0.1.0"
edition = "2021"

[dependencies]
"#,
        artifact_name(name)
    )
}

fn artifact_name(name: &str) -> String {
    let mut name = name
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while name.contains("--") {
        name = name.replace("--", "-");
    }
    name.trim_matches('-').to_string()
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建目录 {} 失败", parent.display()))?;
    }
    fs::write(path, contents).with_context(|| format!("写入 {} 失败", path.display()))
}

fn collect_sources(
    dir: &Path,
    language: Language,
    result: &mut Vec<PathBuf>,
) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in
        fs::read_dir(dir).with_context(|| format!("读取目录 {} 失败", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_sources(&path, language, result)?;
            continue;
        }
        let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        let ext = ext.to_ascii_lowercase();
        let matches = match language {
            Language::C => matches!(ext.as_str(), "c"),
            Language::Cpp => matches!(ext.as_str(), "cc" | "cpp" | "cxx"),
            Language::Rust => ext == "rs",
            Language::Zig => ext == "zig",
            Language::ZR => ext == "zr",
        };
        if matches {
            result.push(path);
        }
    }
    Ok(())
}

fn project_readme(config: &ProjectConfig) -> String {
    format!(
        r#"# {}

由 ns-embed-ide 创建。

- 语言: {}
- 目标设备: {}
- 编译器: {}
- 仿真器: {}
- 调试器: {}
- 烧写器: {}

## 构建

在 IDE 中按 `Cmd/Ctrl + B`，或直接使用项目根目录的 `project.toml` 驱动工具链。

所有工具链配置都在 `project.toml` 中，IDE 不硬编码工具路径。
"#,
        config.project.name,
        config.project.language,
        if config.project.target.is_empty() {
            "未指定"
        } else {
            config.project.target.as_str()
        },
        if config.project.compiler.is_empty() {
            "自动发现"
        } else {
            config.project.compiler.as_str()
        },
        if config.project.simulator.is_empty() {
            "未指定"
        } else {
            config.project.simulator.as_str()
        },
        if config.project.debugger.is_empty() {
            "未指定"
        } else {
            config.project.debugger.as_str()
        },
        if config.project.flasher.is_empty() {
            "未指定"
        } else {
            config.project.flasher.as_str()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ns-embed-ide-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn create_and_reopen_c_project() {
        let base = temp_dir("create-c");
        let project = Project::create(NewProjectSpec {
            name: "blinky".into(),
            location: base.clone(),
            language: Language::C,
            compiler: "gcc".into(),
            simulator: "ns-sim".into(),
            debugger: "gdb".into(),
            flasher: "openocd".into(),
            target: "stm32f103c8".into(),
        })
        .unwrap();

        assert!(project.root.join("project.toml").is_file());
        assert!(project.root.join("src/main.c").is_file());
        assert_eq!(project.config.project.language, Language::C);

        let reopened = Project::open(&project.root).unwrap();
        assert_eq!(reopened.config.project.name, "blinky");
        assert_eq!(reopened.config.project.target, "stm32f103c8");
        assert_eq!(
            reopened.config.project.simulator.as_str(),
            "ns-sim"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn reject_non_empty_target() {
        let base = temp_dir("occupied");
        fs::create_dir_all(base.join("taken")).unwrap();
        fs::write(base.join("taken/hello.txt"), "hello").unwrap();
        let result = Project::create(NewProjectSpec {
            name: "taken".into(),
            location: base.clone(),
            language: Language::C,
            compiler: String::new(),
            simulator: String::new(),
            debugger: String::new(),
            flasher: String::new(),
            target: String::new(),
        });
        assert!(result.is_err());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn source_file_discovery() {
        let base = temp_dir("sources");
        let project = Project::create(NewProjectSpec {
            name: "sources".into(),
            location: base.clone(),
            language: Language::C,
            compiler: String::new(),
            simulator: String::new(),
            debugger: String::new(),
            flasher: String::new(),
            target: String::new(),
        })
        .unwrap();
        let sources = project.source_files().unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].ends_with("src/main.c"));
        let _ = fs::remove_dir_all(base);
    }
}
