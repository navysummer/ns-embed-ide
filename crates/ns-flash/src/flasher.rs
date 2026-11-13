//! Flasher adapters. Each adapter knows how to invoke its external tool to
//! flash / erase / verify / read back a firmware image, and reports progress.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use crate::firmware::{FirmwareFormat, load_firmware};
use ns_project::Project;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashAction {
    Flash,
    Erase,
    Verify,
    Read,
}

impl FlashAction {
    pub fn label(self) -> &'static str {
        match self {
            FlashAction::Flash => "烧写",
            FlashAction::Erase => "擦除",
            FlashAction::Verify => "校验",
            FlashAction::Read => "读回",
        }
    }
}

/// Which external flasher tool to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashTool {
    OpenOcd,
    PyOcd,
    JLink,
    StLink,
    ProbeRs,
}

impl FlashTool {
    pub fn label(self) -> &'static str {
        match self {
            FlashTool::OpenOcd => "OpenOCD",
            FlashTool::PyOcd => "pyOCD",
            FlashTool::JLink => "J-Link Commander",
            FlashTool::StLink => "ST-Link",
            FlashTool::ProbeRs => "probe-rs",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FlashRequest {
    pub tool: FlashTool,
    pub action: FlashAction,
    pub firmware: PathBuf,
    /// Debug interface, e.g. `swd` or `jtag`.
    pub interface: String,
    /// Read-back output file (only meaningful for `FlashAction::Read`).
    pub read_to: Option<PathBuf>,
    /// Optional adapter serial/device overrides, passed through to the tool.
     pub device: Option<String>,
     /// Optional external flash-algorithm geometry (from a `.FLM` file). When
     /// present, adapters use its page size / base address to configure the
     /// flash programming.
     pub algorithm: Option<crate::flash_algorithm::FlashAlgorithm>,
    /// When set, the flash subprocess is killed after the flag is observed
    /// (polled while streaming output), so a long erase/verify can be aborted.
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
 }

#[derive(Debug, Clone)]
pub struct FlashOutcome {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
    pub tool: FlashTool,
    pub action: FlashAction,
}

impl FlashOutcome {
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

/// A flasher adapter: how to invoke one external flashing tool.
pub trait Flasher: Send + Sync {
    fn tool(&self) -> FlashTool;
    /// Build and run the tool for `request`. `on_line` is invoked for each
    /// line of combined stdout/stderr as it is produced, so the UI can show
    /// progress without waiting for completion. Returns the outcome; the
    /// caller decides how to surface it.
    fn run(&self, request: &FlashRequest, on_line: &mut dyn FnMut(&str)) -> Result<FlashOutcome>;
}

/// Resolve the target device name for a flasher adapter, falling back to the
/// adapter's historical default when `project.toml` has no target.
fn device_or(device: &Option<String>, fallback: &str) -> String {
    device
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or(fallback)
        .to_string()
}


/// Choose the adapter from `project.toml`'s `[flasher]`. Empty `kind` selects
/// `openocd` by default; an unknown kind is an error so the IDE can report it.
pub fn adapter_for(project: &Project) -> Result<Box<dyn Flasher>> {
    let kind = project.config.flasher.kind.trim().to_ascii_lowercase();
    let tool = match kind.as_str() {
        "" | "openocd" => FlashTool::OpenOcd,
        "pyocd" | "py-ocd" => FlashTool::PyOcd,
        "jlink" | "j-link" | "jlinkcommander" => FlashTool::JLink,
        "stlink" | "st-link" | "st-util" => FlashTool::StLink,
        "probe-rs" | "probers" | "probe_rs" => FlashTool::ProbeRs,
        other => bail!(
            "未知烧写器类型: {other}（应为 openocd / pyocd / jlink / stlink / probe-rs）"
        ),
    };
    Ok(match tool {
        FlashTool::OpenOcd => Box::new(OpenOcd::new(&project.config.flasher.path)),
        FlashTool::PyOcd => Box::new(PyOcd::new(&project.config.flasher.path)),
        FlashTool::JLink => Box::new(JLink::new(&project.config.flasher.path)),
        FlashTool::StLink => Box::new(StLink::new(&project.config.flasher.path)),
        FlashTool::ProbeRs => Box::new(ProbeRs::new(&project.config.flasher.path)),
    })
}

/// Locate the flash tool binary, honoring an explicit configured path first.
fn resolve_program(configured: &str, candidates: &[&str]) -> String {
    if !configured.trim().is_empty() {
        return configured.trim().to_string();
    }
    for candidate in candidates {
        if let Ok(path) = which(candidate) {
            return path;
        }
    }
    candidates[0].to_string()
}

fn which(program: &str) -> Result<String> {
    let paths = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }
    bail!("{} 不在 PATH 中", program)
}

/// Run a command and capture its output. `stdin` is written to the child's
/// standard input when provided (used by J-Link Commander). `on_line` receives
/// each line of stdout/stderr as it is produced (streaming progress).
 fn run_command(
     program: &str,
     args: &[String],
     cwd: &Path,
     stdin: Option<&str>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
     on_line: &mut dyn FnMut(&str),
 ) -> Result<FlashOutcome> {
    use std::io::{BufRead, BufReader, Write};
    let start = Instant::now();
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("启动 {} 失败", program))?;
    if let Some(input) = stdin {
        if let Some(mut handle) = child.stdin.take() {
            let _ = handle.write_all(input.as_bytes());
        }
    }
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut out_reader = BufReader::new(child.stdout.take().expect("stdout piped"));
    let mut err_reader = BufReader::new(child.stderr.take().expect("stderr piped"));
    // Read stdout fully first, streaming each line; then drain stderr. This
    // keeps the tool's normal progress output visible as it is produced.
     let mut buf = String::new();
    let mut cancelled = false;
     loop {
        if let Some(flag) = cancel {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                cancelled = true;
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
         buf.clear();
        match out_reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(_) => {
                on_line(buf.trim_end());
                stdout.push_str(&buf);
            }
            Err(_) => break,
        }
    }
     while let Ok(n) = err_reader.read_line(&mut buf) {
        if let Some(flag) = cancel {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                cancelled = true;
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
        if n == 0 {
            break;
        }
        on_line(buf.trim_end());
        stderr.push_str(&buf);
        buf.clear();
    }
    let status = child.wait()?;
    let duration = start.elapsed();
    let success = status.success() && !cancelled;
    let code = status.code();
    Ok(FlashOutcome {
        success,
        code,
        stdout,
        stderr,
        duration,
        tool: FlashTool::OpenOcd,
        action: FlashAction::Flash,
    })
}

/// OpenOCD adapter: `openocd -f interface.cfg -f target.cfg -c "program ..."`.
pub struct OpenOcd {
    program: String,
}

impl OpenOcd {
    pub fn new(configured: &str) -> Self {
        Self {
            program: resolve_program(configured, &["openocd"]),
        }
    }

     fn base_args(&self, request: &FlashRequest) -> Vec<String> {
         let interface = match request.interface.as_str() {
             "jtag" => "jtag",
             _ => "swd",
         };
        let target_cfg = openocd_target_cfg(request.device.as_deref().unwrap_or(""));
         vec![
             format!("-f interface/{interface}.cfg"),
             "-f".to_string(),
            target_cfg,
             "-c".to_string(),
             format!("adapter speed 1000"),
         ]
     }
 }

/// Map a `project.toml` target key to the OpenOCD target config it needs.
/// Unrecognised targets fall back to `stm32f1x.cfg` (the historical default).
pub fn openocd_target_cfg(device: &str) -> String {
    let key = device.trim().to_ascii_lowercase();
    let cfg = match key.as_str() {
        // STM32 family -> the matching OpenOCD target config.
        "stm32f103c8" | "stm32f103" | "stm32f1" | "stm32f103c6" | "stm32f103rb" => "stm32f1x",
        "stm32f407vg" | "stm32f407" | "stm32f4" | "stm32f405" | "stm32f427" | "stm32f429" => {
            "stm32f4x"
        }
        "stm32f030" | "stm32f0" | "stm32f042" | "stm32f051" | "stm32f072" => "stm32f0x",
        "stm32f746" | "stm32f7" | "stm32f745" | "stm32f769" => "stm32f7x",
        "stm32l0" | "stm32l053" | "stm32l072" => "stm32l0x",
        "stm32l4" | "stm32l476" | "stm32l432" | "stm32l433" => "stm32l4x",
        "stm32g0" | "stm32g031" | "stm32g071" => "stm32g0x",
        "stm32g4" | "stm32g431" | "stm32g474" => "stm32g4x",
        "stm32h7" | "stm32h743" | "stm32h750" => "stm32h7x",
        "stm32wb" | "stm32wb55" => "stm32wbx",
        "nrf52" | "nrf52832" | "nrf52840" => "nrf52",
        "rp2040" | "rp2350" => "rp2040",
        "lpc2148" | "lpc21xx" | "lpc22xx" => "lpc2100",
        "at91sam7" | "at91sam7s" => "at91sam7",
        "stm8s" | "stm8s103" => "stm8s103",
        _ => "stm32f1x",
    };
    format!("target/{cfg}.cfg")
}

impl Flasher for OpenOcd {
    fn tool(&self) -> FlashTool {
        FlashTool::OpenOcd
    }

    fn run(&self, request: &FlashRequest, on_line: &mut dyn FnMut(&str)) -> Result<FlashOutcome> {
        let image = load_firmware(&request.firmware)?;
        // Optional FLM geometry: use the algorithm's base as the program /
        // verify / readback address; a page size drives the erase span.
        let flash_base = request
            .algorithm
            .as_ref()
            .map(|flm| flm.base_address)
            .unwrap_or(image.base as u32);
        let page_size = request
            .algorithm
            .as_ref()
            .map(|flm| flm.page_size)
            .unwrap_or(0);
        let action_args = match request.action {
            FlashAction::Flash => {
                let cmd = match image.format {
                    FirmwareFormat::Elf => {
                        format!("program {} verify reset", request.firmware.display())
                    }
                    _ => format!(
                        "program {} {:#x} verify reset",
                        request.firmware.display(),
                        flash_base
                    ),
                };
                vec!["-c".to_string(), cmd]
            }
            FlashAction::Erase => {
                let cmd = if page_size > 0 {
                    format!("flash erase_address {:#x} {}", flash_base, page_size)
                } else {
                    "erase flash".to_string()
                };
                vec!["-c".to_string(), cmd]
            }
            FlashAction::Verify => {
                let cmd = format!(
                    "verify_image {} {:#x}",
                    request.firmware.display(),
                    flash_base
                );
                vec!["-c".to_string(), cmd]
            }
            FlashAction::Read => {
                let out = request.read_to.clone().unwrap_or_else(|| {
                    request.firmware.with_extension("readback.bin")
                });
                let cmd = format!(
                    "dump_image {} {:#x} {}",
                    out.display(),
                    flash_base,
                    image.bytes.len()
                );
                vec!["-c".to_string(), cmd]
            }
        };
        let mut args = self.base_args(request);
        args.extend(action_args);
        args.extend(["-c".to_string(), "shutdown".to_string()]);
        let mut outcome = run_command(&self.program, &args, &request.firmware.parent().unwrap_or(Path::new(".")), None, request.cancel.as_deref(), on_line)?;
        outcome.tool = FlashTool::OpenOcd;
        outcome.action = request.action;
        Ok(outcome)
    }
}

/// pyOCD adapter: `pyocd flash -t <target> <firmware>`.
pub struct PyOcd {
    program: String,
}

impl PyOcd {
    pub fn new(configured: &str) -> Self {
        Self {
            program: resolve_program(configured, &["pyocd"]),
        }
    }
}

impl Flasher for PyOcd {
    fn tool(&self) -> FlashTool {
        FlashTool::PyOcd
    }

    fn run(&self, request: &FlashRequest, on_line: &mut dyn FnMut(&str)) -> Result<FlashOutcome> {
        let device = request
            .device
            .clone()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "stm32f103c8".to_string());
        let args: Vec<String> = match request.action {
            FlashAction::Flash => vec![
                "flash".to_string(),
                "-t".to_string(),
                device.clone(),
                request.firmware.to_string_lossy().into_owned(),
            ],
            FlashAction::Erase => vec![
                "erase".to_string(),
                "-t".to_string(),
                device.clone(),
            ],
            FlashAction::Verify => vec![
                "verify".to_string(),
                "-t".to_string(),
                device.clone(),
                request.firmware.to_string_lossy().into_owned(),
            ],
            FlashAction::Read => {
                let out = request.read_to.clone().unwrap_or_else(|| {
                    request.firmware.with_extension("readback.bin")
                });
                vec![
                    "save".to_string(),
                    "-t".to_string(),
                    device.clone(),
                    out.to_string_lossy().into_owned(),
                ]
            }
        };
        let mut outcome = run_command(&self.program, &args, &request.firmware.parent().unwrap_or(Path::new(".")), None, request.cancel.as_deref(), on_line)?;
        outcome.tool = FlashTool::PyOcd;
        outcome.action = request.action;
        Ok(outcome)
    }
}

/// J-Link Commander adapter: `JLinkExe -device <dev> -if <interface> -speed 4000 -autoconnect 1`.
pub struct JLink {
    program: String,
}

impl JLink {
    pub fn new(configured: &str) -> Self {
        Self {
            program: resolve_program(configured, &["JLinkExe"]),
        }
    }
}

impl Flasher for JLink {
    fn tool(&self) -> FlashTool {
        FlashTool::JLink
    }

    fn run(&self, request: &FlashRequest, on_line: &mut dyn FnMut(&str)) -> Result<FlashOutcome> {
        let device = request
            .device
            .clone()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "STM32F103C8".to_string());
        let interface = match request.interface.as_str() {
            "jtag" => "JTAG",
            _ => "SWD",
        };
        // Optional FLM geometry: J-Link's `verifybin`/`savebin` take an explicit
        // base address; the algorithm's base refines the default 0x0800_0000.
        let flash_base = request
            .algorithm
            .as_ref()
            .map(|flm| flm.base_address)
            .unwrap_or(0x0800_0000);
        let mut args = vec![
            "-device".to_string(),
            device,
            "-if".to_string(),
            interface.to_string(),
            "-speed".to_string(),
            "4000".to_string(),
            "-autoconnect".to_string(),
            "1".to_string(),
        ];
        // J-Link Commander reads commands from stdin.
        let mut commands = String::new();
        match request.action {
            FlashAction::Flash => {
                commands.push_str(&format!("loadfile {}\n", request.firmware.display()));
            }
            FlashAction::Erase => {
                commands.push_str("erase\n");
            }
            FlashAction::Verify => {
                commands.push_str(&format!(
                    "verifybin {} {:#x}\n",
                    request.firmware.display(),
                    flash_base
                ));
            }
            FlashAction::Read => {
                let out = request.read_to.clone().unwrap_or_else(|| {
                    request.firmware.with_extension("readback.bin")
                });
                let size = request
                    .algorithm
                    .as_ref()
                    .map(|flm| flm.device_size)
                    .unwrap_or(0x8000);
                commands.push_str(&format!(
                    "savebin {} {:#x} {:#x}\n",
                    out.display(),
                    flash_base,
                    size
                ));
            }
        }
        commands.push_str("exit\n");
        args.push(commands.clone());
        let mut outcome = run_command(&self.program, &args, &request.firmware.parent().unwrap_or(Path::new(".")), Some(&commands), request.cancel.as_deref(), on_line)?;
        outcome.tool = FlashTool::JLink;
        outcome.action = request.action;
        Ok(outcome)
    }
}

/// ST-Link flasher adapter: `st-flash write <firmware> <base>` (ST-Link CLI).
pub struct StLink {
    program: String,
}

impl StLink {
    pub fn new(configured: &str) -> Self {
        Self {
            program: resolve_program(configured, &["st-flash"]),
        }
    }
}

impl Flasher for StLink {
    fn tool(&self) -> FlashTool {
        FlashTool::StLink
    }

    fn run(&self, request: &FlashRequest, on_line: &mut dyn FnMut(&str)) -> Result<FlashOutcome> {
        // ST-Link's `write`/`verify`/`read` take an explicit base address; use
        // the FLM geometry when present, else the historical STM32 flash base.
        let base = request
            .algorithm
            .as_ref()
            .map(|flm| format!("{:#x}", flm.base_address))
            .unwrap_or_else(|| "0x08000000".to_string());
        let args: Vec<String> = match request.action {
            FlashAction::Flash => vec![
                "write".to_string(),
                request.firmware.to_string_lossy().into_owned(),
                base.to_string(),
            ],
            FlashAction::Erase => vec!["erase".to_string()],
            FlashAction::Verify => vec![
                "verify".to_string(),
                request.firmware.to_string_lossy().into_owned(),
                base.to_string(),
            ],
            FlashAction::Read => {
                let out = request.read_to.clone().unwrap_or_else(|| {
                    request.firmware.with_extension("readback.bin")
                });
                vec![
                    "read".to_string(),
                    out.to_string_lossy().into_owned(),
                    base.to_string(),
                ]
            }
        };
        let mut outcome = run_command(&self.program, &args, &request.firmware.parent().unwrap_or(Path::new(".")), None, request.cancel.as_deref(), on_line)?;
        outcome.tool = FlashTool::StLink;
        outcome.action = request.action;
        Ok(outcome)
    }
}

/// probe-rs flasher adapter: `probe-rs download <firmware>` / `probe-rs erase`.
pub struct ProbeRs {
    program: String,
}

impl ProbeRs {
    pub fn new(configured: &str) -> Self {
        Self {
            program: resolve_program(configured, &["probe-rs"]),
        }
    }
}

impl Flasher for ProbeRs {
    fn tool(&self) -> FlashTool {
        FlashTool::ProbeRs
    }

    fn run(&self, request: &FlashRequest, on_line: &mut dyn FnMut(&str)) -> Result<FlashOutcome> {
        // Use the project target as the chip, falling back to the historical
        // STM32F103C8 default when `project.toml` has no target.
        let chip = device_or(&request.device, "stm32f103c8");
        let args: Vec<String> = match request.action {
            FlashAction::Flash => vec![
                "download".to_string(),
                "--chip".to_string(),
                chip.clone(),
                request.firmware.to_string_lossy().into_owned(),
            ],
            FlashAction::Erase => vec![
                "erase".to_string(),
                "--chip".to_string(),
                chip.clone(),
            ],
            FlashAction::Verify => vec![
                "verify".to_string(),
                "--chip".to_string(),
                chip.clone(),
                request.firmware.to_string_lossy().into_owned(),
            ],
            FlashAction::Read => {
                let out = request.read_to.clone().unwrap_or_else(|| {
                    request.firmware.with_extension("readback.bin")
                });
                vec![
                    "dump".to_string(),
                    "--chip".to_string(),
                    chip.clone(),
                    out.to_string_lossy().into_owned(),
                ]
            }
        };
        let mut outcome = run_command(&self.program, &args, &request.firmware.parent().unwrap_or(Path::new(".")), None, request.cancel.as_deref(), on_line)?;
        outcome.tool = FlashTool::ProbeRs;
        outcome.action = request.action;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prove the full adapter flow end-to-end with a stub tool: a shell script
    /// that records its arguments and exits 0. The flash action must succeed
    /// and report success.
    #[test]
    fn openocd_adapter_flashes_via_stub_tool() {
        if cfg!(windows) {
            return; // stub uses /bin/sh.
        }
        let dir = std::env::temp_dir().join(format!("ns-flash-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Write a fake firmware (Intel HEX).
        let hex = ":020000040000FA\n\
                   :0400000001020304F2\n\
                   :00000001FF";
        let fw = dir.join("fw.hex");
        std::fs::write(&fw, hex).unwrap();

        // Write a stub flasher that records args to a file and exits 0.
        let log = dir.join("args.log");
        let stub = dir.join("stub-flasher");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let request = FlashRequest {
            tool: FlashTool::OpenOcd,
            action: FlashAction::Flash,
            firmware: fw.clone(),
            interface: "swd".to_string(),
            read_to: None,
            device: None,
             algorithm: None,
            cancel: None,
        };
        let flasher = OpenOcd::new(stub.to_str().unwrap());
        let outcome = flasher.run(&request, &mut |_| {}).unwrap();
        assert!(outcome.success, "flash should succeed: {}", outcome.combined_output());
        assert_eq!(outcome.tool, FlashTool::OpenOcd);
        assert_eq!(outcome.action, FlashAction::Flash);

        let recorded = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            recorded.contains("program") && recorded.contains("verify"),
            "stub should receive a program/verify command, got: {recorded}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Prove `adapter_for(project)` resolves from `project.toml` and the
    /// whole flash request flows through a stub tool successfully.
    #[test]
    fn adapter_for_project_flashes() {
        if cfg!(windows) {
            return;
        }
        let base = std::env::temp_dir().join(format!("ns-flash-proj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let mut project = ns_project::Project::create(ns_project::NewProjectSpec {
            name: "flashtest".into(),
            location: base.clone(),
            language: ns_project::Language::C,
            compiler: String::new(),
            simulator: String::new(),
            debugger: String::new(),
            flasher: String::new(),
            target: String::new(),
            svd: String::new(),
            c_template: None,
        })
        .unwrap();

        let log = base.join("args.log");
        let stub = base.join("stub-flasher");
        std::fs::write(
            &stub,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit 0\n", log.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        project.config.flasher.kind = "openocd".to_string();
        project.config.flasher.path = stub.to_string_lossy().into_owned();
        project.save_config().unwrap();

        // Simulate the built artifact.
        let artifact = project.artifact_path();
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        std::fs::write(
            &artifact,
            ":020000040000FA\n:0400000001020304F2\n:00000001FF\n",
        )
        .unwrap();

        let adapter = adapter_for(&project).unwrap();
        assert_eq!(adapter.tool(), FlashTool::OpenOcd);
        let request = FlashRequest {
            tool: adapter.tool(),
            action: FlashAction::Flash,
            firmware: artifact,
            interface: project.config.flasher.interface.clone(),
            read_to: None,
            device: None,
             algorithm: None,
            cancel: None,
        };
        let outcome = adapter.run(&request, &mut |_| {}).unwrap();
        assert!(outcome.success, "flash should succeed: {}", outcome.combined_output());

        let recorded = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            recorded.contains("program"),
            "stub should receive a program command, got: {recorded}"
        );

        let _ = std::fs::remove_dir_all(&base);
 }

/// The OpenOCD adapter resolves the project target to a matching target
/// config; unrecognised devices fall back to `stm32f1x.cfg`.
#[test]
fn openocd_target_cfg_maps_device_and_falls_back() {
    assert_eq!(openocd_target_cfg("stm32f103c8"), "target/stm32f1x.cfg");
    assert_eq!(openocd_target_cfg("stm32f407vg"), "target/stm32f4x.cfg");
    assert_eq!(openocd_target_cfg("nrf52840"), "target/nrf52.cfg");
    assert_eq!(openocd_target_cfg("rp2040"), "target/rp2040.cfg");
    assert_eq!(openocd_target_cfg("lpc2148"), "target/lpc2100.cfg");
    // Unknown / explicit device strings use the historical default.
    assert_eq!(openocd_target_cfg(""), "target/stm32f1x.cfg");
    assert_eq!(openocd_target_cfg("custom-mcu"), "target/stm32f1x.cfg");
}

/// `probe-rs` passes the project target as `--chip` (no hardcoded device),
/// and falls back to the historical STM32F103C8 when no target is set.
#[test]
fn probe_rs_uses_request_device_as_chip() {
    assert_eq!(device_or(&None, "stm32f103c8"), "stm32f103c8");
    assert_eq!(
        device_or(&Some("nrf52840".to_string()), "stm32f103c8"),
        "nrf52840"
    );
    // Empty / whitespace-only device falls back too.
    assert_eq!(
        device_or(&Some("  ".to_string()), "stm32f103c8"),
        "stm32f103c8"
    );
}

/// A pre-set cancel flag aborts the subprocess and reports failure rather
/// than a spurious success.
#[test]
fn flash_cancel_flag_aborts_and_reports_failure() {
    if cfg!(windows) {
        return; // stub uses /bin/sh.
    }
    let dir = std::env::temp_dir().join(format!("ns-flash-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let fw = dir.join("fw.hex");
    std::fs::write(
        &fw,
        ":020000040000FA\n:0400000001020304F2\n:00000001FF",
    )
    .unwrap();

    // A stub that sleeps long enough for the cancel check to observe the flag.
    let stub = dir.join("stub-slow");
    std::fs::write(&stub, "#!/bin/sh\nsleep 30\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let request = FlashRequest {
        tool: FlashTool::OpenOcd,
        action: FlashAction::Flash,
        firmware: fw.clone(),
        interface: "swd".to_string(),
        read_to: None,
        device: None,
        algorithm: None,
        cancel: Some(cancel),
    };
    let flasher = OpenOcd::new(stub.to_str().unwrap());
    let outcome = flasher.run(&request, &mut |_| {}).unwrap();
    assert!(!outcome.success, "cancelled flash must not report success");
    let _ = std::fs::remove_dir_all(&dir);
}
 }
