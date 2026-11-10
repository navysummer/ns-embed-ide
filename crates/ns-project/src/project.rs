use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::language::{CTemplate, Language};

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
    /// Debug launch configuration: arguments, environment and working
    /// directory for the debuggee (the executable being debugged).
    #[serde(default)]
    pub launch: LaunchSection,
    /// Per-target build configurations (optional). Each entry ties a target
    /// MCU name to its build `config`; when the project's active `target`
    /// matches an entry, that config is used for builds. Empty = single-target
    /// project using `build.config`.
    #[serde(default)]
    pub targets: Vec<TargetSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetSection {
    /// Target MCU name (matches `ProjectSection::target`).
    pub target: String,
    /// Build config for this target: debug | release.
    pub config: String,
}

impl Default for TargetSection {
    fn default() -> Self {
        Self {
            target: String::new(),
            config: "debug".to_string(),
        }
    }
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
    /// SVD device description source: a built-in catalog key (e.g.
    /// `stm32f103c8`) or a path to an external CMSIS-SVD file.
    pub svd: String,
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
            svd: String::new(),
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
    /// Debug probe kind: `openocd` | `stlink` | `jlink` | `pyocd` | `probe-rs`.
    /// Empty = auto-detect (OpenOCD). Drives embedded-target attach.
    pub probe: String,
    /// GDB client path for embedded attach (arm-none-eabi-gdb / gdb-multiarch).
    pub gdb: String,
}

impl Default for DebuggerSection {
    fn default() -> Self {
        Self {
            path: String::new(),
            interface: "swd".to_string(),
            server: String::new(),
            probe: "openocd".to_string(),
            gdb: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FlasherSection {
    /// Adapter kind: `openocd` | `pyocd` | `jlink`. Empty = auto-detect.
    pub kind: String,
    pub path: String,
    /// Debug interface, e.g. `swd` or `jtag`.
    pub interface: String,
    /// Optional external flash-algorithm file (`.FLM`). When set, the flash
    /// geometry (device size / page size / base) is parsed from it and used
    /// for the flash programming configuration.
    #[serde(default)]
    pub algorithm: String,
}

impl Default for FlasherSection {
    fn default() -> Self {
        Self {
            kind: "openocd".to_string(),
            path: String::new(),
            interface: "swd".to_string(),
            algorithm: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildSection {
    /// debug | release
    pub config: String,
    pub out_dir: String,
    /// Build-system strategy for C/C++ projects: `auto` (detect a
    /// CMakeLists.txt or Makefile, falling back to direct compilation),
    /// `direct` (compile all sources in one command), `make`, or `cmake`.
    pub system: String,
    /// Number of parallel build jobs. 0 = auto-detect (available CPUs, capped
    /// at a sane maximum). Drives `-j` for CMake/Make and `cargo build -j`.
    #[serde(default)]
    pub jobs: u32,
}

impl Default for BuildSection {
    fn default() -> Self {
        Self {
            config: "debug".to_string(),
            out_dir: "build".to_string(),
            system: "auto".to_string(),
            jobs: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SimSection {
    pub cpu: String,
    pub period: u64,
}

/// Debug launch configuration (`[launch]` in `project.toml`): the arguments,
/// environment variables and working directory passed to the debuggee when a
/// debug session starts. An empty working directory means the project root.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LaunchSection {
    /// Command-line arguments passed to the debuggee, one per entry.
    pub args: Vec<String>,
    /// Environment variables as `KEY=value` entries applied on top of the
    /// inherited environment when the debuggee starts.
    pub env: Vec<String>,
    /// Working directory for the debuggee. Empty = the project root.
    pub cwd: String,
}

impl Default for LaunchSection {
    fn default() -> Self {
        Self {
            args: Vec::new(),
            env: Vec::new(),
            cwd: String::new(),
        }
    }
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
    /// SVD source: built-in catalog key or external file path.
    pub svd: String,
    /// C starter template selection (ignored for non-C languages).
    pub c_template: Option<CTemplate>,
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
        config.project.svd = spec.svd;
        config.project.compiler = spec.compiler;
        config.project.simulator = spec.simulator;
        config.project.debugger = spec.debugger;
        config.project.flasher = spec.flasher;
        config.compiler.std = spec.language.default_std().to_string();

        let (source_path, source) = spec
            .language
            .starter_source_for_template(spec.c_template);
        write_file(&root.join(source_path), source)?;

        if let Some((path, contents)) = spec.language.starter_build_file() {
            let contents = if spec.language == Language::Rust {
                cargo_toml(name)
            } else if spec.language == Language::C || spec.language == Language::Cpp {
                // Substitute the project name so CMake emits the expected
                // artifact name (`artifact_name` uses the project name).
                contents.replace("ns-app", &artifact_name(name))
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

    /// Non-fatal warnings about configured tools not found on PATH, each with
    /// an install hint when known. These do not prevent opening the project.
    pub fn tool_warnings(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        for (field, tool) in [
            ("project.compiler", self.config.project.compiler.as_str()),
            ("project.simulator", self.config.project.simulator.as_str()),
            ("project.debugger", self.config.project.debugger.as_str()),
            ("project.flasher", self.config.project.flasher.as_str()),
        ] {
            if tool.trim().is_empty() {
                continue;
            }
            if crate::toolchain::find_command(tool).is_none() {
                let guide = crate::toolchain::missing_guide(tool)
                    .map(|g| format!("。安装建议: {g}"))
                    .unwrap_or_default();
                issues.push(ValidationIssue {
                    field: field.into(),
                    message: format!("配置的工具「{tool}」未在 PATH 中找到{guide}"),
                });
            }
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

    /// The effective build config for the project's active target.
    ///
    /// When a per-target entry in `targets` matches the active `target`, that
    /// entry's config wins; otherwise the shared `build.config` applies.
    pub fn build_config(&self) -> &str {
        let target = self.config.project.target.trim();
        if !target.is_empty() {
            if let Some(entry) = self
                .config
                .targets
                .iter()
                .find(|entry| entry.target.trim() == target)
            {
                if !entry.config.trim().is_empty() {
                    return entry.config.trim();
                }
            }
        }
        &self.config.build.config
    }

    /// Number of parallel build jobs: the configured `[build] jobs` value, or
    /// an auto-detected default (available CPUs, capped at 32) when 0.
    pub fn build_jobs(&self) -> u32 {
        let jobs = self.config.build.jobs;
        if jobs > 0 {
            return jobs;
        }
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        (cpus as u32).clamp(1, 32)
    }

    pub fn artifact_path(&self) -> PathBuf {
        let name = artifact_name(&self.config.project.name);
        let config = self.build_config();
        let target = self.config.project.target.trim().to_lowercase();
        // SDCC / vendor-HEX targets (MCS-51, 80251, STM8, C166) produce Intel
        // HEX; the ELF targets produce `.elf`. C166 needs a vendor toolchain
        // (build is gated), but its artifact is still HEX for consistency.
        let is_hex_target = target.contains("mcs51")
            || target.contains("8051")
            || target.contains("c51")
            || target.contains("80251")
            || target.contains("mcs251")
            || target.contains("c251")
            || target.contains("stm8")
            || target.contains("c166")
            || target.contains("xc166")
            || target.contains("xc2000")
            || target.contains("st10");
        match self.config.project.language {
            Language::C | Language::Cpp | Language::Zig | Language::ZR | Language::Asm => {
                let ext = if is_hex_target { "ihx" } else { "elf" };
                self.build_dir().join(config).join(format!("{name}.{ext}"))
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

    /// Resolve a path-like string field from `project.toml`, expanding
    /// `$config["key"]` references and built-in variables so config files are
    /// portable across machines and platforms.
    pub fn expand_path(&self, value: &str) -> String {
        let config = crate::ConfigKeys::load();
        let vars = crate::BuiltinVars::for_project(&self.root);
        crate::expand(value, &config, &vars)
    }

    /// Resolve the configured tool path for a compiler/debugger/flasher field,
    /// expanding config references and built-in variables, then resolving
    /// relative paths against the project root.
    pub fn resolve_configured_path(&self, value: &str) -> PathBuf {
        let expanded = self.expand_path(value);
        let path = PathBuf::from(&expanded);
        if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        }
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
            Language::Asm => matches!(ext.as_str(), "s" | "asm" | "inc"),
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
            svd: "stm32f103c8".into(),
            c_template: None,
        })
        .unwrap();

        assert!(project.root.join("project.toml").is_file());
        assert!(project.root.join("src/main.c").is_file());
        // C/C++ wizard projects get a CMakeLists.txt so `[build] system = "auto"`
        // resolves to a CMake build instead of one-shot direct compilation.
        assert!(project.root.join("CMakeLists.txt").is_file());
        let cmake = fs::read_to_string(project.root.join("CMakeLists.txt")).unwrap();
        assert!(cmake.contains("blinky"), "CMake project name: {cmake}");
        assert_eq!(project.config.project.language, Language::C);
        assert_eq!(project.config.build.system, "auto");

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
            svd: String::new(),
            c_template: None,
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
            svd: String::new(),
            c_template: None,
        })
        .unwrap();
        let sources = project.source_files().unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].ends_with("src/main.c"));
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn validate_flags_missing_tool_with_guide() {
        let base = temp_dir("toolcheck");
        let project = Project::create(NewProjectSpec {
            name: "toolcheck".into(),
            location: base.clone(),
            language: Language::C,
            compiler: "definitely-not-a-real-compiler-xyz".into(),
            simulator: String::new(),
            debugger: String::new(),
            flasher: String::new(),
            target: String::new(),
            svd: String::new(),
            c_template: None,
        })
        .unwrap();
        let issues = project.tool_warnings();
        let tool_issue = issues
            .iter()
            .find(|issue| issue.field == "project.compiler")
            .expect("missing tool should be reported");
        assert!(tool_issue.message.contains("未在 PATH 中找到"));
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn validate_passes_when_tool_configured_and_present() {
        let base = temp_dir("toolok");
        let project = Project::create(NewProjectSpec {
            name: "toolok".into(),
            location: base.clone(),
            language: Language::C,
            compiler: if cfg!(unix) { "sh".into() } else { String::new() },
            simulator: String::new(),
            debugger: String::new(),
            flasher: String::new(),
            target: String::new(),
            svd: String::new(),
            c_template: None,
        })
        .unwrap();
        let issues = project.tool_warnings();
        assert!(
            !issues.iter().any(|issue| issue.field == "project.compiler"),
            "a present tool must not be flagged"
        );
        let _ = fs::remove_dir_all(base);
    }

    /// Settings modal edits `project.toml` fields; prove that saving then
    /// reopening preserves them (the exact path `save_settings` exercises).
    #[test]
    fn settings_save_reload_round_trip() {
        let base = temp_dir("settings");
        let mut project = Project::create(NewProjectSpec {
            name: "cfgproj".into(),
            location: base.clone(),
            language: Language::C,
            compiler: String::new(),
            simulator: String::new(),
            debugger: String::new(),
            flasher: String::new(),
            target: String::new(),
            svd: String::new(),
            c_template: None,
        })
        .unwrap();

        // Mimic `save_settings` field writes.
        project.config.project.name = "renamed".to_string();
        project.config.project.target = "stm32f103c8".to_string();
        project.config.compiler.path = "/opt/toolchain/bin/gcc".to_string();
        project.config.build.config = "release".to_string();
        project.config.build.out_dir = "out".to_string();
        project.save_config().unwrap();
        // Debug launch configuration round-trips through project.toml.
        project.config.launch.args = vec!["--port".to_string(), "8080".to_string()];
        project.config.launch.env = vec!["DEBUG=1".to_string()];
        project.config.launch.cwd = "run".to_string();
        project.save_config().unwrap();

        let reopened = Project::open(base.join("cfgproj").join(CONFIG_FILE)).unwrap();
        assert_eq!(reopened.config.project.name, "renamed");
        assert_eq!(reopened.config.project.target, "stm32f103c8");
        assert_eq!(reopened.config.compiler.path, "/opt/toolchain/bin/gcc");
        assert_eq!(reopened.config.build.config, "release");
        assert_eq!(reopened.config.build.out_dir, "out");
        assert_eq!(reopened.config.launch.args, vec!["--port", "8080"]);
        assert_eq!(reopened.config.launch.env, vec!["DEBUG=1"]);
        assert_eq!(reopened.config.launch.cwd, "run");


        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn c_template_generates_matching_source() {
        for template in [
            CTemplate::Empty,
            CTemplate::LedBlink,
            CTemplate::UartLoopback,
            CTemplate::Freertos,
        ] {
            let base = temp_dir("ctemplate");
            let project = Project::create(NewProjectSpec {
                name: "ctpl".into(),
                location: base.clone(),
                language: Language::C,
                compiler: String::new(),
                simulator: String::new(),
                debugger: String::new(),
                flasher: String::new(),
                target: String::new(),
                svd: String::new(),
                c_template: Some(template),
            })
            .unwrap();
            let source = std::fs::read_to_string(project.root.join("src/main.c")).unwrap();
            match template {
                CTemplate::Empty => assert!(source.contains("counter++")),
                CTemplate::LedBlink => assert!(source.contains("GPIOB_ODR")),
                CTemplate::UartLoopback => assert!(source.contains("USART1_DR")),
                CTemplate::Freertos => {
                    assert!(source.contains("scheduler_add"));
                    assert!(source.contains("task_led"));
                    assert!(source.contains("task_monitor"));
                }
            }
            let _ = fs::remove_dir_all(base);
        }
    }

    /// SDCC-based targets (MCS-51/80251/STM8) build Intel HEX; ELF targets
    /// build `.elf`. The artifact path must match so the IDE finds the build
    /// output for each simulated architecture.
    #[test]
    fn artifact_path_matches_build_output_format() {
        let base = temp_dir("artifact-path");
        for (target, expect_ext) in [
            ("mcs51", "ihx"),
            ("80251", "ihx"),
            ("stm8s003f3", "ihx"),
            ("c166", "ihx"),
            ("xc2000", "ihx"),
            ("st10", "ihx"),
            ("stm32f103c8", "elf"),
            ("atmega328p", "elf"),
        ] {
            // Use the target as the project name so each iteration creates its
            // own directory (a non-empty `location/t` would fail on create).
            let project = Project::create(NewProjectSpec {
                name: target.replace(|c: char| !c.is_ascii_alphanumeric(), "-"),
                location: base.clone(),
                language: Language::C,
                compiler: "gcc".into(),
                simulator: "ns-sim".into(),
                debugger: "gdb".into(),
                flasher: "openocd".into(),
                target: target.into(),
                svd: target.into(),
                c_template: None,
            })
            .unwrap();
            let path = project.artifact_path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            assert_eq!(ext, expect_ext, "target {target}: artifact ext must be {expect_ext}");
        }
        let _ = fs::remove_dir_all(base);
    }

    /// Per-target build config resolution: a matching `targets` entry wins,
    /// otherwise the shared `build.config` applies.
    #[test]
    fn build_config_resolves_per_target_override() {
        let base = temp_dir("build_config");
        let mut project = Project::create(NewProjectSpec {
            name: "cfgproj".into(),
            location: base.clone(),
            language: Language::C,
            compiler: String::new(),
            simulator: String::new(),
            debugger: String::new(),
            flasher: String::new(),
            target: "stm32f103c8".into(),
            svd: String::new(),
            c_template: None,
        })
        .unwrap();
        // Shared default: debug.
        assert_eq!(project.build_config(), "debug");
        // Add a per-target override for the active target.
        project.config.targets.push(TargetSection {
            target: "stm32f103c8".into(),
            config: "release".into(),
        });
        assert_eq!(project.build_config(), "release");
        // A different target falls back to the shared config.
        project.config.project.target = "nrf52840".into();
        assert_eq!(project.build_config(), "debug");
        // Round-trip through save/open preserves the targets array.
        project.save_config().unwrap();
        let reopened = Project::open(base.join("cfgproj")).unwrap();
        assert_eq!(reopened.config.targets.len(), 1);
        assert_eq!(reopened.config.targets[0].target, "stm32f103c8");
        assert_eq!(reopened.build_config(), "debug");
        let _ = fs::remove_dir_all(base);
    }
}
