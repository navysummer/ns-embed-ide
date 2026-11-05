use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Languages understood by the IDE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Language {
    #[serde(rename = "C", alias = "c")]
    C,
    #[serde(rename = "C++", alias = "cpp", alias = "c++")]
    Cpp,
    #[serde(rename = "Rust", alias = "rust")]
    Rust,
    #[serde(rename = "Zig", alias = "zig")]
    Zig,
    #[serde(rename = "ZR", alias = "zr")]
    ZR,
}

impl Language {
    pub const ALL: [Language; 5] = [
        Language::C,
        Language::Cpp,
        Language::Rust,
        Language::Zig,
        Language::ZR,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Language::C => "C",
            Language::Cpp => "C++",
            Language::Rust => "Rust",
            Language::Zig => "Zig",
            Language::ZR => "ZR",
        }
    }

    pub fn default_compilers(self) -> &'static [&'static str] {
        match self {
            Language::C => &["arm-none-eabi-gcc", "gcc", "clang", "zig"],
            Language::Cpp => &["arm-none-eabi-g++", "g++", "clang++", "zig"],
            Language::Rust => &["cargo", "rustc"],
            Language::Zig => &["zig"],
            Language::ZR => &["zrc"],
        }
    }

    pub fn default_std(self) -> &'static str {
        match self {
            Language::C => "c11",
            Language::Cpp => "c++17",
            Language::Rust => "2021",
            Language::Zig => "",
            Language::ZR => "",
        }
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "c" | "h" => Some(Language::C),
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Some(Language::Cpp),
            "rs" => Some(Language::Rust),
            "zig" => Some(Language::Zig),
            "zr" => Some(Language::ZR),
            _ => None,
        }
    }

    /// Relative path + contents of the starter source file for a new project.
    pub fn starter_source(self) -> (&'static str, &'static str) {
        match self {
            Language::C => (
                "src/main.c",
                r#"#include <stdint.h>

/*
 * ns-embed-ide 项目模板
 * 从这里开始编写你的嵌入式程序。
 */

int main(void) {
    volatile uint32_t counter = 0;

    while (1) {
        counter++;
    }
}
"#,
            ),
            Language::Cpp => (
                "src/main.cpp",
                r#"#include <cstdint>

/*
 * ns-embed-ide 项目模板
 * 从这里开始编写你的嵌入式程序。
 */

int main() {
    volatile std::uint32_t counter = 0;

    while (true) {
        counter++;
    }
}
"#,
            ),
            Language::Rust => (
                "src/main.rs",
                r#"//! ns-embed-ide Rust 项目模板
//!
//! 嵌入式目标通常需要 `#![no_std]` 与链接脚本支持，
//! 可在熟悉工具链后替换本文件。

fn main() {
    let mut counter: u32 = 0;

    loop {
        counter = counter.wrapping_add(1);
    }
}
"#,
            ),
            Language::Zig => (
                "src/main.zig",
                r#"const std = @import("std");

// ns-embed-ide Zig 项目模板
pub fn main() void {
    var counter: u32 = 0;

    while (true) {
        counter +%= 1;
    }
}
"#,
            ),
            Language::ZR => (
                "src/main.zr",
                r#"// ns-embed-ide ZR 项目模板
// ZR 是面向嵌入式场景的自研语言，编译器为 zrc。

fn main() -> void {
    let counter: u32 = 0;

    loop {
        counter = counter + 1;
    }
}
"#,
            ),
        }
    }

    pub fn starter_build_file(self) -> Option<(&'static str, &'static str)> {
        match self {
            Language::Rust => Some((
                "Cargo.toml",
                r#"[package]
name = "ns-app"
version = "0.1.0"
edition = "2021"

[dependencies]
"#,
            )),
            Language::Zig => Some((
                "build.zig",
                r#"const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    const exe = b.addExecutable(.{
        .name = "ns-app",
        .root_source_file = b.path("src/main.zig"),
        .target = target,
        .optimize = optimize,
    });

    b.installArtifact(exe);
}
"#,
            )),
            _ => None,
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Language {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "c" => Ok(Language::C),
            "c++" | "cpp" | "cxx" => Ok(Language::Cpp),
            "rust" | "rs" => Ok(Language::Rust),
            "zig" => Ok(Language::Zig),
            "zr" => Ok(Language::ZR),
            other => Err(format!("未知语言: {other}")),
        }
    }
}
