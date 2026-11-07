//! Build planning and output parsing.
//!
//! The build module never hardcodes a compiler: every command is derived from
//! `project.toml` and executed as an external process.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ns_project::{Language, Project};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPlan {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub artifact: PathBuf,
}

impl BuildPlan {
    pub fn command_line(&self) -> String {
        let mut parts = Vec::with_capacity(self.args.len() + 1);
        parts.push(shell_quote(&self.program));
        for arg in &self.args {
            parts.push(shell_quote(arg));
        }
        parts.join(" ")
    }
}

#[derive(Debug, Clone)]
pub struct BuildOutcome {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
    pub plan: BuildPlan,
}

impl BuildOutcome {
    pub fn diagnostics(&self, root: &Path) -> Vec<Diagnostic> {
        let mut diagnostics = parse_diagnostics(&self.stderr, root);
        diagnostics.extend(parse_diagnostics(&self.stdout, root));
        diagnostics.dedup_by(|a, b| {
            a.path == b.path && a.line == b.line && a.column == b.column && a.message == b.message
        });
        diagnostics
    }

    pub fn combined_output(&self) -> String {
        let mut output = String::new();
        if !self.stdout.trim().is_empty() {
            output.push_str(self.stdout.trim_end());
            output.push('\n');
        }
        if !self.stderr.trim().is_empty() {
            output.push_str(self.stderr.trim_end());
            output.push('\n');
        }
        if output.is_empty() {
            output.push_str("(无输出)\n");
        }
        output
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "note",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: Option<PathBuf>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub severity: Severity,
    pub message: String,
    pub raw: String,
}

impl Diagnostic {
    pub fn location_label(&self) -> String {
        match (&self.path, self.line, self.column) {
            (Some(path), Some(line), Some(column)) => {
                format!("{}:{}:{}", path.display(), line, column)
            }
            (Some(path), Some(line), None) => format!("{}:{}", path.display(), line),
            (Some(path), None, _) => path.display().to_string(),
            _ => "构建输出".to_string(),
        }
    }
}

pub fn plan_build(project: &Project) -> Result<BuildPlan> {
    let language = project.config.project.language;
    let compiler = resolve_compiler(project);
    if compiler.is_empty() {
        bail!(
            "未找到 {} 编译器，请在 project.toml 的 project.compiler 或 compiler.path 中指定",
            language
        );
    }

    let artifact = project.artifact_path();
    let out_dir = artifact
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| project.build_dir());
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("创建构建目录 {} 失败", out_dir.display()))?;

    match language {
        Language::C | Language::Cpp => {
            plan_native(project, &compiler, artifact)
        }
        Language::Rust => plan_rust(project, &compiler, out_dir, artifact),
        Language::Zig => plan_zig(project, &compiler, artifact),
        Language::ZR => plan_zr(project, &compiler, artifact),
    }
}
/// Plan a static-analysis run for the project's language. Returns `None` when
/// no analyzer for the language is found on PATH, so the IDE can report the
/// tool as unavailable rather than fail the run.
pub fn plan_static_analysis(project: &Project) -> Option<BuildPlan> {
    let language = project.config.project.language;
    let sources = project.source_files().ok()?;
    match language {
        Language::C | Language::Cpp => {
            let clang_tidy = find_program("clang-tidy")?;
            let mut args = vec![];
            for source in &sources {
                args.push(source.display().to_string());
            }
            args.push(format!("--std={}", project.config.compiler.std));
            for define in &project.config.compiler.defines {
                args.push(format!("-D{define}"));
            }
            for include in &project.config.compiler.includes {
                let include = project.root.join(include);
                args.push(format!("-I{}", include.display()));
            }
            Some(BuildPlan {
                program: clang_tidy,
                args,
                cwd: project.root.clone(),
                env: vec![],
                artifact: project.build_dir(),
            })
        }
        Language::Rust => {
            let cargo = find_program("cargo")?;
            Some(BuildPlan {
                program: cargo,
                args: vec!["clippy".into(), "--message-format=short".into()],
                cwd: project.root.clone(),
                env: vec![],
                artifact: project.build_dir(),
            })
        }
        Language::Zig => {
            let zig = find_program("zig")?;
            let entry = project.entry_file();
            let mut args = vec!["build-exe".into(), entry.display().to_string()];
            args.push(format!("-fno-emit-bin={}", project.build_dir().display()));
            Some(BuildPlan {
                program: zig,
                args,
                cwd: project.root.clone(),
                env: vec![],
                artifact: project.build_dir(),
            })
        }
        Language::ZR => find_program("zr-lint").map(|zr_lint| BuildPlan {
            program: zr_lint,
            args: sources
                .iter()
                .map(|source| source.display().to_string())
                .collect(),
            cwd: project.root.clone(),
            env: vec![],
            artifact: project.build_dir(),
        }),
    }
}

fn find_program(name: &str) -> Option<String> {
    let path = ns_project::toolchain::find_command(name)?;
    Some(path.display().to_string())
}

fn plan_native(project: &Project, compiler: &str, artifact: PathBuf) -> Result<BuildPlan> {
    let sources = project.source_files()?;
    if sources.is_empty() {
        bail!("src/ 下没有找到源文件");
    }

    let mut args = Vec::new();
    let compiler_name = Path::new(compiler)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(compiler)
        .to_ascii_lowercase();

    // `zig cc` / `zig c++` are useful host cross-compilers.
    if compiler_name == "zig" {
        args.push(if project.config.project.language == Language::Cpp {
            "c++".to_string()
        } else {
            "cc".to_string()
        });
    }

    let std = if project.config.compiler.std.trim().is_empty() {
        project.config.project.language.default_std()
    } else {
        project.config.compiler.std.trim()
    };
    if !std.is_empty() {
        args.push(format!("-std={std}"));
    }

    let opt = project.config.compiler.opt.trim();
    if !opt.is_empty() {
        args.extend(opt.split_whitespace().map(str::to_string));
    }
    if project.config.build.config == "debug" {
        args.push("-g".to_string());
    }

    for define in &project.config.compiler.defines {
        let define = define.trim();
        if !define.is_empty() {
            args.push(format!("-D{define}"));
        }
    }

    for include in &project.config.compiler.includes {
        let include = include.trim();
        if include.is_empty() {
            continue;
        }
        let path = if Path::new(include).is_absolute() {
            PathBuf::from(include)
        } else {
            project.root.join(include)
        };
        args.push(format!("-I{}", path.display()));
    }

    for source in &sources {
        args.push(source.display().to_string());
    }
    args.push("-o".to_string());
    args.push(artifact.display().to_string());

    Ok(BuildPlan {
        program: compiler.to_string(),
        args,
        cwd: project.root.clone(),
        env: Vec::new(),
        artifact,
    })
}

fn plan_rust(
    project: &Project,
    compiler: &str,
    _out_dir: PathBuf,
    artifact: PathBuf,
) -> Result<BuildPlan> {
    let mut args = Vec::new();
    let target_dir = project.build_dir();
    // If the user configured rustc, fall back to directly compiling main.rs.
    let is_cargo = Path::new(compiler)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.eq_ignore_ascii_case("cargo") || name.eq_ignore_ascii_case("cargo.exe"))
        .unwrap_or(false);

    if is_cargo {
        args.push("build".to_string());
        args.push("--target-dir".to_string());
        args.push(target_dir.display().to_string());
        if project.config.build.config == "release" {
            args.push("--release".to_string());
        }
    } else {
        let entry = project.entry_file();
        args.push(entry.display().to_string());
        args.push("-o".to_string());
        args.push(artifact.display().to_string());
        if project.config.build.config == "debug" {
            args.push("-C".to_string());
            args.push("debuginfo=2".to_string());
        }
        for define in &project.config.compiler.defines {
            args.push("--cfg".to_string());
            args.push(define.clone());
        }
    }

    Ok(BuildPlan {
        program: compiler.to_string(),
        args,
        cwd: project.root.clone(),
        env: vec![("CARGO_TARGET_DIR".into(), target_dir.display().to_string())],
        artifact,
    })
}

fn plan_zig(project: &Project, compiler: &str, artifact: PathBuf) -> Result<BuildPlan> {
    let entry = project.entry_file();
    let mut args = vec![
        "build-exe".to_string(),
        entry.display().to_string(),
        format!("-femit-bin={}", artifact.display()),
        "-O".to_string(),
        if project.config.build.config == "release" {
            "ReleaseFast".to_string()
        } else {
            "Debug".to_string()
        },
    ];
    for define in &project.config.compiler.defines {
        args.push(format!("-D{define}"));
    }
    Ok(BuildPlan {
        program: compiler.to_string(),
        args,
        cwd: project.root.clone(),
        env: Vec::new(),
        artifact,
    })
}

fn plan_zr(project: &Project, compiler: &str, artifact: PathBuf) -> Result<BuildPlan> {
    let entry = project.entry_file();
    Ok(BuildPlan {
        program: compiler.to_string(),
        args: vec![
            "build".to_string(),
            entry.display().to_string(),
            "-o".to_string(),
            artifact.display().to_string(),
        ],
        cwd: project.root.clone(),
        env: Vec::new(),
        artifact,
    })
}

pub fn resolve_compiler(project: &Project) -> String {
    let configured = project.config.compiler.path.trim();
    if !configured.is_empty() {
        // Expand `$config["key"]` / built-in variables for cross-platform paths.
        let expanded = project.expand_path(configured);
        if !expanded.is_empty() {
            return expanded;
        }
    }
    let named = project.config.project.compiler.trim();
    if !named.is_empty() {
        return named.to_string();
    }
    ns_project::toolchain::first_available(&ns_project::toolchain::compilers(
        project.config.project.language,
    ))
    .map(|tool| tool.name.clone())
    .unwrap_or_default()
}

pub fn execute(plan: &BuildPlan) -> BuildOutcome {
    let started = Instant::now();
    if let Some(parent) = plan.artifact.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let result = Command::new(&plan.program)
        .args(&plan.args)
        .current_dir(&plan.cwd)
        .envs(plan.env.iter().map(|(key, value)| (key, value)))
        .output();

    match result {
        Ok(output) => BuildOutcome {
            success: output.status.success(),
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            duration: started.elapsed(),
            plan: plan.clone(),
        },
        Err(error) => BuildOutcome {
            success: false,
            code: None,
            stdout: String::new(),
            stderr: format!(
                "无法启动构建命令: {}\n命令: {}\n原因: {}",
                plan.program,
                plan.command_line(),
                error
            ),
            duration: started.elapsed(),
            plan: plan.clone(),
        },
    }
}

/// Parse GCC/Clang and Cargo style diagnostics from build output.
pub fn parse_diagnostics(text: &str, root: &Path) -> Vec<Diagnostic> {
    let mut result: Vec<Diagnostic> = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        if line.trim().is_empty() {
            continue;
        }

        if let Some(rest) = line.trim().strip_prefix("--> ") {
            if let Some((path, line_no, column)) = parse_location(rest) {
                if let Some(last) = result.last_mut() {
                    if last.path.is_none() {
                        last.path = Some(resolve_path(path, root));
                        last.line = Some(line_no);
                        last.column = column;
                    }
                }
            }
            continue;
        }

        if let Some(diagnostic) = parse_gcc_diagnostic(line, root) {
            result.push(diagnostic);
            continue;
        }

        let trimmed = line.trim_start();
        let lower = trimmed.to_ascii_lowercase();
        let severity = if lower.starts_with("error") || lower.starts_with("fatal") {
            Some(Severity::Error)
        } else if lower.starts_with("warning") {
            Some(Severity::Warning)
        } else {
            None
        };
        if let Some(severity) = severity {
            let message = trimmed
                .split_once(':')
                .map(|(_, rest)| rest.trim())
                .unwrap_or(trimmed)
                .to_string();
            if !message.is_empty() {
                result.push(Diagnostic {
                    path: None,
                    line: None,
                    column: None,
                    severity,
                    message,
                    raw: line.to_string(),
                });
            }
        }
    }

    result.dedup_by(|a, b| {
        a.path == b.path && a.line == b.line && a.column == b.column && a.message == b.message
    });
    result
}

fn parse_gcc_diagnostic(line: &str, root: &Path) -> Option<Diagnostic> {
    let markers = [
        (": fatal error:", Severity::Error),
        (": error:", Severity::Error),
        (": warning:", Severity::Warning),
        (": note:", Severity::Info),
    ];
    let mut found: Option<(usize, usize, Severity)> = None;
    for (marker, severity) in markers {
        if let Some(index) = line.find(marker) {
            let end = index + marker.len();
            let replace = found.map(|(start, _, _)| index < start).unwrap_or(true);
            if replace {
                found = Some((index, end, severity));
            }
        }
    }
    let (index, end, severity) = found?;
    let prefix = &line[..index];
    let message = line[end..].trim().to_string();
    if message.is_empty() {
        return None;
    }

    let (path, line_no, column) = parse_prefix_location(prefix)?;
    Some(Diagnostic {
        path: Some(resolve_path(path, root)),
        line: Some(line_no),
        column,
        severity,
        message,
        raw: line.to_string(),
    })
}

fn parse_prefix_location(prefix: &str) -> Option<(&str, usize, Option<usize>)> {
    let mut parts = prefix.rsplitn(3, ':');
    let last = parts.next()?;
    let second = parts.next();
    let rest = parts.next();

    if let Some(rest) = rest {
        if let (Ok(line), Ok(column)) = (second?.parse::<usize>(), last.parse::<usize>()) {
            if !rest.is_empty() {
                return Some((rest, line, Some(column)));
            }
        }
    }

    let line_no = last.parse::<usize>().ok()?;
    if let Some(path) = second {
        if !path.is_empty() {
            return Some((path, line_no, None));
        }
    }
    None
}

fn parse_location(value: &str) -> Option<(&str, usize, Option<usize>)> {
    let value = value.trim();
    let mut parts = value.rsplitn(3, ':');
    let last = parts.next()?;
    let second = parts.next()?;
    if let Ok(column) = last.parse::<usize>() {
        if let Ok(line) = second.parse::<usize>() {
            let path = parts.next()?;
            return Some((path, line, Some(column)));
        }
    }
    let line = last.parse::<usize>().ok()?;
    Some((second, line, None))
}

fn resolve_path(path: &str, root: &Path) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "-_./=:@".contains(ch))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gcc_errors() {
        let text = "src/main.c:12:5: error: 'x' undeclared (first use in this function)\n\
                    src/main.c:14:9: warning: unused variable 'y'\n\
                    make: *** [all] Error 1\n";
        let diagnostics = parse_diagnostics(text, Path::new("/project"));
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert_eq!(diagnostics[0].line, Some(12));
        assert_eq!(diagnostics[0].column, Some(5));
        assert_eq!(diagnostics[1].severity, Severity::Warning);
        assert!(diagnostics[0].path.as_ref().unwrap().ends_with("src/main.c"));
    }

    #[test]
    fn parses_rust_errors_with_location_hint() {
        let text = "error[E0308]: mismatched types\n  --> src/main.rs:7:18\n   |\n";
        let diagnostics = parse_diagnostics(text, Path::new("/project"));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].line, Some(7));
        assert_eq!(diagnostics[0].column, Some(18));
        assert!(diagnostics[0].path.as_ref().unwrap().ends_with("src/main.rs"));
    }

    #[test]
    fn debug_native_build_includes_debug_info() {
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-debug-build-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\nlanguage = \"C\"\ntarget = \"x86_64\"\ncompiler = \"cc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"c11\"\nopt = \"-O0\"\ndefines = []\nincludes = []\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

        let project = Project::open(&root).unwrap();
        let plan = plan_build(&project).unwrap();
        assert!(plan.args.iter().any(|arg| arg == "-g"), "debug 构建必须带 -g: {:?}", plan.args);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn command_line_quotes_paths_with_spaces() {
        let plan = BuildPlan {
            program: "gcc".into(),
            args: vec!["-o".into(), "/tmp/my project/app.elf".into()],
            cwd: PathBuf::new(),
            env: Vec::new(),
            artifact: PathBuf::new(),
        };
        assert!(plan.command_line().contains("'/tmp/my project/app.elf'"));
    }

    #[test]
    fn static_analysis_plan_uses_language_analyzer() {
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-static-plan-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\nlanguage = \"C\"\ntarget = \"x86_64\"\ncompiler = \"cc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"c11\"\nopt = \"-O0\"\ndefines = []\nincludes = []\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

        let project = Project::open(&root).unwrap();
        // clang-tidy is not guaranteed to be installed; the plan must either
        // return None (tool absent) or a clang-tidy plan. Both are valid.
        if let Some(plan) = plan_static_analysis(&project) {
            assert_eq!(plan.program, find_program("clang-tidy").unwrap());
            assert!(plan.args.iter().any(|arg| arg == "--std=c11"));
        }

        let _ = fs::remove_dir_all(root);
    }
}
