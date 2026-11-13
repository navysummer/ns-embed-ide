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
    /// Assembly source (`startup.s`, `.S`, `.asm`). Never a project main
    /// language (excluded from [`Language::ALL`]); used for editor highlighting
    /// of startup files, interrupt handlers and linker scripts (`.ld`).
    #[serde(rename = "Asm", alias = "asm", alias = "assembly")]
    Asm,
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
            // Not a project language; only reached through `from_path`.
            Language::Asm => "Asm",
        }
    }

    pub fn default_compilers(self) -> &'static [&'static str] {
        match self {
            Language::C => &["arm-none-eabi-gcc", "gcc", "clang", "zig"],
            Language::Cpp => &["arm-none-eabi-g++", "g++", "clang++", "zig"],
            Language::Rust => &["cargo", "rustc"],
            Language::Zig => &["zig"],
            Language::ZR => &["zr"],
            Language::Asm => &["arm-none-eabi-gcc", "gcc"],
        }
    }

    pub fn default_std(self) -> &'static str {
        match self {
            Language::C => "c11",
            Language::Cpp => "c++17",
            Language::Rust => "2021",
            Language::Zig => "",
            Language::ZR => "",
            Language::Asm => "",
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
            // Assembly / linker script: startup files, interrupt vectors,
            // and the memory-map script produced by the device library.
            "s" | "S" | "asm" | "inc" | "ld" => Some(Language::Asm),
            _ => None,
        }
     }

    /// The line-comment prefix for this language (`//` for C/C++/Rust/ZR,
    /// `///`-style is not needed; Zig uses `//` too). Assembly and unknown
    /// languages return `None` so callers can fall back to a no-op.
    pub fn line_comment(self) -> Option<&'static str> {
        match self {
            Language::C | Language::Cpp | Language::Rust | Language::Zig | Language::ZR => {
                Some("//")
            }
            Language::Asm => None,
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
    /// FreeRTOS-style: a small cooperative task scheduler with two tasks.
    /// Uses a self-contained scheduler so the project builds without an
    /// external kernel download (swap in the real FreeRTOS kernel to extend).
    Freertos,
}

impl CTemplate {
    pub const ALL: &'static [CTemplate] = &[
        CTemplate::Empty,
        CTemplate::LedBlink,
        CTemplate::UartLoopback,
        CTemplate::Freertos,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CTemplate::Empty => "空模板（while(1) 循环）",
            CTemplate::LedBlink => "LED Blink（GPIO 翻转）",
            CTemplate::UartLoopback => "UART 回环（收发回显）",
            CTemplate::Freertos => "FreeRTOS（两任务协作调度）",
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
}"#,
            CTemplate::Freertos => r#"#include <stdint.h>

/*
 * ns-embed-ide FreeRTOS 风格模板
 * 一个自包含的协作式任务调度器 + 两个任务，无需外部内核下载即可构建。
 * 要换成真正的 FreeRTOS 内核：把 Scheduler 换成 FreeRTOS 的 vTaskStartScheduler，
 * 任务函数改为 xTaskCreate 注册的入口（每个任务一个 while(1) 循环）即可。
 */

#define MAX_TASKS     4

typedef struct {
    const char *name;
    unsigned priority;      /* 0=低，数字越大越优先 */
    unsigned stack_usage;   /* 0-100，相对高水位估算 */
    unsigned state;         /* 0=就绪,1=阻塞,2=挂起 */
} Task;

static Task tasks[MAX_TASKS];
static volatile int task_count = 0;
static volatile int current_task = 0;
static volatile uint32_t sys_ticks = 0;

/* 每个任务跑一个时间片，返回后调度器轮转到下一个。 */
static void task_led(void) {
    volatile uint32_t *gpio_odr = (volatile uint32_t *)0x40010C0CUL; // STM32F1 GPIOB
    *gpio_odr ^= (1u << 13);   /* 时间片内翻转一次 */
}

static void task_monitor(void) {
    static volatile uint32_t counter = 0;
    counter++;                 /* 模拟监控/统计任务 */
}

static void scheduler_add(const char *name) {
    if (task_count >= MAX_TASKS) return;
    tasks[task_count].name = name;
    task_count++;
}

int main(void) {
    scheduler_add("led");
    scheduler_add("monitor");
    /* 每个任务的可观测属性（调试器经 RTOS 视图读取）。 */
    tasks[0].priority = 2; tasks[0].stack_usage = 35; tasks[0].state = 0;
    tasks[1].priority = 1; tasks[1].stack_usage = 60; tasks[1].state = 0;

    for (;;) {
        current_task = (current_task + 1) % task_count;
        sys_ticks++;
        /* 标记运行任务为就绪，其余保持；这里简单地把运行任务置 0。 */
        for (int i = 0; i < task_count; i++) {
            tasks[i].state = (i == current_task) ? 0 : 0;
        }
        if (current_task == 0) {
            task_led();
        } else {
            task_monitor();
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
// zr 语言（https://zrlang.github.io/）。编译器为 `zr`，
// 裸机目标用 `--target <triple>` 直接产出可烧录 ELF。

fn main() -> i32 {
    let mut counter: u32 = 0;

    while true {
        counter += 1;
    }

    return 0;
}
"#,
            ),
            // Assembly starter: a minimal ARM Cortex-M reset handler.
            Language::Asm => (
                "src/startup.s",
                r#"/* ARM Cortex-M startup (Thumb). */
.syntax unified
.cpu cortex-m3
.thumb

.section .isr_vector,"a",%progbits
.global g_pfnVectors
g_pfnVectors:
    .word _estack
    .word Reset_Handler

.section .text.Reset_Handler,"ax",%progbits
.thumb_func
.global Reset_Handler
Reset_Handler:
    ldr r0, =_estack
    mov sp, r0
    bl main
    b .
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
            // C/C++ projects get a CMakeLists.txt so `[build] system = "auto"`
            // resolves to a CMake build. `file(GLOB ...)` picks up every source
            // in `src/`; the wizard's single-file template builds as-is, and
            // added files are picked up on the next configure.
            Language::C | Language::Cpp => Some((
                "CMakeLists.txt",
                r#"cmake_minimum_required(VERSION 3.16)
project(ns-app C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)

file(GLOB SOURCES CONFIGURE_DEPENDS "${CMAKE_SOURCE_DIR}/src/*.c"
     "${CMAKE_SOURCE_DIR}/src/*.S")

add_executable(ns-app ${SOURCES})

# Forward project.toml defines/includes from the IDE (cache variables set by
# the CMake build plan).
if(DEFINED NS_IDE_DEFINES)
    target_compile_definitions(ns-app PRIVATE ${NS_IDE_DEFINES})
endif()
if(DEFINED NS_IDE_INCLUDES)
    target_include_directories(ns-app PRIVATE ${NS_IDE_INCLUDES})
endif()

set_target_properties(ns-app PROPERTIES
    SUFFIX ".elf")
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
            "asm" | "s" | "assembly" => Ok(Language::Asm),
            other => Err(format!("未知语言: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_path_recognizes_assembly_and_linker_scripts() {
        assert_eq!(Language::from_path(Path::new("src/startup.s")), Some(Language::Asm));
        assert_eq!(Language::from_path(Path::new("src/startup.S")), Some(Language::Asm));
        assert_eq!(Language::from_path(Path::new("boot.asm")), Some(Language::Asm));
        assert_eq!(Language::from_path(Path::new("link.ld")), Some(Language::Asm));
        assert_eq!(Language::from_path(Path::new("stm32.inc")), Some(Language::Asm));
        assert_eq!(Language::from_path(Path::new("main.c")), Some(Language::C));
        assert_eq!(Language::from_path(Path::new("main.rs")), Some(Language::Rust));
        assert_eq!(Language::from_path(Path::new("notes.txt")), None);
    }

    #[test]
    fn asm_is_not_a_project_language() {
        // Assembly is for highlighting startup/linker files, not a selectable
        // project main language (wizard/`Language::ALL`).
        assert!(!Language::ALL.contains(&Language::Asm));
    }
}
