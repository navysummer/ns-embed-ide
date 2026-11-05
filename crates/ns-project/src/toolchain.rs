use std::path::{Path, PathBuf};

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

    pub fn label(&self) -> String {
        match &self.path {
            Some(path) => path.display().to_string(),
            None => "未安装".to_string(),
        }
    }
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
    };
    tools(names)
}

pub fn debuggers(language: Language) -> Vec<Tool> {
    let names: &[&str] = match language {
        Language::C | Language::Cpp => &["arm-none-eabi-gdb", "gdb", "lldb"],
        Language::Rust => &["rust-gdb", "arm-none-eabi-gdb", "gdb", "lldb"],
        Language::Zig => &["gdb", "lldb"],
        Language::ZR => &["zr-dbg", "gdb"],
    };
    tools(names)
}

pub fn flashers(language: Language) -> Vec<Tool> {
    let names: &[&str] = match language {
        Language::C | Language::Cpp | Language::Rust | Language::Zig => {
            &["openocd", "pyocd", "JLinkExe", "probe-rs"]
        }
        Language::ZR => &["zr-flash", "openocd"],
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
}
