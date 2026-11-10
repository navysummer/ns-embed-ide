use std::path::{Path, PathBuf};
use std::process::Command;

use crate::language::Language;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    pub path: Option<PathBuf>,
}

impl Tool {
    pub fn available(&self) -> bool {
        self.path.is_some()
    }

    /// First non-empty line of `tool --version` (or the first line of the
    /// tool's help when `--version` is not supported). Used to show the
    /// detected version in the wizard/toolchain status.
    pub fn version(&self) -> Option<String> {
        let path = self.path.as_ref()?;
        for flag in ["--version", "-version", "-V"] {
            if let Ok(output) = Command::new(path).arg(flag).output() {
                if output.status.success() {
                    let text = String::from_utf8_lossy(&output.stdout);
                    let line = text
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .map(str::trim)
                        .unwrap_or("");
                    if !line.is_empty() {
                        return Some(line.to_string());
                    }
                }
            }
        }
        None
    }

    pub fn label(&self) -> String {
        match &self.path {
            Some(path) => path.display().to_string(),
            None => "未安装".to_string(),
        }
    }
}

/// Installation hint for a tool that is not on PATH. Returns the suggested
/// package/install command per tool family, or `None` for unknown tools.
pub fn missing_guide(name: &str) -> Option<&'static str> {
    let known: &[(&str, &str)] = &[
        ("arm-none-eabi-gcc", "brew install arm-none-eabi-gcc (或下载 ARM GNU Toolchain)"),
        ("arm-none-eabi-gdb", "brew install arm-none-eabi-gdb (或下载 ARM GNU Toolchain)"),
        ("clang", "brew install llvm (或 Xcode Command Line Tools)"),
        ("gcc", "brew install gcc"),
        ("rustc", "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"),
        ("cargo", "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"),
        ("zig", "brew install zig (或下载 ziglang.org)"),
        ("lldb", "xcode-select --install (Xcode Command Line Tools)"),
        ("gdb", "brew install gdb"),
        ("openocd", "brew install open-ocd"),
        ("pyocd", "pip install pyocd"),
        ("JLinkExe", "下载 SEGGER J-Link 安装包"),
        ("probe-rs", "cargo install probe-rs-tools"),
        // Keil/IAR 交叉工具链（各架构裸机编译器）。
        ("avr-gcc", "brew tap osx-cross/avr && brew install avr-gcc (或下载 Microchip AVR GCC)"),
        ("avr-g++", "brew tap osx-cross/avr && brew install avr-gcc (或下载 Microchip AVR GCC)"),
        ("msp430-elf-gcc", "下载 TI MSP430 GCC 工具链 (ti.com/msp430-gcc)"),
        ("msp430-elf-gdb", "下载 TI MSP430 GCC 工具链 (ti.com/msp430-gcc)"),
        ("v850-elf-gcc", "下载 Renesas V850 GCC 工具链 (renesas.com)"),
        ("rx-elf-gcc", "下载 Renesas RX GCC 工具链 (renesas.com)"),
        ("rl78-elf-gcc", "下载 Renesas RL78 GCC 工具链 (renesas.com)"),
        ("sdcc", "brew install sdcc (SDCC 支持 8051/80251/STM8 目标)"),
        ("qemu-system-arm", "brew install qemu"),
        ("qemu-system-riscv32", "brew install qemu"),
    ];
    known
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, guide)| *guide)
}

pub fn find_command(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 {
        return path.is_file().then(|| path.to_path_buf());
    }

    let path_var = std::env::var_os("PATH")?;
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".to_string())
            .split(';')
            .map(|ext| ext.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };

    for directory in std::env::split_paths(&path_var) {
        for extension in &extensions {
            let candidate = if extension.is_empty() {
                directory.join(name)
            } else {
                directory.join(format!("{name}{extension}"))
            };
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub fn tools(names: &[&str]) -> Vec<Tool> {
    names
        .iter()
        .map(|name| Tool {
            name: (*name).to_string(),
            path: find_command(name),
        })
        .collect()
}

pub fn compilers(language: Language) -> Vec<Tool> {
    tools(language.default_compilers())
}

pub fn simulators(language: Language) -> Vec<Tool> {
    let names: &[&str] = match language {
        Language::C | Language::Cpp => &["ns-sim", "qemu-system-arm", "qemu-system-riscv32"],
        Language::Rust => &["ns-sim", "qemu-system-arm"],
        Language::Zig => &["ns-sim", "qemu-system-arm"],
        Language::ZR => &["ns-sim"],
        Language::Asm => &["ns-sim", "qemu-system-arm"],
    };
    tools(names)
}

pub fn debuggers(language: Language) -> Vec<Tool> {
    let names: &[&str] = match language {
        Language::C | Language::Cpp => &["arm-none-eabi-gdb", "gdb", "lldb"],
        Language::Rust => &["rust-gdb", "arm-none-eabi-gdb", "gdb", "lldb"],
        Language::Zig => &["gdb", "lldb"],
        Language::ZR => &["gdb"],
        Language::Asm => &["arm-none-eabi-gdb", "gdb", "lldb"],
    };
    tools(names)
}

pub fn flashers(language: Language) -> Vec<Tool> {
    let names: &[&str] = match language {
        Language::C | Language::Cpp | Language::Rust | Language::Zig => {
            &["openocd", "pyocd", "JLinkExe", "probe-rs"]
        }
        Language::ZR => &["openocd"],
        Language::Asm => &["openocd", "pyocd", "JLinkExe"],
    };
    tools(names)
}


pub fn first_available(tools: &[Tool]) -> Option<&Tool> {
    tools.iter().find(|tool| tool.available())
}

pub fn preferred_or_first(tools: &[Tool]) -> Option<String> {
    first_available(tools)
        .or_else(|| tools.first())
        .map(|tool| tool.name.clone())
}

pub const TARGETS: &[(&str, &str)] = &[
    ("stm32f103c8", "STM32F103C8 · Cortex-M3 · 64KB Flash"),
    ("stm32f407vg", "STM32F407VG · Cortex-M4F · 1MB Flash"),
    ("stm32g431rb", "STM32G431RB · Cortex-M4F · 128KB Flash"),
    ("nrf52840", "nRF52840 · Cortex-M4F · 1MB Flash"),
    ("rp2040", "RP2040 · Dual Cortex-M0+ · 2MB Flash"),
    ("esp32c3", "ESP32-C3 · RISC-V · 4MB Flash"),
    ("custom", "自定义目标（稍后在 project.toml 中填写）"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_sh_which_always_exists_on_unix() {
        if cfg!(unix) {
            assert!(find_command("sh").is_some());
        }
    }

    #[test]
    fn preferred_tool_prefers_installed() {
        let list = tools(&["definitely-not-a-real-tool-xyz", "sh"]);
        assert_eq!(preferred_or_first(&list).as_deref(), Some("sh"));
    }
    #[test]
    fn version_reports_first_line_for_installed_tool() {
        // `sh` always exists on Unix; its `--version` (or `-V`) may not, so
        // only assert the probe returns a value or None without panicking.
        if let Some(path) = find_command("sh") {
            let tool = Tool {
                name: "sh".to_string(),
                path: Some(path),
            };
            let _ = tool.version();
        }
    }

    #[test]
    fn missing_tool_reports_none_for_unknown() {
        assert_eq!(missing_guide("sh"), None);
    }

    #[test]
    fn missing_guide_covers_known_tools() {
        assert!(missing_guide("openocd").is_some());
        assert!(missing_guide("arm-none-eabi-gcc").is_some());
        assert!(missing_guide("rustc").is_some());
        // Keil/IAR cross toolchains for the simulated architectures.
        assert!(missing_guide("avr-gcc").is_some());
        assert!(missing_guide("msp430-elf-gcc").is_some());
        assert!(missing_guide("v850-elf-gcc").is_some());
        assert!(missing_guide("rx-elf-gcc").is_some());
        assert!(missing_guide("rl78-elf-gcc").is_some());
        assert!(missing_guide("sdcc").is_some());
    }

    #[test]
    fn absent_tool_has_no_version() {
        let tool = Tool {
            name: "definitely-not-real-xyz".to_string(),
            path: None,
        };
        assert_eq!(tool.version(), None);
    }
}
