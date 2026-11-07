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
}

/// A selectable C starter template for a new project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CTemplate {
    /// Default: an empty `while(1)` loop.
    Empty,
    /// LED Blink: toggles a GPIO pin in a busy loop.
    LedBlink,
    /// UART Loopback: echoes received bytes back over the UART.
    UartLoopback,
}

impl CTemplate {
    pub const ALL: &'static [CTemplate] = &[
        CTemplate::Empty,
        CTemplate::LedBlink,
        CTemplate::UartLoopback,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CTemplate::Empty => "空模板（while(1) 循环）",
            CTemplate::LedBlink => "LED Blink（GPIO 翻转）",
            CTemplate::UartLoopback => "UART 回环（收发回显）",
        }
    }

    pub fn source(self) -> &'static str {
        match self {
            CTemplate::Empty => r#"#include <stdint.h>

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
            CTemplate::LedBlink => r#"#include <stdint.h>

/*
 * ns-embed-ide LED Blink 模板
 * 通过 GPIO 端口输出寄存器翻转一个引脚，形成闪烁。
 *
 * 引脚与端口按目标设备调整：
 *   - STM32: 将下方 GPIOB_ODR 改为实际端口（如 GPIOA_ODR）。
 *   - nRF/RP2040/ESP32: 改用对应 SVD 的 GPIO 寄存器。
 */

#define GPIOB_ODR  (*(volatile uint32_t *)0x40010C0CUL) // STM32F1 GPIOB 输出数据

static void delay(volatile uint32_t ticks) {
    while (ticks--) {
        __asm__ volatile ("nop");
    }
}

int main(void) {
    while (1) {
        GPIOB_ODR ^= (1u << 13);  // 翻转 PB13（板载 LED）
        delay(500000);
    }
}
"#,
            CTemplate::UartLoopback => r#"#include <stdint.h>

/*
 * ns-embed-ide UART 回环模板
 * 轮询 UART 状态寄存器，读到字节后原样写回发送寄存器。
 *
 * 寄存器地址按目标设备调整；以下为 STM32F1 USART1 的示例。
 */

#define USART1_SR   (*(volatile uint32_t *)0x40013800UL)  // 状态
#define USART1_DR   (*(volatile uint32_t *)0x40013804UL)  // 数据
#define USART1_CR1  (*(volatile uint32_t *)0x4001380CUL)  // 控制1

#define SR_RXNE   (1u << 5)
#define SR_TXE    (1u << 7)
#define CR1_UE    (1u << 13)
#define CR1_TE    (1u << 3)
#define CR1_RE    (1u << 2)

int main(void) {
    USART1_CR1 = CR1_UE | CR1_TE | CR1_RE;  // 使能 UART、发送、接收

    while (1) {
        if (USART1_SR & SR_RXNE) {
            uint8_t byte = (uint8_t)(USART1_DR & 0xFFu);
            while (!(USART1_SR & SR_TXE)) { /* 等待发送就绪 */ }
            USART1_DR = byte;  // 回环发送
        }
    }
}
"#,
        }
    }
}

impl Language {
    /// The C starter template source for a new project, selected by kind.
    pub fn starter_source_for_template(
        self,
        template: Option<CTemplate>,
    ) -> (&'static str, &'static str) {
        if self == Language::C {
            let template = template.unwrap_or(CTemplate::Empty);
            ("src/main.c", template.source())
        } else {
            self.starter_source()
        }
    }
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
