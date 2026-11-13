//! Build planning and output parsing.
//!
//! The build module never hardcodes a compiler: every command is derived from
//! `project.toml` and executed as an external process.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc::RecvTimeoutError};
use std::time::{Duration, Instant};

 use anyhow::{Context, Result, bail};
 use ns_project::{Language, Project};
 
 pub mod layout;
 
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
    /// Estimated per-source-file compile time, parsed from the build output.
    /// Only populated when the toolchain prints per-file progress (Make/CMake
    /// `Building ...` lines) or per-file `-c` invocations; otherwise empty.
    pub file_timings: Vec<FileTiming>,
}

/// Estimated wall-clock spent compiling one source file, derived from the
/// build output's per-file markers. The toolchain may print each source as it
/// starts; the gap to the next marker (or the end of the build) is the
/// estimate. `elapsed_ms` is the time attributed to that file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTiming {
    pub file: String,
    pub elapsed_ms: u64,
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

/// A suggested quick fix for a build diagnostic that does not require a
/// language server (missing include, undefined reference, undeclared
/// identifier, missing semicolon). `kind` tells the IDE how to apply it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickFix {
    /// Insert an `#include <...>` line at the top of the file.
    AddInclude { header: String },
    /// Add a linker flag (e.g. `-lm`) to the build's linker flags.
    AddLinkFlag { flag: String },
    /// A human-readable hint with no automatic edit.
    Hint(String),
}

/// Suggest a quick fix for a single build diagnostic. `source_text` is the
/// source file's current contents (used to check whether an include already
/// exists); pass `None` when the file cannot be read.
pub fn suggest_quick_fix(diagnostic: &Diagnostic, source_text: Option<&str>) -> Option<QuickFix> {
    let message = &diagnostic.message;
    let lower = message.to_lowercase();

    // `undefined reference to `foo`` -> offer to add the library that
    // provides the symbol (`sqrt` -> `-lm`, `pthread_create` -> `-lpthread`),
    // falling back to a hint when the symbol isn't a known libc/libm API.
    if lower.contains("undefined reference") {
        if let Some(symbol) = extract_symbol_after(message, "undefined reference to") {
            if let Some(lib) = library_for_symbol(&symbol) {
                return Some(QuickFix::AddLinkFlag {
                    flag: format!("-l{lib}"),
                });
            }
            return Some(QuickFix::Hint(format!(
                "未定义引用 `{symbol}`：检查是否缺少链接库（如 `-l` 标志）或该符号所在的源文件未参与构建。"
            )));
        }
        return Some(QuickFix::Hint(
            "未定义引用：检查链接器库标志（`[compiler]` 的链接参数）与源文件是否全部参与构建。".to_string(),
        ));
    }


    // `fatal error: xxx.h: No such file or directory` -> AddInclude.
    if lower.contains("no such file or directory")
        || (lower.contains("fatal error") && lower.contains(".h"))
    {
        let header = message
            .split_whitespace()
             .find(|token| token.contains(".h"))
             .map(|token| {
                token.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '_' && c != '/')
             })
             .unwrap_or_default();
        if !header.is_empty() {
            let already = source_text
                .map(|text| text.contains(&format!("<{header}>")))
                .unwrap_or(false);
            if !already {
                return Some(QuickFix::AddInclude {
                    header: header.to_string(),
                });
            }
            return Some(QuickFix::Hint(format!(
                "头文件 `<{header}>` 已包含但仍报错：检查 include 路径（`[compiler]` 的 includes 或安装位置）。"
            )));
        }
    }

    // `'foo' undeclared (first use in this function)` -> declaration hint.
    if lower.contains("undeclared") || lower.contains("was not declared") {
        let symbol = extract_symbol_after(message, "‘")
            .or_else(|| extract_symbol_after(message, "'"))
            .or_else(|| extract_symbol_after(message, "`"))
            .unwrap_or_default();
        if !symbol.is_empty() {
            // Standard-library identifiers are almost always a missing
            // `#include`; offer to add the conventional header. This is an
            // edit (like AddInclude) rather than a hint, so it fixes the
            // error in one click instead of explaining what to type.
            if let Some(header) = header_for_symbol(&symbol) {
                let already = source_text
                    .map(|text| text.contains(&format!("#include <{header}>")))
                    .unwrap_or(false);
                if !already {
                    return Some(QuickFix::AddInclude {
                        header: header.to_string(),
                    });
                }
                return Some(QuickFix::Hint(format!(
                    "标识符 `{symbol}` 未声明：`<{header}>` 已包含但仍报错，检查宏定义或平台头文件。"
                )));
            }
            return Some(QuickFix::Hint(format!(
                "标识符 `{symbol}` 未声明：检查拼写、是否缺少 `#include`，或需在使用前声明/定义。"
            )));
        }
    }


    // `expected ';' before ...` -> missing semicolon hint.
    if lower.contains("expected ';'") {
        return Some(QuickFix::Hint(
            "缺少分号：上一行语句结尾可能是 `;`（或宏展开后缺 `;`）。".to_string(),
        ));
    }

    None
}

/// Extract the quoted identifier/symbol after a marker in a diagnostic message.
fn extract_symbol_after(message: &str, marker: &str) -> Option<String> {
    let idx = message.find(marker)?;
    let rest = &message[idx + marker.len()..];
     let symbol: String = rest
         .trim_start()
        .trim_start_matches(|c: char| !c.is_alphanumeric() && c != '_')
         .chars()
         .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | ':' | '$'))
         .collect();
    if symbol.is_empty() {
        None
    } else {
        Some(symbol)
    }
}

/// Map a linker symbol to the conventional library that provides it, so an
/// `undefined reference to 'foo'` can be fixed by adding `-l<lib>` to the
/// link flags. Returns `None` for symbols with no conventional library (e.g.
/// project-defined functions, which need their defining source to be built).
fn library_for_symbol(symbol: &str) -> Option<&'static str> {
    // libm: math functions (sqrt, sin, cos, pow, log, exp, floor, ceil, …).
    const LIBM: &[&str] = &[
        "sqrt", "sqrtf", "sqrtl", "sin", "sinf", "cos", "cosf", "tan", "tanf", "asin", "acos",
        "atan", "atan2", "pow", "powf", "log", "logf", "log10", "exp", "expf", "floor", "ceil",
        "fmod", "fmodf", "fabs", "fabsf", "round", "roundf", "trunc", "truncf", "ldexp", "frexp",
        "modf", "hypot", "hypotf", "cbrt", "erf", "erff", "tanh", "sinh", "cosh", "nan", "nanf",
    ];
    if LIBM.contains(&symbol) {
        return Some("m");
    }
    // libpthread: POSIX threads.
    const LIBPTHREAD: &[&str] = &[
        "pthread_create", "pthread_join", "pthread_mutex_lock", "pthread_mutex_unlock",
        "pthread_mutex_init", "pthread_mutex_destroy", "pthread_cond_wait", "pthread_cond_signal",
        "pthread_cond_broadcast", "pthread_cond_init", "pthread_cond_destroy", "pthread_exit",
        "pthread_detach", "pthread_self", "pthread_equal", "pthread_once",
    ];
    if LIBPTHREAD.contains(&symbol) {
        return Some("pthread");
    }
    // librt: real-time clocks/timers.
    const LIBRT: &[&str] = &[
        "clock_gettime", "clock_settime", "timer_create", "timer_settime", "timer_delete",
        "nanosleep", "clock_nanosleep", "mq_open", "mq_send", "mq_receive", "shm_open", "shm_unlink",
        "sigqueue", "aio_read", "aio_write", "sched_setparam", "sched_getparam",
    ];
    if LIBRT.contains(&symbol) {
        return Some("rt");
    }
    // libdl: dynamic loading.
    const LIBDL: &[&str] = &[
        "dlopen", "dlsym", "dlclose", "dlerror", "dladdr", "dlmopen", "dlinfo", "dlvsym",
    ];
    if LIBDL.contains(&symbol) {
        return Some("dl");
    }
    // libutil: login/tty helpers.
    const LIBUTIL: &[&str] = &["openpty", "forkpty", "login_tty", "login", "logout"];
    if LIBUTIL.contains(&symbol) {
        return Some("util");
    }
    None
}

/// Map an undeclared standard-library identifier to the conventional header
/// that declares it, so a `'foo' undeclared` error can be fixed by adding the
/// missing `#include`. Returns `None` for user-defined symbols.
fn header_for_symbol(symbol: &str) -> Option<&'static str> {
    // C standard library: one header per family of identifiers.
    if matches!(
        symbol,
        "printf" | "fprintf" | "sprintf" | "snprintf" | "scanf" | "fscanf" | "sscanf" | "puts"
            | "putchar" | "getchar" | "fgets" | "fputs" | "fopen" | "fclose" | "fread" | "fwrite"
            | "fseek" | "ftell" | "rewind" | "fgetc" | "fputc" | "feof" | "ferror" | "perror"
            | "remove" | "rename" | "tmpfile" | "printf_s" | "vfprintf" | "vprintf"
            | "stdin" | "stdout" | "stderr" | "FILE" | "fpos_t"
    ) {
        return Some("stdio.h");
    }
    if matches!(
        symbol,
        "malloc" | "calloc" | "realloc" | "free" | "aligned_alloc" | "reallocarray"
    ) {
        return Some("stdlib.h");
    }
    if matches!(
        symbol,
        "strlen" | "strcpy" | "strncpy" | "strcat" | "strncat" | "strcmp" | "strncmp" | "strchr"
            | "strrchr" | "strstr" | "strtok" | "memcpy" | "memmove" | "memset" | "memcmp"
            | "memchr" | "strdup" | "strcasecmp" | "strncasecmp" | "strcoll" | "strxfrm"
    ) {
        return Some("string.h");
    }
    if matches!(
        symbol,
        "sqrt" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2" | "pow" | "log"
            | "log10" | "exp" | "floor" | "ceil" | "fmod" | "fabs" | "round" | "trunc"
            | "M_PI" | "M_E" | "INFINITY" | "NAN"
    ) {
        return Some("math.h");
    }
    if matches!(
        symbol,
        "assert" | "NDEBUG" | "static_assert" | "assert_hdr"
    ) {
        return Some("assert.h");
    }
    if matches!(symbol, "errno" | "errno_t" | "EINVAL" | "ERANGE" | "EDOM") {
        return Some("errno.h");
    }
    if matches!(symbol, "time" | "clock" | "difftime" | "mktime" | "asctime" | "ctime" | "gmtime" | "localtime" | "strftime" | "clock_t" | "time_t" | "tm" | "timespec") {
        return Some("time.h");
    }
    if matches!(symbol, "isalpha" | "isdigit" | "isalnum" | "isspace" | "toupper" | "tolower" | "isupper" | "islower" | "ispunct" | "isxdigit") {
        return Some("ctype.h");
    }
    if matches!(symbol, "qsort" | "bsearch" | "abs" | "labs" | "div" | "ldiv" | "atoi" | "atol" | "atof" | "strtol" | "strtoul" | "strtod" | "rand" | "srand" | "EXIT_SUCCESS" | "EXIT_FAILURE" | "RAND_MAX") {
        return Some("stdlib.h");
    }
    // POSIX threads.
    if symbol.starts_with("pthread_") || matches!(symbol, "pthread_t" | "pthread_mutex_t" | "pthread_cond_t" | "PTHREAD_MUTEX_INITIALIZER" | "PTHREAD_COND_INITIALIZER") {
        return Some("pthread.h");
    }
    None
}

pub fn plan_build(project: &Project) -> Result<BuildPlan> {
    let language = project.config.project.language;
    let target = project.config.project.target.trim().to_lowercase();
    // 8051-family targets (MCS-51 and the binary-compatible 80251) use SDCC.
    let is_mcs51 = target.contains("mcs51")
        || target.contains("8051")
        || target.contains("c51")
        || target.contains("80251")
        || target.contains("mcs251")
        || target.contains("c251");
    // Xtensa (ESP32/ESP32-S3) uses the xtensa-esp32-elf toolchain.
    let is_xtensa = target.contains("xtensa") || target.contains("esp32");
    // AVR (ATmega/ATtiny/AT90) uses avr-gcc; MSP430 uses msp430-elf-gcc;
    // V850 uses v850-elf-gcc; STM8 uses sdcc (-mstm8); RX uses rx-elf-gcc.
    let is_avr = target.contains("atmega")
        || target.contains("attiny")
        || target.contains("at90")
        || target.contains("avr");
    let is_msp430 = target.contains("msp430");
    let is_v850 = target.contains("v850");
    let is_stm8 = target.contains("stm8");
    let is_rx = target.contains("rx") || target.starts_with("r5f");
    let is_rl78 = target.contains("rl78");
    // ARM7/ARM9/Cortex-A run A32 code; the Cortex-M family runs Thumb.
    let is_arm_a32 = target.contains("arm7")
        || target.contains("arm9")
        || target.contains("arm926")
        || target.contains("arm920")
        || target.contains("lpc21")
        || target.contains("lpc22")
        || target.contains("s3c24")
        || target.contains("at91")
        || target.contains("imx")
        || target.contains("a32")
        || target.contains("arm-state");
    let mut compiler = resolve_compiler(project);
    if is_avr && compiler.is_empty() {
        compiler = ns_project::toolchain::find_command("avr-gcc")
            .map(|p| p.display().to_string())
            .unwrap_or_default();
    }
    if is_msp430 && compiler.is_empty() {
        compiler = ns_project::toolchain::find_command("msp430-elf-gcc")
            .map(|p| p.display().to_string())
            .unwrap_or_default();
    }
    if is_v850 && compiler.is_empty() {
        compiler = ns_project::toolchain::find_command("v850-elf-gcc")
            .map(|p| p.display().to_string())
            .unwrap_or_default();
    }
    if is_rx && compiler.is_empty() {
        compiler = ns_project::toolchain::find_command("rx-elf-gcc")
            .map(|p| p.display().to_string())
            .unwrap_or_default();
    }
    if is_rl78 && compiler.is_empty() {
        compiler = ns_project::toolchain::find_command("rl78-elf-gcc")
            .map(|p| p.display().to_string())
            .unwrap_or_default();
    }
    if is_stm8 && compiler.is_empty() {
        compiler = ns_project::toolchain::find_command("sdcc")
            .map(|p| p.display().to_string())
            .unwrap_or_default();
    }
    // (compiler resolved above; original duplicate removed)
    if is_mcs51 && compiler.is_empty() {
        compiler = ns_project::toolchain::find_command("sdcc")
            .map(|p| p.display().to_string())
            .unwrap_or_default();
    }
    if is_xtensa && compiler.is_empty() {
        compiler = ns_project::toolchain::find_command("xtensa-esp32-elf-gcc")
            .map(|p| p.display().to_string())
            .unwrap_or_default();
    }
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

    if is_mcs51 {
        return plan_mcs51(project, &compiler, artifact);
    }
    // C166-family targets need a vendor toolchain (Tasking/Keil) that is not
    // bundled; surface a clear error instead of silently compiling host code.
    if target.contains("c166") || target.contains("xc166") || target.contains("xc2000") || target.contains("st10") {
        bail!(
            "C166/XC166/XC2000 构建需要供应商工具链(Tasking/Keil C166),当前环境未安装;请先在 project.toml 的 compiler.path 指定"
        );
    }
    // Xtensa targets compile with the xtensa-esp32-elf toolchain; add the
    // `-mlongcalls` flag ESP-IDF and bare-metal firmware need for 24-bit
    // branch/call targets.
    if is_xtensa {
        return plan_xtensa(project, &compiler, artifact);
    }
    if is_avr {
        return plan_avr(project, &compiler, artifact);
    }
    if is_msp430 {
        return plan_msp430(project, &compiler, artifact);
    }
    if is_v850 {
        return plan_v850(project, &compiler, artifact);
    }
    if is_stm8 {
        return plan_stm8(project, &compiler, artifact);
    }
    if is_rx {
        return plan_rx(project, &compiler, artifact);
    }
    if is_rl78 {
        return plan_rl78(project, &compiler, artifact);
    }
    if is_arm_a32 {
        return plan_arm_a32(project, &compiler, artifact);
    }
    match language {
        Language::C | Language::Cpp => {
            // `[build] system` selects the C/C++ build strategy. `auto` (the
            // default) prefers an existing CMakeLists.txt, then a Makefile,
            // and finally falls back to compiling all sources directly.
            let system = project.config.build.system.trim().to_lowercase();
            match system.as_str() {
                "make" => plan_make(project, artifact),
                "cmake" => plan_cmake(project, artifact),
                "direct" => plan_native(project, &compiler, artifact),
                _ => {
                    if project.root.join("CMakeLists.txt").is_file() {
                        plan_cmake(project, artifact)
                    } else if ["Makefile", "makefile", "GNUmakefile"]
                        .iter()
                        .any(|name| project.root.join(name).is_file())
                    {
                        plan_make(project, artifact)
                    } else {
                        plan_native(project, &compiler, artifact)
                    }
                }
            }
        }
        Language::Rust => plan_rust(project, &compiler, out_dir, artifact),
        Language::Zig => plan_zig(project, &compiler, artifact),
        Language::ZR => plan_zr(project, &compiler, artifact),
        // Assembly as a project language: GCC compiles `.s`/`.S` directly.
        Language::Asm => plan_native(project, &compiler, artifact),
    }
}

/// Build plan for a C/C++ project using CMake. Configures a build tree under
/// `<out>/cmake` and builds it; `CMAKE_RUNTIME_OUTPUT_DIRECTORY` points at the
/// project's expected artifact directory so the IDE finds the binary where
/// `artifact_path()` predicts. Requires a `CMakeLists.txt` in the project root.
fn plan_cmake(project: &Project, artifact: PathBuf) -> Result<BuildPlan> {
    if !project.root.join("CMakeLists.txt").is_file() {
        bail!(
            "项目缺少 CMakeLists.txt；请在项目根目录创建它，或将 [build] system 改为 \"direct\""
        );
    }
    let out_dir = project.build_dir();
    let cmake_build = out_dir.join("cmake");
    fs::create_dir_all(&cmake_build)
        .with_context(|| format!("创建 CMake 构建目录 {} 失败", cmake_build.display()))?;
    let artifact_dir = artifact
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| out_dir.clone());
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("创建产物目录 {} 失败", artifact_dir.display()))?;

    let build_type = if project.build_config() == "release" {
        "Release"
    } else {
        "Debug"
    };
    // `cmake --build` needs the configure step to have run; a bare `cmake -B`
    // only configures. Wrap both in a shell command so the single-process
    // BuildPlan executes configure then build, and CMake's own stdout/stderr
    // (progress, warnings, errors) still streams through to the output panel.
    let configure = vec![
        "cmake".to_string(),
        "-S".to_string(),
        shell_quote(&project.root.display().to_string()),
        "-B".to_string(),
        shell_quote(&cmake_build.display().to_string()),
        format!("-DCMAKE_BUILD_TYPE={build_type}"),
        format!(
            "-DCMAKE_RUNTIME_OUTPUT_DIRECTORY={}",
            shell_quote(&artifact_dir.display().to_string())
        ),
        format!(
            "-DCMAKE_LIBRARY_OUTPUT_DIRECTORY={}",
            shell_quote(&artifact_dir.display().to_string())
        ),
        format!(
            "-DCMAKE_ARCHIVE_OUTPUT_DIRECTORY={}",
            shell_quote(&artifact_dir.display().to_string())
        ),
        "-DCMAKE_EXPORT_COMPILE_COMMANDS=ON".to_string(),
    ];
    let mut configure = configure;
    if !project.config.compiler.defines.is_empty() {
        configure.push(format!(
            "-DNS_IDE_DEFINES={}",
            shell_quote(&project.config.compiler.defines.join(";"))
        ));
    }
    if !project.config.compiler.includes.is_empty() {
        let includes = project
            .config
            .compiler
            .includes
            .iter()
            .map(|inc| {
                let p = if Path::new(inc).is_absolute() {
                    PathBuf::from(inc)
                } else {
                    project.root.join(inc)
                };
                p.display().to_string()
            })
            .collect::<Vec<_>>()
            .join(";");
        configure.push(format!("-DNS_IDE_INCLUDES={}", shell_quote(&includes)));
    }
    let shell_cmd = format!(
        "{} && cmake --build {} -j {}",
        configure.join(" "),
        shell_quote(&cmake_build.display().to_string()),
        project.build_jobs()
    );
    Ok(BuildPlan {
        program: "sh".to_string(),
        args: vec!["-c".to_string(), shell_cmd],
        cwd: project.root.clone(),
        env: vec![],
        artifact,
    })
}

/// Build plan for a C/C++ project using a Makefile. Requires a `Makefile` in
/// the project root; the Makefile is responsible for producing the artifact
/// at `artifact_path()` (the wizard emits one that does).
fn plan_make(project: &Project, artifact: PathBuf) -> Result<BuildPlan> {
    let makefile = ["Makefile", "makefile", "GNUmakefile"]
        .iter()
        .map(|name| project.root.join(name))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "项目缺少 Makefile；请在项目根目录创建它，或将 [build] system 改为 \"direct\""
            )
        })?;
    let out_dir = project.build_dir();
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("创建构建目录 {} 失败", out_dir.display()))?;
    let artifact_dir = artifact
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| out_dir.clone());
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("创建产物目录 {} 失败", artifact_dir.display()))?;
    let config = project.build_config();
    Ok(BuildPlan {
        program: "make".to_string(),
        args: vec![
            "-f".to_string(),
            makefile.display().to_string(),
            format!("-j{}", project.build_jobs()),
            format!("BUILD_DIR={}", out_dir.display()),
            format!("CONFIG={config}"),
            format!("OUTPUT={}", artifact_dir.display()),
            "all".to_string(),
        ],
        cwd: project.root.clone(),
        env: vec![],
        artifact,
    })
}

/// Resolve the build output directory the project's artifacts land in.
pub fn build_out_dir(project: &Project) -> PathBuf {
    project
        .artifact_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| project.build_dir())
}

/// Remove the build output directory. Returns the directory removed, or `None`
/// when it does not exist. Errors surface as `Err` (e.g. permission denied).
pub fn clean_build(project: &Project) -> anyhow::Result<Option<PathBuf>> {
    let out_dir = build_out_dir(project);
    if !out_dir.exists() {
        return Ok(None);
    }
    fs::remove_dir_all(&out_dir)
        .with_context(|| format!("清理构建目录 {} 失败", out_dir.display()))?;
    Ok(Some(out_dir))
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
            // Static analysis must compile for the target the build uses so the
            // diagnostics match what the real build sees. Map the MCU core to a
            // bare-metal triple (RISC-V / ARM Cortex-M); unknown targets stay
            // on the host triple (zig's default).
            if let Some(triple) = zr_target_triple(&project.config.project.target) {
                args.push("-target".to_string());
                args.push(triple.to_string());
            }
            // Treat analyzer diagnostics as errors so CI/local lint surfaces
            // them in the problems panel instead of a silent pass.
            args.push("-Werror".to_string());
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
        // Assembly: no static analyzer; report unavailable so the IDE shows a
        // clear "tool missing" message instead of a silent no-op.
        Language::Asm => None,
    }
}

/// Plan the language's unit-test runner (F4.1 单测集成). Returns `None` when
/// the language/project has no discoverable test command, so the IDE can show
/// a clear "no test runner" message instead of silently doing nothing.
///
/// - Rust: `cargo test` (isolated `--target-dir`, honors build jobs).
/// - C/C++: `ctest` for CMake projects, `make test` for Makefile projects.
/// - Zig: `zig build test` when a `build.zig` exists.
/// - ZR: no standard runner (tests are external, e.g. `zr pkg test`); `None`.
pub fn plan_tests(project: &Project) -> Option<BuildPlan> {
    let language = project.config.project.language;
    match language {
        Language::Rust => {
            let cargo = find_program("cargo")?;
            let target_dir = project.build_dir();
            let mut args = vec!["test".to_string(), "--target-dir".to_string()];
            args.push(target_dir.display().to_string());
            args.push("-j".to_string());
            args.push(project.build_jobs().to_string());
            Some(BuildPlan {
                program: cargo,
                args,
                cwd: project.root.clone(),
                env: vec![("CARGO_TARGET_DIR".into(), target_dir.display().to_string())],
                artifact: project.build_dir(),
            })
        }
        Language::C | Language::Cpp => {
            if project.root.join("CMakeLists.txt").is_file() {
                let ctest = find_program("ctest")?;
                // CMake builds land in `<out>/cmake`; run the tests there.
                let build_tree = project.build_dir().join("cmake");
                Some(BuildPlan {
                    program: ctest,
                    args: vec!["--test-dir".into(), build_tree.display().to_string()],
                    cwd: project.root.clone(),
                    env: vec![],
                    artifact: build_tree,
                })
            } else if ["Makefile", "makefile", "GNUmakefile"]
                .iter()
                .any(|name| project.root.join(name).is_file())
            {
                let make = find_program("make")?;
                Some(BuildPlan {
                    program: make,
                    args: vec!["test".into()],
                    cwd: project.root.clone(),
                    env: vec![],
                    artifact: project.build_dir(),
                })
            } else {
                // No build system: only run if the built artifact is itself a
                // runnable test binary (unusual for firmware; `None` otherwise).
                None
            }
        }
        Language::Zig => {
            if project.root.join("build.zig").is_file() {
                let zig = find_program("zig")?;
                Some(BuildPlan {
                    program: zig,
                    args: vec!["build".into(), "test".into()],
                    cwd: project.root.clone(),
                    env: vec![],
                    artifact: project.build_dir(),
                })
            } else {
                None
            }
        }
        // ZR / Asm: no in-IDE test runner (ZR tests run via `zr pkg`; assembly
        // has no unit-test concept).
        Language::ZR | Language::Asm => None,
    }
}

/// Per-function stack usage, parsed from GCC's `-fstack-usage` `.su` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackUsageEntry {
    /// Source file path as written in the `.su` file.
    pub file: String,
    /// Function name.
    pub function: String,
    /// Static stack usage in bytes (0 for leaf functions using no stack).
    pub bytes: u64,
    /// Whether the function is dynamic (uses alloca/VLA, so the static figure
    /// is a lower bound).
    pub dynamic: bool,
}

/// Parse a GCC `-fstack-usage` `.su` file (one line per function):
/// `\t<file>\t<function>\t<bytes>\t<static|dynamic>`.
pub fn parse_stack_usage_su(text: &str) -> Vec<StackUsageEntry> {
    let mut entries = Vec::new();
    for raw in text.lines() {
        // `.su` lines begin with a tab (the leading tab separates the file
        // column). Reject blank lines and lines that are not tab-prefixed.
        if !raw.starts_with('\t') {
            continue;
        }
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // Split on tab: [file, function, bytes, kind].
        let parts: Vec<&str> = line.split('\t').map(str::trim).collect();
        if parts.len() < 4 {
            continue;
        }
        let bytes: u64 = parts[2].parse().unwrap_or(0);
        let dynamic = parts[3].eq_ignore_ascii_case("dynamic");
        entries.push(StackUsageEntry {
            file: parts[0].to_string(),
            function: parts[1].to_string(),
            bytes,
            dynamic,
        });
    }
    entries
}

/// Plan a stack-usage analysis for a C/C++ project: compile each source with
/// `-fstack-usage` into `<out>/stack-usage/`, so GCC emits a `.su` file next
/// to each object. Returns `None` for non-C/C++ projects or when the compiler
/// is unavailable.
pub fn plan_stack_usage(project: &Project) -> Option<BuildPlan> {
    let language = project.config.project.language;
    if !matches!(language, Language::C | Language::Cpp) {
        return None;
    }
    let compiler = resolve_compiler(project);
    if compiler.is_empty() {
        return None;
    }
    let sources = project.source_files().ok()?;
    if sources.is_empty() {
        return None;
    }
    let out_dir = project.build_dir().join("stack-usage");
    let _ = fs::create_dir_all(&out_dir);
    let mut args = Vec::new();
    let std = if project.config.compiler.std.trim().is_empty() {
        language.default_std()
    } else {
        project.config.compiler.std.trim()
    };
    if !std.is_empty() {
        args.push(format!("-std={std}"));
    }
    // Compile only (`-c`), with stack-usage emission. `-fstack-usage` writes
    // `<object>.su` next to the object file; we name objects in `out_dir`.
    args.push("-fstack-usage".to_string());
    args.push("-c".to_string());
    let opt = project.config.compiler.opt.trim();
    if !opt.is_empty() {
        args.extend(opt.split_whitespace().map(str::to_string));
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
    for (index, source) in sources.iter().enumerate() {
        let object = out_dir.join(format!("{}.o", index));
        args.push(source.display().to_string());
        args.push("-o".to_string());
        args.push(object.display().to_string());
    }
    Some(BuildPlan {
        program: compiler,
        args,
        cwd: project.root.clone(),
        env: vec![],
        artifact: out_dir,
    })
}

/// Collect and parse all `.su` files emitted by a `plan_stack_usage` run.
/// Returns an empty vec when no `.su` files were produced.
pub fn collect_stack_usage(out_dir: &Path) -> Vec<StackUsageEntry> {
    let mut entries = Vec::new();
    let Ok(read_dir) = fs::read_dir(out_dir) else {
        return entries;
    };
    let mut files: Vec<PathBuf> = read_dir
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("su"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    for file in files {
        if let Ok(text) = fs::read_to_string(&file) {
            entries.extend(parse_stack_usage_su(&text));
        }
    }
    // Stable order: by file, then by descending stack usage (largest first).
    entries.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(b.bytes.cmp(&a.bytes))
            .then(a.function.cmp(&b.function))
    });
    entries
}

 /// Aggregate line-coverage summary for one source file, from `gcov`.
 #[derive(Debug, Clone, PartialEq, Eq)]
 pub struct CoverageFile {
     /// Source file path as reported by gcov.
     pub file: String,
     /// Lines executed at least once.
     pub executed: u64,
     /// Lines never executed.
     pub not_executed: u64,
    /// Per-line coverage from `gcov -t` (0-based line → count of executions).
    /// Only executable lines appear; `None` means line-level data is absent.
    pub line_hits: Option<Vec<u64>>,
 }
 
 impl CoverageFile {
     /// Percentage of executable lines covered (0-100), rounded down.
     pub fn percent(&self) -> u64 {
         let total = self.executed + self.not_executed;
         if total == 0 {
             return 0;
         }
         self.executed * 100 / total
     }
 }

/// Parse `gcov -t` per-line output into a flat line-hit map.
///
/// gcov's `-t` (stdout) format writes, for each source file, a
/// `:line:source` header followed by `count:line_no:source_code` rows. The
/// count field is `#####` (never executed), `-` (non-executable), or a
/// decimal execution count. This returns `(file, Vec<u64>)` pairs where the
/// vector is indexed by 0-based line and holds the hit count (0 = never).
///
/// Rows that are not executable (`-`) are omitted so the vector stays dense
/// for executable lines; callers map line numbers directly.
pub fn parse_gcov_lines(text: &str) -> Vec<(String, Vec<u64>)> {
    let mut files: Vec<(String, Vec<u64>)> = Vec::new();
    let mut current: Option<usize> = None;
    for line in text.lines() {
        // `:0:File 'src/main.c'` (with `-t` gcov prefixes the file header
        // with `:<line>:File '...'`).
        if let Some(idx) = line.find("File '") {
            let rest = &line[idx + "File '".len()..];
            let file = rest.trim_end_matches('\'').to_string();
            files.push((file, Vec::new()));
            current = Some(files.len() - 1);
            continue;
        }
        let Some(fi) = current else { continue };
        // `count:line_no:source` — parse the two leading numeric fields.
        let mut parts = line.splitn(3, ':');
        let count_field = parts.next().unwrap_or("").trim();
        let line_field = parts.next().unwrap_or("").trim();
        if count_field.is_empty() || line_field.is_empty() {
            continue;
        }
        if count_field == "-" {
            // Non-executable line; skip (no hit entry).
            continue;
        }
        let Ok(line_no) = line_field.parse::<usize>() else {
            continue;
        };
        if line_no == 0 {
            continue;
        }
        let hits = if count_field == "#####" {
            0u64
        } else {
            match count_field.parse::<u64>() {
                Ok(hits) => hits,
                Err(_) => continue,
            }
        };
        let line = line_no - 1; // 0-based.
        let vec = &mut files[fi].1;
        if vec.len() <= line {
            vec.resize(line + 1, 0);
        }
        vec[line] = hits;
    }
    files
}


/// Parse `gcov` aggregate output (produced without `-t`) into per-file line
/// coverage. Each file appears as a `File '<path>'` header followed by a
/// `Lines executed:<pct>% of <n>` summary line.
pub fn parse_gcov_report(text: &str) -> Vec<CoverageFile> {
    let mut files = Vec::new();
    let mut current: Option<CoverageFile> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        // `File 'src/main.c'` opens a per-file section.
        if let Some(rest) = trimmed.strip_prefix("File '") {
            let file = rest.trim_end_matches('\'').to_string();
             current = Some(CoverageFile {
                 file,
                 executed: 0,
                 not_executed: 0,
                line_hits: None,
             });
            continue;
        }
        // A `Lines executed: N% of M` line closes the current file's summary.
        if let (Some(entry), Some(m)) = (current.as_mut(), parse_lines_executed(trimmed)) {
            let total = m;
            let percent = parse_percent(trimmed).unwrap_or(0);
            let executed = total.saturating_mul(percent) / 100;
            entry.executed = executed;
            entry.not_executed = total.saturating_sub(executed);
            files.push(current.take().expect("entry checked above"));
        }
    }
    files
}

fn parse_lines_executed(line: &str) -> Option<u64> {
    // `Lines executed:42.86% of 14`
    let after = line.strip_prefix("Lines executed:")?;
    let of = after.split("of").nth(1)?.trim();
    of.parse().ok()
}

fn parse_percent(line: &str) -> Option<u64> {
    let after = line.strip_prefix("Lines executed:")?;
    let pct = after.split('%').next()?.trim();
    // `pct` may be a float like `42.86`; take the integer part.
    let int_part = pct.split('.').next()?.trim();
    int_part.parse().ok()
}

/// Plan a gcov line-coverage analysis for a C/C++ project. Compiles every
/// source with `--coverage`, links them into a single host-runnable test
/// binary (the entry source is required to have a `main`), so `gcov` can
/// measure line coverage after the binary runs. Returns `None` when the
/// compiler is unavailable or the project is not C/C++.
pub fn plan_coverage(project: &Project) -> Option<BuildPlan> {
    let language = project.config.project.language;
    if !matches!(language, Language::C | Language::Cpp) {
        return None;
    }
    let compiler = resolve_compiler(project);
    if compiler.is_empty() {
        return None;
    }
    let sources = project.source_files().ok()?;
    if sources.is_empty() {
        return None;
    }
    let out_dir = project.build_dir().join("coverage");
    let _ = fs::create_dir_all(&out_dir);
    let binary = out_dir.join("coverage-test");
    let mut args = Vec::new();
    let std = if project.config.compiler.std.trim().is_empty() {
        language.default_std()
    } else {
        project.config.compiler.std.trim()
    };
    if !std.is_empty() {
        args.push(format!("-std={std}"));
    }
    // `--coverage` enables gcov instrumentation (`.gcno` at compile, `.gcda`
    // at run) and links the gcov runtime.
    args.push("--coverage".to_string());
    let opt = project.config.compiler.opt.trim();
    if !opt.is_empty() {
        args.extend(opt.split_whitespace().map(str::to_string));
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
    for link_flag in &project.config.compiler.link_flags {
        let link_flag = link_flag.trim();
        if !link_flag.is_empty() {
            args.extend(link_flag.split_whitespace().map(str::to_string));
        }
    }

    args.push("-o".to_string());
    args.push(binary.display().to_string());
    Some(BuildPlan {
        program: compiler,
        args,
        cwd: project.root.clone(),
        env: vec![],
        artifact: binary,
    })
}

/// Run a coverage-instrumented binary and then `gcov` over its `.gcda`/`.gcno`
/// data, returning the parsed per-file line coverage. The binary is executed
/// from the project root so `.gcda` files land next to the `.gcno` (in the
/// build dir). `Ok(None)` when `gcov` is unavailable or produced no data.
pub fn run_coverage(
    binary: &Path,
    project_root: &Path,
    out_dir: &Path,
) -> Result<Option<Vec<CoverageFile>>> {
    // Run the instrumented test binary; `.gcda` files are written next to the
    // object files on exit (flush at process end).
    let run = Command::new(binary)
        .current_dir(project_root)
        .output()
        .with_context(|| format!("运行覆盖率测试二进制 {} 失败", binary.display()))?;
    if !run.status.success() {
        // A non-zero exit still produces coverage data (tests may fail); do not
        // bail here — gcov can still summarize the lines that ran.
    }
    // Locate gcov; without it we cannot summarize.
    let gcov = match find_program("gcov") {
        Some(gcov) => gcov,
        None => return Ok(None),
    };
    // gcov needs the object/data *stem* (the `.gcno` file name without its
    // extension), e.g. `coverage-test-main`. Collect those stems.
    let stems: Vec<String> = fs::read_dir(out_dir)
        .map(|entries| {
            let mut stems: Vec<String> = entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| {
                    path.extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext.eq_ignore_ascii_case("gcno"))
                        .unwrap_or(false)
                })
                .filter_map(|path| {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .map(str::to_string)
                })
                .collect();
            stems.sort();
            stems.dedup();
            stems
        })
        .unwrap_or_default();
    if stems.is_empty() {
        return Ok(None);
    }
     let mut output = Vec::new();
     for stem in &stems {
         let report = Command::new(&gcov)
             .arg("-o")
             .arg(out_dir)
            .arg("-t")
             .arg(stem)
             .current_dir(project_root)
             .output();
         if let Ok(report) = report {
             output.extend(report.stdout);
             output.push(b'\n');
         }
     }
     let text = String::from_utf8_lossy(&output);
    let mut files = parse_gcov_report(&text);
    // Attach per-line hit data from the same `-t` output.
    let lines = parse_gcov_lines(&text);
    for file in &mut files {
        let path = file.file.clone();
        if let Some((_, hits)) = lines.iter().find(|(f, _)| *f == path) {
            file.line_hits = Some(hits.clone());
        }
    }
    Ok((!files.is_empty()).then_some(files))
 }

fn find_program(name: &str) -> Option<String> {
    let path = ns_project::toolchain::find_command(name)?;
    Some(path.display().to_string())
}
/// Plan a C/C++ preprocessor run: compile each C/C++ source with `-E` (only
/// preprocess) and stream the concatenated output, so the IDE can show the
/// macro-expanded translation unit. Returns `None` when the project is not
/// C/C++ or no compiler is configured, so the IDE can report it cleanly.
pub fn plan_preprocess(project: &Project) -> Option<BuildPlan> {
    let language = project.config.project.language;
    if !matches!(language, Language::C | Language::Cpp) {
        return None;
    }
    let compiler = resolve_compiler(project);
    if compiler.is_empty() {
        return None;
    }
    let sources = project.source_files().ok()?;
    let c_sources: Vec<&PathBuf> = sources
        .iter()
        .filter(|source| {
            let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("");
            matches!(ext, "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "C")
        })
        .collect();
    if c_sources.is_empty() {
        return None;
    }
    // A single `-E` invocation over all sources (not one per source) keeps the
    // output a single stream and matches how the compiler would expand macros
    // across the project. `-P` strips the `# <line> <file>` markers so the
    // preview is readable source, not a marker-heavy dump.
    let mut args = Vec::new();
    let std = if project.config.compiler.std.trim().is_empty() {
        language.default_std()
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
    args.push("-E".to_string());
    args.push("-P".to_string());
    for source in &c_sources {
        args.push(source.display().to_string());
    }
    Some(BuildPlan {
        program: compiler,
        args,
        cwd: project.root.clone(),
        env: Vec::new(),
        artifact: PathBuf::new(),
    })
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
    if project.build_config() == "debug" {
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
    for link_flag in &project.config.compiler.link_flags {
        let link_flag = link_flag.trim();
        if !link_flag.is_empty() {
            // Split on whitespace so `-Wl,-Map=out.map` stays one flag while
            // `-lm -lfoo` becomes two; quoted arguments are kept verbatim.
            args.extend(link_flag.split_whitespace().map(str::to_string));
        }
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

/// Build plan for Xtensa (ESP32/ESP32-S3) targets. Uses the
/// `xtensa-esp32-elf-gcc` cross-compiler with `-mlongcalls` (24-bit
/// call/branch targets) and the ESP32 CPU model flags.
fn plan_xtensa(project: &Project, compiler: &str, artifact: PathBuf) -> Result<BuildPlan> {
    let sources = project.source_files()?;
    if sources.is_empty() {
        bail!("src/ 下没有找到源文件");
    }
    let mut args = Vec::new();
    // Xtensa LX6 target: ESP32 (or ESP32-S3 via -mcpu). `-mlongcalls` lets
    // calls reach the full 24-bit program space; `-Os` is the ESP-IDF default.
    args.push("-mlongcalls".to_string());
    if project.build_config() == "debug" {
        args.push("-g".to_string());
    }
    let std = if project.config.compiler.std.trim().is_empty() {
        "c11".to_string()
    } else {
        project.config.compiler.std.trim().to_string()
    };
    args.push(format!("-std={std}"));
    let opt = project.config.compiler.opt.trim();
    if !opt.is_empty() {
        args.extend(opt.split_whitespace().map(str::to_string));
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
    for link_flag in &project.config.compiler.link_flags {
        let link_flag = link_flag.trim();
        if !link_flag.is_empty() {
            args.extend(link_flag.split_whitespace().map(str::to_string));
        }
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

/// Build an Intel 8051 (MCS-51) program with SDCC. The artifact is Intel HEX
/// (`.ihx`), which the IDE's 8051 simulator decodes. SDCC must be on PATH or
/// configured in `project.toml`; a missing compiler surfaces as an error so
/// the IDE can report it honestly.
fn plan_mcs51(project: &Project, compiler: &str, artifact: PathBuf) -> Result<BuildPlan> {
    let sources = project.source_files()?;
    if sources.is_empty() {
        bail!("src/ 下没有找到源文件");
    }
    let mut args = Vec::new();
    // SDCC model: small memory model, MCS-51 core.
    args.push("-mmcs51".to_string());
    args.push("--model-small".to_string());
    if project.build_config() == "debug" {
        args.push("--debug".to_string());
    }
    let opt = project.config.compiler.opt.trim();
    if !opt.is_empty() {
        args.extend(opt.split_whitespace().map(str::to_string));
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
    for link_flag in &project.config.compiler.link_flags {
        let link_flag = link_flag.trim();
        if !link_flag.is_empty() {
            args.extend(link_flag.split_whitespace().map(str::to_string));
        }
    }

    // SDCC derives the output name from `-o`; Intel HEX is the default format
    // for MCS-51 targets.
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

/// Build plan for a cross-compiled bare-metal target (AVR/MSP430/V850/RX).
/// The cross-compiler is invoked with `-mmcu`/`-mcpu`/`-march` flags as
/// appropriate; the resulting ELF is loaded by the simulator.
fn plan_cross(
    project: &Project,
    compiler: &str,
    artifact: PathBuf,
    arch_flags: &[&str],
) -> Result<BuildPlan> {
    let sources = project.source_files()?;
    if sources.is_empty() {
        bail!("src/ 下没有找到源文件");
    }
    let mut args = Vec::new();
    args.extend(arch_flags.iter().map(|f| f.to_string()));
    let std = if project.config.compiler.std.trim().is_empty() {
        "c11".to_string()
    } else {
        project.config.compiler.std.trim().to_string()
    };
    args.push(format!("-std={std}"));
    if project.build_config() == "debug" {
        args.push("-g".to_string());
    }
    let opt = project.config.compiler.opt.trim();
    if !opt.is_empty() {
        args.extend(opt.split_whitespace().map(str::to_string));
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
    for link_flag in &project.config.compiler.link_flags {
        let link_flag = link_flag.trim();
        if !link_flag.is_empty() {
            args.extend(link_flag.split_whitespace().map(str::to_string));
        }
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

/// Build plan for an AVR target with `avr-gcc -mmcu=<target>`.
fn plan_avr(project: &Project, compiler: &str, artifact: PathBuf) -> Result<BuildPlan> {
    let target = project.config.project.target.trim().to_lowercase();
    let mmcu = if target.contains("attiny85") {
        "attiny85"
    } else if target.contains("attiny") {
        "attiny2313"
    } else {
        "atmega328p"
    };
    plan_cross(project, compiler, artifact, &[&format!("-mmcu={mmcu}")])
}

/// Build plan for an ARM7/ARM9/Cortex-A (A32) target with the ARM GNU
/// toolchain. `-marm` forces ARM state (A32) rather than Thumb, and `-mcpu`
/// picks the exact core so the toolchain emits the right instruction set.
fn plan_arm_a32(project: &Project, compiler: &str, artifact: PathBuf) -> Result<BuildPlan> {
    let target = project.config.project.target.trim().to_lowercase();
    let mcpu = if target.contains("arm920") || target.contains("s3c24") {
        "arm920t"
    } else if target.contains("arm926") {
        "arm926ej-s"
    } else if target.contains("cortex-a") {
        "cortex-a7"
    } else {
        // ARM7TDMI (LPC21xx/22xx, AT91SAM7, generic ARM7/ARM9).
        "arm7tdmi"
    };
    plan_cross(project, compiler, artifact, &["-marm", &format!("-mcpu={mcpu}")])
}

/// Build plan for an MSP430 target with `msp430-elf-gcc`.
fn plan_msp430(project: &Project, compiler: &str, artifact: PathBuf) -> Result<BuildPlan> {
    let target = project.config.project.target.trim().to_lowercase();
    let mcpu = if target.contains("g2553") {
        "msp430g2553"
    } else if target.contains("g2") {
        "msp430g2553"
    } else {
        "msp430f5529"
    };
    plan_cross(project, compiler, artifact, &[&format!("-mmcu={mcpu}")])
}

/// Build plan for a V850 target with `v850-elf-gcc`.
fn plan_v850(project: &Project, compiler: &str, artifact: PathBuf) -> Result<BuildPlan> {
    plan_cross(project, compiler, artifact, &["-mv850e2"])
}

/// Build plan for an RX target with `rx-elf-gcc`.
fn plan_rx(project: &Project, compiler: &str, artifact: PathBuf) -> Result<BuildPlan> {
    let target = project.config.project.target.trim().to_lowercase();
    let mcpu = if target.contains("rx64") || target.contains("rx62") {
        "rx62n"
    } else {
        "rx600"
    };
    plan_cross(project, compiler, artifact, &[&format!("-mcpu={mcpu}")])
}

/// Build plan for an STM8 target with SDCC (`sdcc -mstm8`). The artifact is
/// Intel HEX, which the STM8 simulator decodes.
fn plan_stm8(project: &Project, compiler: &str, artifact: PathBuf) -> Result<BuildPlan> {
    let sources = project.source_files()?;
    if sources.is_empty() {
        bail!("src/ 下没有找到源文件");
    }
    let mut args = Vec::new();
    args.push("-mstm8".to_string());
    args.push("--out-fmt-ihx".to_string());
    if project.build_config() == "debug" {
        args.push("--debug".to_string());
    }
    let opt = project.config.compiler.opt.trim();
    if !opt.is_empty() {
        args.extend(opt.split_whitespace().map(str::to_string));
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

/// Build plan for an RL78 target with `rl78-elf-gcc` (or `rl78-elf-as` for
/// assembly). RL78 code is 16-bit CISC; the compiler emits a `.elf` that the
/// RL78 simulator loads.
fn plan_rl78(project: &Project, compiler: &str, artifact: PathBuf) -> Result<BuildPlan> {
    plan_cross(project, compiler, artifact, &["-mmcu=rl78"])
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
        if project.build_config() == "release" {
            args.push("--release".to_string());
        }
        // Parallel compilation: `cargo build -j <jobs>`.
        args.push("-j".to_string());
        args.push(project.build_jobs().to_string());
    } else {
        let entry = project.entry_file();
        args.push(entry.display().to_string());
        args.push("-o".to_string());
        args.push(artifact.display().to_string());
        if project.build_config() == "debug" {
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
        if project.build_config() == "release" {
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
    // The zr compiler is a single-file tool: `zr <input> --target <triple>
    // -o <out>`. The target triple comes from the MCU's core (RISC-V → riscv32,
    // ARM Cortex-M → arm-none-eabi), defaulting to the host when unknown.
    let triple = zr_target_triple(&project.config.project.target);
    let mut args = vec![entry.display().to_string()];
    if let Some(triple) = triple {
        args.push("--target".to_string());
        args.push(triple.to_string());
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

/// Map a target MCU id to the zr bare-metal target triple it should compile
/// for. `None` means "use the host triple".
fn zr_target_triple(target: &str) -> Option<&'static str> {
    let core = ns_device::catalog_entry(target).map(|entry| entry.core);
    match core {
        Some("RISC-V") => Some("riscv32-unknown-none"),
        Some("ARM Cortex-M0+" | "ARM Cortex-M3" | "ARM Cortex-M4") => Some("arm-none-eabi"),
        // ARM7/ARM9/Cortex-A (A32 state) also use the bare-metal ARM triple.
        Some("ARM7TDMI-S" | "ARM920T" | "ARM926EJ-S" | "Cortex-A") => Some("arm-none-eabi"),
        _ => None,
    }
}

/// Build the clang-compatible compile flags for C/C++ sources from
/// `project.toml`, matching what `plan_build` uses so clangd's diagnostics and
/// jump-to-definition see the same macros, includes and target.
fn compile_flags(project: &Project) -> Vec<String> {
    let mut args = Vec::new();
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
    if project.build_config() == "debug" {
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
    args
}

/// Generate a clangd `compile_commands.json` in the project root so the C/C++
/// language server sees the exact `-D`/`-I`/`-std` flags from `project.toml`.
/// Returns the number of translation units written, or an error.
pub fn generate_compile_commands(project: &Project) -> Result<usize> {
    let language = project.config.project.language;
    if !matches!(language, Language::C | Language::Cpp) {
        return Ok(0);
    }
    let sources = project.source_files()?;
    if sources.is_empty() {
        return Ok(0);
    }
    let compiler = resolve_compiler(project);
    if compiler.is_empty() {
        bail!("未找到可用的 C/C++ 编译器，无法生成 compile_commands.json");
    }
    let flags = compile_flags(project);
    let cwd = project.root.to_string_lossy().to_string();
    let mut entries = Vec::with_capacity(sources.len());
    for source in &sources {
        let file = source.to_string_lossy().to_string();
        let mut command = vec![compiler.clone()];
        command.extend(flags.iter().cloned());
        command.push(file.clone());
        // clangd needs `-c` for object-style compile units.
        command.push("-c".to_string());
        entries.push(format!(
            "{{ \"directory\": {:?}, \"file\": {:?}, \"command\": {:?} }}",
            cwd, file, command.join(" ")
        ));
    }
    let json = format!("[\n{}\n]\n", entries.join(",\n"));
    fs::write(project.root.join("compile_commands.json"), json)?;
    Ok(sources.len())
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
            file_timings: Vec::new(),
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
            file_timings: Vec::new(),
         },
    }
}

/// A single line of build output streamed while the build runs.
#[derive(Debug, Clone)]
pub struct StreamLine {
    pub is_stderr: bool,
    pub text: String,
}

/// Run a build plan, streaming stdout/stderr lines to `on_line` as they are
/// produced (not only after the process exits). Returns the same [`BuildOutcome`]
/// as [`execute`]. `on_line` runs on the calling thread between pipe reads.
pub fn execute_stream(plan: &BuildPlan, on_line: impl FnMut(StreamLine)) -> BuildOutcome {
    execute_stream_inner(plan, None, on_line)
}

/// Like [`execute_stream`], but stops the child process (and returns early with
/// `success == false`) as soon as `cancel` is set. The build report still
/// contains whatever output was produced before cancellation.
pub fn execute_stream_cancelable(
    plan: &BuildPlan,
    cancel: Arc<AtomicBool>,
    on_line: impl FnMut(StreamLine),
) -> BuildOutcome {
    execute_stream_inner(plan, Some(cancel), on_line)
}

fn execute_stream_inner(
    plan: &BuildPlan,
    cancel: Option<Arc<AtomicBool>>,
    mut on_line: impl FnMut(StreamLine),
) -> BuildOutcome {
    let started = Instant::now();
    if let Some(parent) = plan.artifact.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let mut child = match Command::new(&plan.program)
        .args(&plan.args)
        .current_dir(&plan.cwd)
        .envs(plan.env.iter().map(|(key, value)| (key, value)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return BuildOutcome {
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
                file_timings: Vec::new(),
             };
        }
    };

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    // Read stdout/stderr concurrently so a chatty child cannot block on a full
    // pipe while the other reader waits.
    let (tx, rx) = std::sync::mpsc::channel::<StreamLine>();
    let writer = tx.clone();
    let stdout_thread = std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if writer
                .send(StreamLine { is_stderr: false, text: line })
                .is_err()
            {
                break;
            }
        }
    });
    let writer = tx.clone();
    let stderr_thread = std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if writer
                .send(StreamLine { is_stderr: true, text: line })
                .is_err()
            {
                break;
            }
        }
    });
    drop(tx);

     let mut stdout_buf = String::new();
     let mut stderr_buf = String::new();
     let mut cancelled = false;
    // Real per-file timing: when a per-file marker line arrives, close the
    // previous file's window (gap to this marker) and start a new one. The
    // last window closes at the end of the build.
    let mut marker: Option<(String, std::time::Instant)> = None;
    let mut file_timings: Vec<FileTiming> = Vec::new();
     loop {
         match rx.recv_timeout(Duration::from_millis(100)) {
             Ok(line) => {
                 if line.is_stderr {
                     stderr_buf.push_str(&line.text);
                     stderr_buf.push('\n');
                 } else {
                     stdout_buf.push_str(&line.text);
                     stdout_buf.push('\n');
                 }
                if let Some(file) = file_from_build_line(&line.text) {
                    let now = std::time::Instant::now();
                    if let Some((prev_file, prev_at)) = marker.take() {
                        file_timings.push(FileTiming {
                            file: prev_file,
                            elapsed_ms: now.duration_since(prev_at).as_millis() as u64,
                        });
                    }
                    marker = Some((file, now));
                }
                 on_line(line);
             }
             Err(RecvTimeoutError::Timeout) => {
                 if cancel.as_ref().is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                     cancelled = true;
                     let _ = child.kill();
                     break;
                 }
             }
             Err(RecvTimeoutError::Disconnected) => break,
         }
     }
    // Close the final window against the total build duration.
    if let Some((prev_file, prev_at)) = marker.take() {
        file_timings.push(FileTiming {
             file: prev_file,
            elapsed_ms: std::time::Instant::now().duration_since(prev_at).as_millis() as u64,
         });
    }
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();

    let status = child.wait();
    let success = if cancelled {
        false
    } else {
        status.as_ref().map(|s| s.success()).unwrap_or(false)
    };
    let code = status.as_ref().ok().and_then(|s| s.code());
    let mut stderr = stderr_buf;
    if cancelled {
        stderr.push_str("\n构建已取消\n");
    }
    BuildOutcome {
        success,
        code,
        stdout: stdout_buf,
        stderr,
         duration: started.elapsed(),
         plan: plan.clone(),
        file_timings,
     }
 }
 /// Extract a source file name from a build progress/compile line. Handles
/// `[ 25%] Building C object .../foo.c.o`, `cc ... -c foo.c`, and
/// `clang ... -c /path/foo.c`.
fn file_from_build_line(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    // CMake progress: `[ 25%] Building C object CMakeFiles/.../foo.c.o`
    if line.contains("Building ") {
        if let Some(idx) = line.find("object ") {
            let rest = &line[idx + "object ".len()..];
            let file = rest.split_whitespace().next().unwrap_or("");
            let file = file.trim_end_matches(".o");
            if !file.is_empty() {
                return Some(file.to_string());
            }
        }
    }
    // Compiler `-c <file>`: look for the last `-c` and take the following token.
    if let Some(idx) = line.rfind(" -c ") {
        let rest = &line[idx + 4..];
        let file = rest.split_whitespace().next().unwrap_or("");
        if file.ends_with(".c") || file.ends_with(".cpp") || file.ends_with(".cc") {
            return Some(file.to_string());
        }
    }
    None
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

    /// Serializes tests that mutate the process-global `PATH` (the clang-tidy
    /// and zig static-analysis tests both inject a stub bin dir; running them
    /// in parallel corrupts each other's PATH).
    static PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    /// `file_from_build_line` extracts the source file from CMake progress
    /// lines and compiler `-c` invocations, and ignores unrelated lines.
    #[test]
    fn file_from_build_line_extracts_source() {
        assert_eq!(
            file_from_build_line("[ 25%] Building C object CMakeFiles/app.dir/src/main.c.o"),
            Some("CMakeFiles/app.dir/src/main.c".to_string())
        );
        assert_eq!(
            file_from_build_line("arm-none-eabi-gcc -mcpu=cortex-m3 -c src/blink.c"),
            Some("src/blink.c".to_string())
        );
        assert_eq!(file_from_build_line("Linking C executable app.elf"), None);
        assert_eq!(file_from_build_line("make: *** [all] Error 1"), None);
         assert_eq!(file_from_build_line(""), None);
     }

    /// `suggest_quick_fix` maps common compiler errors to actionable fixes:
    /// missing include -> AddInclude, undefined reference -> hint, undeclared
    /// identifier -> hint, and unknown messages -> None.
    #[test]
    fn suggests_quick_fix_for_common_errors() {
        let missing_include = Diagnostic {
            path: None,
            line: None,
            column: None,
            severity: Severity::Error,
            message: "fatal error: stdio.h: No such file or directory".to_string(),
            raw: String::new(),
        };
        assert_eq!(
            suggest_quick_fix(&missing_include, Some("int main(){}")),
            Some(QuickFix::AddInclude {
                header: "stdio.h".to_string()
            })
        );
        // Already included -> hint, not a duplicate insert.
        assert!(matches!(
            suggest_quick_fix(&missing_include, Some("#include <stdio.h>\n")),
            Some(QuickFix::Hint(_))
        ));

        // Known libm symbol -> offer `-lm` (AddLinkFlag).
        let undefined_sqrt = Diagnostic {
            path: None,
            line: None,
            column: None,
            severity: Severity::Error,
            message: "undefined reference to `sqrt`".to_string(),
            raw: String::new(),
        };
        assert_eq!(
            suggest_quick_fix(&undefined_sqrt, None),
            Some(QuickFix::AddLinkFlag { flag: "-lm".to_string() })
        );
        // Unknown symbol -> informative hint, not a blind `-l`.
        let undefined_unknown = Diagnostic {
            path: None,
            line: None,
            column: None,
            severity: Severity::Error,
            message: "undefined reference to `my_custom_fn`".to_string(),
            raw: String::new(),
        };
        assert!(matches!(
            suggest_quick_fix(&undefined_unknown, None),
            Some(QuickFix::Hint(msg)) if msg.contains("my_custom_fn")
        ));
        // `pthread_create` -> `-lpthread`.
        let undefined_pthread = Diagnostic {
            path: None,
            line: None,
            column: None,
            severity: Severity::Error,
            message: "undefined reference to `pthread_create`".to_string(),
            raw: String::new(),
        };
        assert_eq!(
            suggest_quick_fix(&undefined_pthread, None),
            Some(QuickFix::AddLinkFlag { flag: "-lpthread".to_string() })
        );


        // `foo` is not a standard-library symbol -> hint.
        let undeclared_custom = Diagnostic {
            path: None,
            line: None,
            column: None,
            severity: Severity::Error,
            message: "‘foo’ undeclared (first use in this function)".to_string(),
            raw: String::new(),
        };
        assert!(matches!(
            suggest_quick_fix(&undeclared_custom, None),
            Some(QuickFix::Hint(msg)) if msg.contains("foo")
        ));
        // `printf` is declared in <stdio.h> -> offer the include.
        let undeclared_printf = Diagnostic {
            path: None,
            line: None,
            column: None,
            severity: Severity::Error,
            message: "‘printf’ undeclared (first use in this function)".to_string(),
            raw: String::new(),
        };
        assert_eq!(
            suggest_quick_fix(&undeclared_printf, None),
            Some(QuickFix::AddInclude { header: "stdio.h".to_string() })
        );
        // Already included -> hint, not a duplicate insert.
        assert!(matches!(
            suggest_quick_fix(&undeclared_printf, Some("#include <stdio.h>\n")),
            Some(QuickFix::Hint(msg)) if msg.contains("printf")
        ));


        let unrelated = Diagnostic {
            path: None,
            line: None,
            column: None,
            severity: Severity::Warning,
            message: "make: *** [all] Error 1".to_string(),
            raw: String::new(),
        };
        assert_eq!(suggest_quick_fix(&unrelated, None), None);
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
    fn preprocess_plan_uses_compiler_flags_and_e() {
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-preprocess-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\nlanguage = \"C\"\ntarget = \"x86_64\"\ncompiler = \"cc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"c11\"\nopt = \"-O0\"\ndefines = [\"LED=1\"]\nincludes = [\"include\"]\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.c"), "#include <stdint.h>\n").unwrap();
        fs::create_dir_all(root.join("include")).unwrap();
        fs::write(root.join("include/defs.h"), "#pragma once\n").unwrap();

        let project = Project::open(&root).unwrap();
        let plan = plan_preprocess(&project).expect("C project has a preprocess plan");
        assert!(plan.args.contains(&"-E".to_string()), "must run preprocessor only: {:?}", plan.args);
        assert!(plan.args.contains(&"-P".to_string()), "must strip line markers: {:?}", plan.args);
        assert!(plan.args.contains(&"-DLED=1".to_string()), "defines forwarded: {:?}", plan.args);
        assert!(
            plan.args.iter().any(|a| a.starts_with("-I") && a.contains("include")),
            "include dirs forwarded: {:?}",
            plan.args
        );
        assert!(
            plan.args.iter().any(|a| a.ends_with("src/main.c")),
            "source file included: {:?}",
            plan.args
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn preprocess_plan_none_for_non_c() {
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-preprocess-rust-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\nlanguage = \"Rust\"\ntarget = \"x86_64\"\ncompiler = \"rustc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n",
        )
        .unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let project = Project::open(&root).unwrap();
        assert!(plan_preprocess(&project).is_none(), "Rust has no C preprocess");
        let _ = fs::remove_dir_all(root);
    }

     /// `generate_compile_commands` writes a clangd `compile_commands.json` with
    /// one entry per C source and the `-D`/`-I`/`-std` flags from `project.toml`.
    #[test]
    fn generate_compile_commands_emits_cflags_and_units() {
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-compile-commands-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\nlanguage = \"C\"\ntarget = \"stm32f103c8\"\ncompiler = \"arm-none-eabi-gcc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"c11\"\nopt = \"-O2\"\ndefines = [\"STM32F103xB\"]\nincludes = [\"src\", \"include\"]\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

        let project = Project::open(&root).unwrap();
        let count = generate_compile_commands(&project).unwrap();
        assert_eq!(count, 1, "one translation unit");
        let json = fs::read_to_string(root.join("compile_commands.json")).unwrap();
        assert!(json.contains("-std=c11"), "std flag: {json}");
        assert!(json.contains("-DSTM32F103xB"), "define flag: {json}");
        assert!(json.contains("-I"), "include flag: {json}");
        assert!(json.contains("src/main.c"), "source file: {json}");
        assert!(json.contains("-c"), "object mode: {json}");

        let _ = fs::remove_dir_all(root);
    }

    /// An ARM7/ARM9/Cortex-A (A32) target must build with `-marm` and the
    /// exact `-mcpu` so the toolchain emits ARM-state code, not Thumb.
    #[test]
    fn arm_a32_build_plan_forces_arm_state_and_cpu() {
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-arm-a32-build-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\nlanguage = \"C\"\ntarget = \"lpc2148\"\ncompiler = \"arm-none-eabi-gcc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"c11\"\nopt = \"-O0\"\ndefines = []\nincludes = []\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

        let project = Project::open(&root).unwrap();
        let plan = plan_build(&project).unwrap();
        let arg_text = plan.args.join(" ");
        assert!(arg_text.contains("-marm"), "A32 build must force ARM state; got: {arg_text}");
        assert!(arg_text.contains("-mcpu=arm7tdmi"), "ARM7 must use arm7tdmi; got: {arg_text}");

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
        // `find_program` scans PATH; hold the lock so the PATH-mutating
        // clang-tidy/zig tests cannot race this assertion.
        let _guard = PATH_LOCK.lock().unwrap();
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

    /// `execute_stream` must deliver each output line to `on_line` and still
    /// populate stdout/stderr on the returned outcome.
    #[test]
    fn execute_stream_delivers_lines() {
        if cfg!(windows) {
            return;
        }
        let plan = BuildPlan {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo one; echo two >&2; echo three".into()],
            cwd: std::env::temp_dir(),
            env: Vec::new(),
            artifact: PathBuf::new(),
        };
        let mut lines: Vec<(bool, String)> = Vec::new();
        let outcome = execute_stream(&plan, |line| lines.push((line.is_stderr, line.text)));
        assert!(outcome.success, "shell echo should succeed");
        assert!(outcome.stdout.contains("one"));
        assert!(outcome.stdout.contains("three"));
        assert!(outcome.stderr.contains("two"));
        assert_eq!(lines.len(), 3, "each output line must reach on_line");
        assert!(lines.iter().any(|(stderr, _)| *stderr));
        assert!(lines.iter().any(|(stderr, _)| !*stderr));
    }

    /// A pre-set cancel flag must stop the build and report failure.
    #[test]
    fn execute_stream_cancel_stops_build() {
        if cfg!(windows) {
            return;
        }
        let plan = BuildPlan {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo start; exec sleep 30; echo never".into()],
            cwd: std::env::temp_dir(),
            env: Vec::new(),
            artifact: PathBuf::new(),
        };
        let cancel = Arc::new(AtomicBool::new(true));
        let mut lines: Vec<String> = Vec::new();
        let outcome = execute_stream_cancelable(&plan, cancel, |line| lines.push(line.text));
        assert!(!outcome.success, "cancelled build must not report success");
        assert!(outcome.stderr.contains("已取消"));
        assert!(lines.iter().any(|line| line.contains("start")));
        assert!(
            !lines.iter().any(|line| line.contains("never")),
            "output after cancellation must not appear"
        );
    }

    /// `clean_build` must remove the output directory and report which one was
    /// removed; a missing directory reports `None` instead of an error.
    #[test]
    fn clean_build_removes_output_dir() {
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-clean-{}",
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
        let out_dir = build_out_dir(&project);
        fs::create_dir_all(&out_dir).unwrap();
        fs::write(out_dir.join("app.elf"), "stub").unwrap();

        let removed = clean_build(&project).unwrap();
        assert_eq!(removed, Some(out_dir.clone()));
        assert!(!out_dir.exists(), "output directory must be gone");

        // A second clean has nothing to remove.
        assert_eq!(clean_build(&project).unwrap(), None);

        let _ = fs::remove_dir_all(root);
    }
    /// Static-analysis plan for a C project must pass the compiler's std/defines/includes
    /// through as clang-tidy args so that diagnostics are resolved against the same
    /// compilation database the build uses. The command-line must contain at least:
    ///   * `--std=c11` (from `[compiler] std`)
    ///   * `-DFOO=bar` and `-DBAZ` (from `[compiler] defines`)
    ///   * `-I/foo/include` (from `[compiler] includes`).
    #[test]
    fn static_analysis_plan_c_passes_compiler_flags() {
        let _guard = PATH_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-static-plan-c-flags-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"flags-test\"\nversion = \"0.1.0\"\nlanguage = \"C\"\ntarget = \"x86_64\"\ncompiler = \"gcc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"c11\"\nopt = \"-O2\"\ndefines = [\"FOO=bar\", \"BAZ\"]\nincludes = [\"/foo/include\"]\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

        let saved_path = std::env::var_os("PATH").unwrap_or_default();
        let bin_dir = root.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        // Write a dummy `clang-tidy` that only prints its argv so we can inspect the plan without needing the real tool.
        let script = r#"#!/usr/bin/env sh
printf '%s\n' "$@"
"#;
        fs::write(bin_dir.join("clang-tidy"), script).unwrap();
        #[cfg(windows)]
        fs::write(bin_dir.join("clang-tidy.exe"), script).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(bin_dir.join("clang-tidy"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

        let mut path = std::env::split_paths(&saved_path).collect::<Vec<_>>();
        path.insert(0, bin_dir);
            unsafe { std::env::set_var("PATH", std::env::join_paths(path).unwrap()); }

        let project = Project::open(&root).unwrap();
        let plan = plan_static_analysis(&project);
        assert!(plan.is_some(), "must produce a plan when clang-tidy is on PATH");
        let plan = plan.unwrap();

        let arg_text = plan.args.join(" ");
        assert!(arg_text.contains("--std=c11"), "args should contain --std=c11; got: {arg_text}");
        assert!(arg_text.contains("-DFOO=bar"), "args should contain -DFOO=bar; got: {arg_text}");
        assert!(arg_text.contains("-DBAZ"), "args should contain -DBAZ; got: {arg_text}");
        assert!(arg_text.contains("/foo/include"), "args should contain the include dir; got: {arg_text}");
        assert!(arg_text.contains("main.c"), "args should reference the source file; got: {arg_text}");

            unsafe { std::env::set_var("PATH", saved_path); }
        let _ = fs::remove_dir_all(root);
    }

    /// Static-analysis plan for a Zig project must pass the target triple the
    /// build uses (MCU core → bare-metal triple) and `-Werror` so analyzer
    /// diagnostics match the real build and surface in the problems panel.
    #[test]
    fn static_analysis_plan_zig_passes_target_and_werror() {
        let _guard = PATH_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-static-plan-zig-flags-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        // stm32f407vg is Cortex-M4 → arm-none-eabi.
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"zig-flags-test\"\nversion = \"0.1.0\"\nlanguage = \"Zig\"\ntarget = \"stm32f407vg\"\ncompiler = \"zig\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"\"\nopt = \"-O0\"\ndefines = []\nincludes = []\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.zig"), "pub fn main() void {}\n").unwrap();

        let saved_path = std::env::var_os("PATH").unwrap_or_default();
        let bin_dir = root.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::write(bin_dir.join("zig"), "#!/usr/bin/env sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        #[cfg(windows)]
        fs::write(bin_dir.join("zig.exe"), "#!/usr/bin/env sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(bin_dir.join("zig"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

        let mut path = std::env::split_paths(&saved_path).collect::<Vec<_>>();
        path.insert(0, bin_dir);
        unsafe { std::env::set_var("PATH", std::env::join_paths(path).unwrap()); }

        let project = Project::open(&root).unwrap();
        let plan = plan_static_analysis(&project);
        assert!(plan.is_some(), "must produce a plan when zig is on PATH");
        let plan = plan.unwrap();

        let arg_text = plan.args.join(" ");
        assert!(
            arg_text.contains("-target") && arg_text.contains("arm-none-eabi"),
            "args should target arm-none-eabi for a Cortex-M4; got: {arg_text}"
        );
        assert!(
            arg_text.contains("-Werror"),
            "args should include -Werror; got: {arg_text}"
        );
        assert!(
            arg_text.contains("-fno-emit-bin"),
            "args should include -fno-emit-bin; got: {arg_text}"
        );

        unsafe { std::env::set_var("PATH", saved_path); }
        let _ = fs::remove_dir_all(root);
    }

    /// A C project with a CMakeLists.txt and `[build] system` unset (auto)
    /// must resolve to a CMake build plan: `cmake -S . -B build/cmake` with
    /// `CMAKE_BUILD_TYPE` set from the effective config.
    #[test]
    fn cmake_build_plan_resolves_auto_and_forwards_flags() {
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-cmake-plan-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"cmake-test\"\nversion = \"0.1.0\"\nlanguage = \"C\"\ntarget = \"stm32f103c8\"\ncompiler = \"arm-none-eabi-gcc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"c11\"\nopt = \"-O2\"\ndefines = [\"STM32F103xB\"]\nincludes = [\"include\"]\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
        fs::write(root.join("CMakeLists.txt"), "cmake_minimum_required(VERSION 3.16)\nproject(x C)\n").unwrap();
        fs::create_dir_all(root.join("include")).unwrap();

        let project = Project::open(&root).unwrap();
        let plan = plan_build(&project).unwrap();
        // The plan wraps `cmake -S ... -B ... && cmake --build ...` in `sh -c`.
        assert_eq!(plan.program, "sh", "auto must prefer CMake when CMakeLists.txt exists");
        let shell = plan.args.iter().find(|a| a.contains("cmake")).cloned().unwrap_or_default();
        assert!(shell.contains("-S"), "configure: {shell}");
        assert!(shell.contains("-B"), "build tree: {shell}");
        assert!(shell.contains("--build"), "build step: {shell}");
        assert!(shell.contains("CMAKE_BUILD_TYPE=Debug"), "debug config: {shell}");
        assert!(shell.contains("-DNS_IDE_DEFINES=STM32F103xB"), "defines forwarded: {shell}");
        assert!(shell.contains("-DNS_IDE_INCLUDES="), "includes forwarded: {shell}");
        assert!(shell.contains("CMAKE_RUNTIME_OUTPUT_DIRECTORY"), "artifact dir forced: {shell}");
        assert!(shell.contains("-j "), "parallel jobs passed to cmake --build: {shell}");
        assert!(
            shell.contains(&format!("-j {}", project.build_jobs())),
            "job count: {shell}"
        );

        let _ = fs::remove_dir_all(root);
    }

    /// A C project with a Makefile and `[build] system = "make"` must resolve
    /// to a make plan passing the build/config/output variables.
    #[test]
    fn make_build_plan_resolves_and_passes_variables() {
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-make-plan-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"make-test\"\nversion = \"0.1.0\"\nlanguage = \"C\"\ntarget = \"stm32f103c8\"\ncompiler = \"arm-none-eabi-gcc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"c11\"\nopt = \"-O2\"\ndefines = []\nincludes = []\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
        fs::write(root.join("Makefile"), "all:\n\t@true\n").unwrap();

        let project = Project::open(&root).unwrap();
        let plan = plan_build(&project).unwrap();
        assert_eq!(plan.program, "make");
        assert!(plan.args.iter().any(|a| a == "-f"), "makefile flag: {:?}", plan.args);
        assert!(
            plan.args.iter().any(|a| a.starts_with("-j")),
            "parallel jobs passed to make: {:?}",
            plan.args
        );
        assert!(
            plan.args.iter().any(|a| a.starts_with("BUILD_DIR=")),
            "build dir passed: {:?}",
            plan.args
        );
        assert!(
            plan.args.iter().any(|a| a.starts_with("CONFIG=")),
            "config passed: {:?}",
            plan.args
        );
        assert!(
            plan.args.iter().any(|a| a.starts_with("OUTPUT=")),
            "output passed: {:?}",
            plan.args
        );

        let _ = fs::remove_dir_all(root);
    }

    /// A Rust project using `cargo` must pass `-j <jobs>` to `cargo build` so
    /// parallel compilation is honored (F5 并行构建).
    #[test]
    fn cargo_build_plan_passes_parallel_jobs() {
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-cargo-plan-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"rust-test\"\nversion = \"0.1.0\"\nlanguage = \"Rust\"\ntarget = \"stm32f103c8\"\ncompiler = \"cargo\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"\"\nopt = \"\"\ndefines = []\nincludes = []\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\njobs = 4\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"rust-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();

        let project = Project::open(&root).unwrap();
        let plan = plan_build(&project).unwrap();
        assert_eq!(plan.program, "cargo");
        assert!(plan.args.iter().any(|a| a == "build"), "cargo build: {:?}", plan.args);
        let jobs_index = plan
            .args
            .iter()
            .position(|a| a == "-j")
            .expect("cargo build -j present");
        assert_eq!(plan.args[jobs_index + 1], "4", "configured jobs honored");

        let _ = fs::remove_dir_all(root);
    }

    /// `[build] system = "direct"` must bypass CMake/Makefile detection and
    /// compile all sources in one command (the pre-existing behavior).
    #[test]
    fn direct_build_plan_ignores_existing_cmake() {
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-direct-plan-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"direct-test\"\nversion = \"0.1.0\"\nlanguage = \"C\"\ntarget = \"x86_64\"\ncompiler = \"cc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"c11\"\nopt = \"-O0\"\ndefines = []\nincludes = []\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\nsystem = \"direct\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
        fs::write(root.join("CMakeLists.txt"), "cmake_minimum_required(VERSION 3.16)\nproject(x C)\n").unwrap();

        let project = Project::open(&root).unwrap();
        let plan = plan_build(&project).unwrap();
        assert_eq!(plan.program, "cc", "direct must ignore CMakeLists.txt");
        assert!(
            plan.args.iter().any(|a| a.ends_with("src/main.c")),
            "source passed: {:?}",
            plan.args
        );


        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn direct_build_plan_appends_link_flags() {
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-linkflags-plan-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"linkflags-test\"\nversion = \"0.1.0\"\nlanguage = \"C\"\ntarget = \"x86_64\"\ncompiler = \"cc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"c11\"\nopt = \"-O0\"\ndefines = []\nincludes = []\nlink_flags = [\"-lm\", \"-Wl,--gc-sections\"]\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\nsystem = \"direct\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

        let project = Project::open(&root).unwrap();
        let plan = plan_build(&project).unwrap();
        let command = plan.command_line();
        // Link flags land after the object files (before `-o`), so the linker
        // sees them on the final link invocation.
        let source_pos = plan.args.iter().position(|a| a.ends_with("src/main.c"));
        let flag_pos = plan
            .args
            .iter()
            .position(|a| a.as_str() == "-lm")
            .expect("-lm must be present");
        let out_pos = plan.args.iter().position(|a| a.as_str() == "-o");
        assert!(
            source_pos.unwrap() < flag_pos && flag_pos < out_pos.unwrap(),
            "link flags must follow sources and precede -o: {command}"
        );
        assert!(plan.args.iter().any(|a| a == "-Wl,--gc-sections"));

        let _ = fs::remove_dir_all(root);
    }


    /// End-to-end: a C project with a CMakeLists.txt and a host compiler builds
    /// through the `sh -c` plan, and the artifact appears where
    /// `artifact_path()` predicts. Skipped when `cmake` is not on PATH.
    #[test]
    fn cmake_build_plan_executes_end_to_end() {
        if ns_project::toolchain::find_command("cmake").is_none() {
            eprintln!("skipping: cmake not on PATH");
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-cmake-e2e-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"cmake-e2e\"\nversion = \"0.1.0\"\nlanguage = \"C\"\ntarget = \"x86_64\"\ncompiler = \"cc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"c11\"\nopt = \"-O0\"\ndefines = [\"GREET=42\"]\nincludes = []\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(
            root.join("src/main.c"),
            "#include <stdio.h>\n#ifndef GREET\n#error \"GREET missing\"\n#endif\nint main(void){printf(\"%d\\n\",GREET);return 0;}\n",
        )
        .unwrap();
        fs::write(
            root.join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.16)\nproject(cmake_e2e C)\nset(CMAKE_C_STANDARD 11)\nfile(GLOB S CONFIGURE_DEPENDS \"${CMAKE_SOURCE_DIR}/src/*.c\")\nadd_executable(cmake-e2e ${S})\nif(DEFINED NS_IDE_DEFINES)\n  target_compile_definitions(cmake-e2e PRIVATE ${NS_IDE_DEFINES})\nendif()\nset_target_properties(cmake-e2e PROPERTIES SUFFIX \".elf\")\n",
        )
        .unwrap();

        let project = Project::open(&root).unwrap();
        let plan = plan_build(&project).unwrap();
        assert_eq!(plan.program, "sh");
        let mut lines = Vec::new();
        let outcome = execute_stream(&plan, |line| lines.push(line.text.clone()));
        assert!(
            outcome.success,
            "cmake build must succeed; stderr: {}",
            outcome.stderr
        );
        assert!(
            plan.artifact.is_file(),
            "artifact must exist at {:?}",
            plan.artifact
        );
        assert!(
            lines.iter().any(|l| l.contains("Built target")),
            "build progress streamed: {lines:?}"
        );

        let _ = fs::remove_dir_all(root);
    }

    /// `plan_tests` selects the language's unit-test runner from the project
    /// layout: `cargo test` for Rust, `ctest` for CMake, `make test` for
    /// Makefile, `zig build test` for Zig with `build.zig`, and `None` when no
    /// runner exists (ZR / assembly / plain direct-compile C with no harness).
    #[test]
    fn plans_test_runner_by_language_and_build_system() {
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-plan-tests-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();

        // Rust → cargo test with --target-dir and parallel jobs.
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\nlanguage = \"Rust\"\ntarget = \"x86_64\"\ncompiler = \"cargo\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"2021\"\nopt = \"\"\ndefines = []\nincludes = []\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let project = Project::open(&root).unwrap();
        let plan = plan_tests(&project).expect("rust has a test runner");
        assert!(plan.program.ends_with("cargo") || plan.program.ends_with("cargo.exe"));
        assert!(plan.args.first().map(String::as_str) == Some("test"));
        assert!(plan.args.iter().any(|a| a == "--target-dir"));
        assert!(plan.args.iter().any(|a| a == "-j"));
        let _ = fs::remove_dir_all(&root);

        // C + CMakeLists.txt → ctest --test-dir <build>/cmake.
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\nlanguage = \"C\"\ntarget = \"x86_64\"\ncompiler = \"cc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"c11\"\nopt = \"-O0\"\ndefines = []\nincludes = []\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("CMakeLists.txt"), "cmake_minimum_required(VERSION 3.16)\nproject(t)\nadd_executable(t src/main.c)\n")
            .unwrap();
        fs::write(root.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
        let project = Project::open(&root).unwrap();
        let plan = plan_tests(&project).expect("cmake project has ctest");
        assert!(plan.program.ends_with("ctest") || plan.program.ends_with("ctest.exe"));
        assert!(plan.args.iter().any(|a| a == "--test-dir"));
        assert!(plan.artifact.ends_with("cmake"));
        let _ = fs::remove_dir_all(&root);

        // C + Makefile → make test.
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\nlanguage = \"C\"\ntarget = \"x86_64\"\ncompiler = \"cc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"c11\"\nopt = \"-O0\"\ndefines = []\nincludes = []\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("Makefile"), "test:\n\t@echo ok\n").unwrap();
        fs::write(root.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
        let project = Project::open(&root).unwrap();
        let plan = plan_tests(&project).expect("makefile project has make test");
        assert!(plan.program.ends_with("make") || plan.program.ends_with("make.exe"));
        assert_eq!(plan.args, vec!["test".to_string()]);
        let _ = fs::remove_dir_all(&root);

        // ZR → no runner.
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\nlanguage = \"ZR\"\ntarget = \"riscv32\"\ncompiler = \"zr\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"\"\nopt = \"\"\ndefines = []\nincludes = []\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.zr"), "fn main() {}\n").unwrap();
        let project = Project::open(&root).unwrap();
        assert!(plan_tests(&project).is_none(), "ZR has no in-IDE runner");
        let _ = fs::remove_dir_all(&root);
    }

    /// `parse_stack_usage_su` reads GCC `-fstack-usage` output, including the
    /// `static`/`dynamic` marker; `plan_stack_usage` only plans C/C++ projects
    /// and always passes `-fstack-usage -c` with per-source objects.
    #[test]
    fn parses_stack_usage_and_plans_only_for_c() {
        let text = "\tmain.c\tmain\t16\tstatic\n\tmain.c\tfoo\t48\tdynamic\n";
        let entries = parse_stack_usage_su(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].function, "main");
        assert_eq!(entries[0].bytes, 16);
        assert!(!entries[0].dynamic);
        assert_eq!(entries[1].function, "foo");
        assert_eq!(entries[1].bytes, 48);
        assert!(entries[1].dynamic);

        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-stack-usage-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\nlanguage = \"C\"\ntarget = \"stm32f103c8\"\ncompiler = \"arm-none-eabi-gcc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"c11\"\nopt = \"-O2\"\ndefines = [\"STM32F103xB\"]\nincludes = [\"src\"]\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();

        let project = Project::open(&root).unwrap();
        let plan = plan_stack_usage(&project).expect("C project has stack plan");
        assert!(plan.args.contains(&"-fstack-usage".to_string()));
        assert!(plan.args.contains(&"-c".to_string()));
        assert!(plan.args.iter().any(|a| a == "-DSTM32F103xB"));
        assert!(
            plan.args.iter().any(|a| a.starts_with("-I") && a.ends_with("/src")),
            "include flag must point at project src: {:?}",
            plan.args
        );
        assert!(plan.artifact.ends_with("stack-usage"));

        // Non-C project → no plan.
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\nlanguage = \"Rust\"\ntarget = \"x86_64\"\ncompiler = \"cargo\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"2021\"\nopt = \"\"\ndefines = []\nincludes = []\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let project = Project::open(&root).unwrap();
        assert!(plan_stack_usage(&project).is_none(), "Rust has no stack plan");
        let _ = fs::remove_dir_all(&root);
    }

    /// `parse_gcov_report` reads gcov's aggregate `File '<path>'` + 
    /// `Lines executed:<pct>% of <n>` summary; `plan_coverage` only plans C/C++
    /// projects and always passes `--coverage`.
    #[test]
    fn parses_gcov_report_and_plans_coverage_only_for_c() {
        let report = "File 'src/main.c'\nLines executed:100.00% of 2\n\
                      File 'src/util.c'\nLines executed:50.00% of 4\n";
        let files = parse_gcov_report(report);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].file, "src/main.c");
        assert_eq!(files[0].executed, 2);
        assert_eq!(files[0].not_executed, 0);
        assert_eq!(files[0].percent(), 100);
        assert_eq!(files[1].file, "src/util.c");
        assert_eq!(files[1].executed, 2);
        assert_eq!(files[1].not_executed, 2);
        assert_eq!(files[1].percent(), 50);

        // Non-C project → no coverage plan.
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-coverage-plan-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\nlanguage = \"Rust\"\ntarget = \"x86_64\"\ncompiler = \"cargo\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"2021\"\nopt = \"\"\ndefines = []\nincludes = []\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let project = Project::open(&root).unwrap();
        assert!(plan_coverage(&project).is_none(), "Rust has no coverage plan");
        let _ = fs::remove_dir_all(&root);
     }

    /// `parse_gcov_lines` reads gcov's `-t` per-line output: executable rows
    /// carry a hit count (or `#####` for never-executed), non-executable `-`
    /// rows are omitted, and the result is indexed by 0-based line.
    #[test]
    fn parses_gcov_line_hits() {
        let report = "        -:    0:File 'src/main.c'\n\
                      2:    1:int main(void) {\n\
                      1:    2:    return 0;\n\
                      -:    3:}\n";
        let lines = parse_gcov_lines(report);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, "src/main.c");
        let hits = &lines[0].1;
        assert_eq!(hits.len(), 2, "non-executable line 3 omitted");
        assert_eq!(hits[0], 2);
        assert_eq!(hits[1], 1);
        // A never-executed row is recorded as a 0 hit.
        let report = "        -:    0:File 'src/main.c'\n\
                      #####:    1:int main(void) {\n\
                      -:    2:}\n";
        let lines = parse_gcov_lines(report);
        assert_eq!(lines[0].1[0], 0);
    }

    /// The aggregate report parser defaults `line_hits` to `None` (line-level
    /// data is attached separately by `run_coverage`).
    #[test]
    fn gcov_aggregate_report_has_no_line_hits_by_default() {
        let report = "File 'src/main.c'\nLines executed:100.00% of 2\n";
        let files = parse_gcov_report(report);
        assert_eq!(files[0].line_hits, None);
    }
    /// End-to-end: build a tiny C file with `--coverage`, run it, then gcov it
    /// and confirm `run_coverage` returns per-file line coverage. Skips when
    /// gcov or a host compiler is unavailable (CI without gcc).
    #[test]
    fn coverage_runs_end_to_end_with_gcov() {
        let root = std::env::temp_dir().join(format!(
            "ns-embed-ide-coverage-e2e-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("project.toml"),
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\nlanguage = \"C\"\ntarget = \"x86_64\"\ncompiler = \"cc\"\nsimulator = \"\"\ndebugger = \"lldb\"\nflasher = \"\"\n\n[compiler]\nstd = \"c11\"\nopt = \"-O0\"\ndefines = []\nincludes = []\n\n[debugger]\ninterface = \"swd\"\n\n[flasher]\ninterface = \"swd\"\n\n[build]\nconfig = \"debug\"\nout_dir = \"build\"\n\n[sim]\ncpu = \"\"\nperiod = 0\n",
        )
        .unwrap();
        fs::write(
            root.join("src/main.c"),
            "int covered(int x) { if (x > 0) return 1; return 0; }\n\
             int main(void) { return covered(5); }\n",
        )
        .unwrap();
        let project = Project::open(&root).unwrap();
        let Some(plan) = plan_coverage(&project) else {
            // No cc/gcov in this environment; skip.
            let _ = fs::remove_dir_all(&root);
            return;
        };
        // Build with coverage instrumentation.
        let build = execute(&plan);
        if !build.success {
            let _ = fs::remove_dir_all(&root);
            return;
        }
        let out_dir = plan.artifact.parent().unwrap().to_path_buf();
        let files = run_coverage(&plan.artifact, &root, &out_dir).unwrap();
        if let Some(files) = files {
            assert!(
                files.iter().any(|file| file.file.contains("main.c")),
                "main.c should appear in coverage: {files:?}"
            );
            assert!(
                files.iter().all(|file| file.executed > 0),
                "the executed path should cover lines: {files:?}"
            );
        }
        let _ = fs::remove_dir_all(&root);
    }
}
