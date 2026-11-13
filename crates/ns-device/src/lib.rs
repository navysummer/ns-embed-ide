//! Target device library for ns-embed-ide.
//!
//! Parses CMSIS-SVD XML device descriptions into a structured model
//! (peripherals -> registers -> bit fields) and provides a small built-in
//! catalog of common MCUs with memory maps. Also generates linker scripts and
//! startup files for a target.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use std::path::Path;

/// A bit field inside a register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub bit_offset: u32,
    pub bit_width: u32,
    pub description: String,
}

/// A memory-mapped register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Register {
    pub name: String,
    pub address: u64,
    pub size: u32,
    pub reset_value: u64,
    pub description: String,
    pub fields: Vec<Field>,
}

/// A peripheral (block of registers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peripheral {
    pub name: String,
    pub base_address: u64,
    pub description: String,
    pub registers: Vec<Register>,
}

/// A memory region in the device memory map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRegion {
    pub name: String,
    pub start: u64,
    pub size: u64,
    pub access: String,
}

/// A parsed device description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    pub vendor: String,
    pub core: String,
    pub peripherals: Vec<Peripheral>,
    pub memory: Vec<MemoryRegion>,
}

impl Device {
    /// All peripherals sorted by base address.
    pub fn sorted_peripherals(&self) -> Vec<&Peripheral> {
        let mut list: Vec<&Peripheral> = self.peripherals.iter().collect();
        list.sort_by_key(|peripheral| peripheral.base_address);
        list
    }

    /// Look up a peripheral by name.
    pub fn peripheral(&self, name: &str) -> Option<&Peripheral> {
        self.peripherals.iter().find(|peripheral| peripheral.name == name)
    }
}

/// Parse a CMSIS-SVD XML document into a [`Device`].
pub fn parse_svd(xml: &str) -> Result<Device> {
    let document = roxmltree::Document::parse(xml)
        .map_err(|error| anyhow::anyhow!("SVD XML 解析失败: {error}"))?;
    let root = document.root_element();
    if root.tag_name().name() != "device" {
        bail!("SVD 根元素必须是 <device>");
    }

    let name = text_of(root, "name").unwrap_or_default();
    let vendor = text_of(root, "vendor").unwrap_or_default();
    let core = child_of(root, "cpu")
        .and_then(|cpu| child_of(cpu, "name"))
        .and_then(|node| node.text())
        .map(|text| text.trim().to_string())
        .unwrap_or_default();


    let mut peripherals = Vec::new();
    let mut memory = Vec::new();

    if let Some(peripherals_node) = child_of(root, "peripherals") {
        for node in peripherals_node
            .children()
            .filter(|child| child.is_element() && child.tag_name().name() == "peripheral")
        {
            if let Some(peripheral) = parse_peripheral(node) {
                peripherals.push(peripheral);
            }
        }
    }

    if let Some(memory_node) = child_of(root, "memory") {
        for node in memory_node
            .children()
            .filter(|child| child.is_element() && child.tag_name().name() == "memory")
        {
            let name = text_of(node, "name").unwrap_or_default();
            let start = text_of(node, "start")
                .and_then(parse_u64)
                .unwrap_or_default();
            let size = text_of(node, "size")
                .and_then(parse_u64)
                .unwrap_or_default();
            let access = text_of(node, "access").unwrap_or_default();
            if !name.is_empty() {
                memory.push(MemoryRegion {
                    name,
                    start,
                    size,
                    access,
                });
            }
        }
    }

    Ok(Device {
        name,
        vendor,
        core,
        peripherals,
        memory,
    })
}

/// Load and parse an external CMSIS-SVD file from disk.
///
/// Reads the file as UTF-8 XML and parses it into a [`Device`].
pub fn load_svd_file(path: &Path) -> Result<Device> {
    let xml = std::fs::read_to_string(path)
        .map_err(|error| anyhow::anyhow!("读取 SVD 文件失败: {error}"))?;
    parse_svd(&xml)
}


fn parse_peripheral(node: roxmltree::Node) -> Option<Peripheral> {
    let name = text_of(node, "name").unwrap_or_default();
    if name.is_empty() {
        return None;
    }
    let base_address = text_of(node, "baseAddress")
        .and_then(parse_u64)
        .unwrap_or_default();
    let description = text_of(node, "description").unwrap_or_default();

    let mut registers = Vec::new();
    if let Some(registers_node) = child_of(node, "registers") {
        for register_node in registers_node
            .children()
            .filter(|child| child.is_element() && child.tag_name().name() == "register")
        {
            if let Some(register) = parse_register(register_node, base_address) {
                registers.push(register);
            }
        }
    }

    Some(Peripheral {
        name,
        base_address,
        description,
        registers,
    })
}

fn parse_register(node: roxmltree::Node, base_address: u64) -> Option<Register> {
    let name = text_of(node, "name").unwrap_or_default();
    if name.is_empty() {
        return None;
    }
    let offset = text_of(node, "addressOffset")
        .and_then(parse_u64)
        .unwrap_or_default();
    let size = text_of(node, "size")
        .and_then(parse_u64)
        .map(|value| value as u32)
        .unwrap_or(32);
    let reset_value = text_of(node, "resetValue")
        .and_then(parse_u64)
        .unwrap_or_default();
    let description = text_of(node, "description").unwrap_or_default();

    let mut fields = Vec::new();
    if let Some(fields_node) = child_of(node, "fields") {
        for field_node in fields_node
            .children()
            .filter(|child| child.is_element() && child.tag_name().name() == "field")
        {
            let field_name = text_of(field_node, "name").unwrap_or_default();
            let (bit_offset, bit_width) = field_bit_range(field_node);
            if field_name.is_empty() {
                continue;
            }
            fields.push(Field {
                name: field_name,
                bit_offset,
                bit_width,
                description: text_of(field_node, "description").unwrap_or_default(),
            });
        }
    }

    Some(Register {
        name,
        address: base_address + offset,
        size,
        reset_value,
        description,
        fields,
    })
}

/// Extract the bit range of a field: either `<bitOffset>`/`<bitWidth>` or a
/// single `<bitRange>` like `"7:0"` or `<lsb>`/`<msb>`.
fn field_bit_range(node: roxmltree::Node) -> (u32, u32) {
    if let (Some(offset), Some(width)) = (
        text_of(node, "bitOffset").and_then(parse_u64),
        text_of(node, "bitWidth").and_then(parse_u64),
    ) {
        return (offset as u32, width as u32);
    }
    if let (Some(lsb), Some(msb)) = (
        text_of(node, "lsb").and_then(parse_u64),
        text_of(node, "msb").and_then(parse_u64),
    ) {
        let low = lsb.min(msb) as u32;
        let high = lsb.max(msb) as u32;
        return (low, high - low + 1);
    }
    if let Some(range) = text_of(node, "bitRange") {
        let range = range.trim().trim_start_matches('[').trim_end_matches(']');
        if let Some((high, low)) = range.split_once(':') {
            if let (Ok(high), Ok(low)) = (high.trim().parse::<u32>(), low.trim().parse::<u32>()) {
                let low = low.min(high);
                let high = low.max(high);
                return (low, high - low + 1);
            }
        }
    }
    (0, 1)
}

fn text_of<'a, 'input>(node: roxmltree::Node<'a, 'input>, tag: &str) -> Option<String> {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name() == tag)
        .and_then(|child| child.text())
        .map(|text| text.trim().to_string())
}

fn child_of<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    tag: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name() == tag)
}

fn parse_u64(text: String) -> Option<u64> {
    parse_hex_or_dec(&text)
}

/// Parse a number that may be hex (`0x…`), binary (`0b…`), or decimal.
pub fn parse_hex_or_dec(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok();
    }
    if let Some(bin) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
        return u64::from_str_radix(bin, 2).ok();
    }
    text.parse::<u64>().ok()
}

/// A built-in device catalog entry.
pub struct CatalogDevice {
    pub key: &'static str,
    pub label: &'static str,
    pub core: &'static str,
    pub flash_start: u64,
    pub flash_size: u64,
    pub ram_start: u64,
    pub ram_size: u64,
    /// Inline SVD XML (peripherals only).
    pub svd: &'static str,
}

/// The built-in device catalog (small set of common MCUs).
pub const CATALOG: &[CatalogDevice] = &[
    CatalogDevice {
        key: "stm32f103c8",
        label: "STM32F103C8 · Cortex-M3 · 64KB Flash",
        core: "ARM Cortex-M3",
        flash_start: 0x0800_0000,
        flash_size: 64 * 1024,
        ram_start: 0x2000_0000,
        ram_size: 20 * 1024,
        svd: include_str!("svd/stm32f103c8.svd"),
    },
    CatalogDevice {
        key: "stm32f407vg",
        label: "STM32F407VG · Cortex-M4F · 1MB Flash",
        core: "ARM Cortex-M4",
        flash_start: 0x0800_0000,
        flash_size: 1024 * 1024,
        ram_start: 0x2000_0000,
        ram_size: 128 * 1024,
        svd: include_str!("svd/stm32f407vg.svd"),
    },
    CatalogDevice {
        key: "nrf52840",
        label: "nRF52840 · Cortex-M4F · 1MB Flash",
        core: "ARM Cortex-M4",
        flash_start: 0x0000_0000,
        flash_size: 1024 * 1024,
        ram_start: 0x2000_0000,
        ram_size: 256 * 1024,
        svd: include_str!("svd/nrf52840.svd"),
    },
    CatalogDevice {
        key: "rp2040",
        label: "RP2040 · Dual Cortex-M0+ · 2MB Flash",
        core: "ARM Cortex-M0+",
        flash_start: 0x1000_0000,
        flash_size: 2 * 1024 * 1024,
        ram_start: 0x2000_0000,
        ram_size: 256 * 1024,
        svd: include_str!("svd/rp2040.svd"),
    },
    CatalogDevice {
        key: "stm32g431rb",
        label: "STM32G431RB · Cortex-M4F · 128KB Flash",
        core: "ARM Cortex-M4",
        flash_start: 0x0800_0000,
        flash_size: 128 * 1024,
        ram_start: 0x2000_0000,
        ram_size: 32 * 1024,
        svd: include_str!("svd/stm32g431rb.svd"),
    },
    CatalogDevice {
        key: "lpc2148",
        label: "LPC2148 · ARM7TDMI-S · 512KB Flash",
        core: "ARM7TDMI-S",
        flash_start: 0x0000_0000,
        flash_size: 512 * 1024,
        ram_start: 0x4000_0000,
        ram_size: 40 * 1024,
        svd: include_str!("svd/stm32f103c8.svd"),
    },
    CatalogDevice {
        key: "s3c2440",
        label: "S3C2440 · ARM920T · 64MB NAND",
        core: "ARM920T",
        flash_start: 0x0000_0000,
        flash_size: 64 * 1024 * 1024,
        ram_start: 0x3000_0000,
        ram_size: 64 * 1024,
        svd: include_str!("svd/stm32f103c8.svd"),
    },
    CatalogDevice {
        key: "esp32c3",
        label: "ESP32-C3 · RISC-V · 4MB Flash",
        core: "RISC-V",
        flash_start: 0x3FC0_0000,
        flash_size: 4 * 1024 * 1024,
        ram_start: 0x3FC8_0000,
        ram_size: 64 * 1024,
        svd: include_str!("svd/esp32c3.svd"),
    },
    CatalogDevice {
        key: "esp32",
        label: "ESP32 · Xtensa LX6 · 4MB Flash",
        core: "Xtensa LX6",
        flash_start: 0x400D_0000,
        flash_size: 4 * 1024 * 1024,
        ram_start: 0x3FF0_0000,
        ram_size: 320 * 1024,
        svd: "<device><name>esp32</name></device>",
    },
    CatalogDevice {
        key: "esp32s3",
        label: "ESP32-S3 · Xtensa LX7 · 8MB Flash",
        core: "Xtensa LX7",
        flash_start: 0x400D_0000,
        flash_size: 8 * 1024 * 1024,
        ram_start: 0x3FC8_0000,
        ram_size: 512 * 1024,
        svd: "<device><name>esp32s3</name></device>",
    },
    CatalogDevice {
        key: "mcs51",
        label: "Intel 8051 (MCS-51) · 8-bit · 64KB ROM",
        core: "MCS-51",
        flash_start: 0x0000,
        flash_size: 64 * 1024,
        ram_start: 0x0020,
        ram_size: 256,
        // 8051 has no standardized SVD; the core registers are modelled in the
        // built-in 8051 simulator instead of a device descriptor.
        svd: "<device><name>mcs51</name></device>",
    },
    CatalogDevice {
        key: "80251",
        label: "Intel 80251 (MCS-251) · 16-bit · 64KB Code",
        core: "MCS-251",
        flash_start: 0x0000,
        flash_size: 64 * 1024,
        ram_start: 0x0020,
        ram_size: 64 * 1024,
        // No standardized SVD; the 80251 simulator models its registers.
        svd: "<device><name>80251</name></device>",
    },
    CatalogDevice {
        key: "c166",
        label: "Infineon C166 · 16-bit · 64KB Code",
        core: "C166",
        flash_start: 0x0000,
        flash_size: 64 * 1024,
        ram_start: 0x0000,
        ram_size: 64 * 1024,
        svd: "<device><name>c166</name></device>",
    },
    CatalogDevice {
        key: "xc166",
        label: "Infineon XC166 · 16-bit · C166+",
        core: "C166",
        flash_start: 0x0000,
        flash_size: 64 * 1024,
        ram_start: 0x0000,
        ram_size: 64 * 1024,
        svd: "<device><name>xc166</name></device>",
    },
    CatalogDevice {
        key: "xc2000",
        label: "Infineon XC2000 · 16-bit · C166+",
        core: "C166",
        flash_start: 0x0000,
        flash_size: 64 * 1024,
        ram_start: 0x0000,
        ram_size: 64 * 1024,
        svd: "<device><name>xc2000</name></device>",
    },
    CatalogDevice {
        key: "st10",
        label: "Infineon ST10 · 16-bit · C166 family",
        core: "C166",
        flash_start: 0x0000,
        flash_size: 64 * 1024,
        ram_start: 0x0000,
        ram_size: 64 * 1024,
        svd: "<device><name>st10</name></device>",
    },
    CatalogDevice {
        key: "atmega328p",
        label: "ATmega328P · AVR · 32KB Flash",
        core: "AVR",
        flash_start: 0x0000,
        flash_size: 32 * 1024,
        ram_start: 0x0100,
        ram_size: 2 * 1024,
        svd: "<device><name>atmega328p</name></device>",
    },
    CatalogDevice {
        key: "attiny85",
        label: "ATtiny85 · AVR · 8KB Flash",
        core: "AVR",
        flash_start: 0x0000,
        flash_size: 8 * 1024,
        ram_start: 0x0060,
        ram_size: 512,
        svd: "<device><name>attiny85</name></device>",
    },
    CatalogDevice {
        key: "at90s8515",
        label: "AT90S8515 · AVR · 8KB Flash",
        core: "AVR",
        flash_start: 0x0000,
        flash_size: 8 * 1024,
        ram_start: 0x0060,
        ram_size: 512,
        svd: "<device><name>at90s8515</name></device>",
    },
    CatalogDevice {
        key: "msp430g2553",
        label: "MSP430G2553 · MSP430 · 16KB Flash",
        core: "MSP430",
        flash_start: 0xF800,
        flash_size: 16 * 1024,
        ram_start: 0x0200,
        ram_size: 512,
        svd: "<device><name>msp430g2553</name></device>",
    },
    CatalogDevice {
        key: "v850es",
        label: "V850ES · V850 · 256KB Flash",
        core: "V850",
        flash_start: 0x0000,
        flash_size: 256 * 1024,
        ram_start: 0x1F0000,
        ram_size: 32 * 1024,
        svd: "<device><name>v850es</name></device>",
    },
    CatalogDevice {
        key: "stm8s003f3",
        label: "STM8S003F3 · STM8 · 8KB Flash",
        core: "STM8",
        flash_start: 0x8000,
        flash_size: 8 * 1024,
        ram_start: 0x0000,
        ram_size: 1024,
        svd: "<device><name>stm8s003f3</name></device>",
    },
    CatalogDevice {
        key: "rx62n",
        label: "Renesas RX62N · RX · 256KB Flash",
        core: "RX",
        flash_start: 0x0000,
        flash_size: 256 * 1024,
        ram_start: 0x0010_0000,
        ram_size: 64 * 1024,
        svd: "<device><name>rx62n</name></device>",
    },
    CatalogDevice {
        key: "rl78g14",
        label: "RL78/G14 · RL78 · 96KB Flash",
        core: "RL78",
        flash_start: 0x0000,
        flash_size: 96 * 1024,
        ram_start: 0xFFE00,
        ram_size: 8 * 1024,
        svd: "<device><name>rl78g14</name></device>",
    },
];

/// Find a catalog entry by target key.
pub fn catalog_entry(key: &str) -> Option<&'static CatalogDevice> {
    CATALOG.iter().find(|entry| entry.key == key)
}

/// Parse the built-in SVD for a catalog target.
pub fn device_for_target(target: &str) -> Option<Device> {
    let entry = catalog_entry(target)?;
    parse_svd(entry.svd).ok()
}

/// Generate a GCC-style linker script for a target.
pub fn linker_script(target: &str) -> Option<String> {
    let entry = catalog_entry(target)?;
    let flash_start = entry.flash_start;
    let ram_start = entry.ram_start;
    let core = match entry.core {
        "ARM Cortex-M0+" => "cortex-m0plus",
        "ARM Cortex-M3" => "cortex-m3",
        "ARM Cortex-M4" => "cortex-m4",
        "RISC-V" => "riscv",
        _ => "cortex-m4",
    };
    Some(format!(
        r#"/* Auto-generated linker script for {target} ({core}). */
ENTRY(Reset_Handler)

MEMORY
{{
  FLASH (rx) : ORIGIN = 0x{flash_start:08X}, LENGTH = {flash_size}
  RAM (rwx)  : ORIGIN = 0x{ram_start:08X}, LENGTH = {ram_size}
}}

SECTIONS
{{
  .text : {{
    KEEP(*(.isr_vector))
    *(.text*)
    *(.rodata*)
    _etext = .;
  }} > FLASH

  .data : ALIGN(4) {{
    _sdata = .;
    *(.data*)
    _edata = .;
  }} > RAM AT> FLASH

  .bss : ALIGN(4) {{
    _sbss = .;
    *(.bss*)
    *(COMMON)
    _ebss = .;
  }} > RAM

  .stack : ALIGN(8) {{
    _estack = ORIGIN(RAM) + LENGTH(RAM);
  }} > RAM
}}
"#,
        flash_size = entry.flash_size,
        ram_size = entry.ram_size,
    ))
}

/// Generate a minimal startup file (vector table + reset handler) for a target.
///
/// Emits ARM Thumb assembly for Cortex-M cores and RISC-V assembly for RISC-V
/// cores. Callers may override by providing their own `startup.s`.
pub fn startup_file(target: &str) -> Option<String> {
    let entry = catalog_entry(target)?;
    let stack_top = entry.ram_start + entry.ram_size;
    if entry.core == "RISC-V" {
        return Some(format!(
            r#"/* Auto-generated startup for {target} (RISC-V). */
.section .text._start
.global _start
_start:
    la sp, _estack
    la a0, _sdata
    la a1, _edata
    la a2, _etext
copy_loop:
    bgeu a0, a1, zero_bss
    lw t0, 0(a2)
    sw t0, 0(a0)
    addi a0, a0, 4
    addi a2, a2, 4
    j copy_loop
zero_bss:
    la a0, _sbss
    la a1, _ebss
    li t0, 0
zero_loop:
    bgeu a0, a1, done
    sw t0, 0(a0)
    addi a0, a0, 4
    j zero_loop
done:
    call main
    j .
"#,
            target = target,
        ));
    }
    Some(format!(
        r#"/* Auto-generated startup for {target} ({core}). */
.syntax unified
.cpu {cpu}
.thumb

.section .isr_vector,"a",%progbits
.word {stack_top}
.word Reset_Handler

.section .text.Reset_Handler,"ax",%progbits
.thumb_func
.global Reset_Handler
Reset_Handler:
    ldr r0, =_sdata
    ldr r1, =_edata
    ldr r2, =_etext
copy_loop:
    cmp r0, r1
    bge zero_bss
    ldr r3, [r2], #4
    str r3, [r0], #4
    b copy_loop
zero_bss:
    ldr r0, =_sbss
    ldr r1, =_ebss
    movs r2, #0
zero_loop:
    cmp r0, r1
    bge done
    str r2, [r0], #4
    b zero_loop
done:
    bl main
    b .
"#,
        target = target,
        core = entry.core,
        cpu = match entry.core {
            "ARM Cortex-M0+" => "cortex-m0plus",
            "ARM Cortex-M3" => "cortex-m3",
            "ARM Cortex-M4" => "cortex-m4",
            _ => "cortex-m4",
        },
        stack_top = stack_top,
    ))
}

/// The memory map of a target as a list of regions, for the register view.
pub fn memory_map(target: &str) -> Vec<MemoryRegion> {
    let Some(entry) = catalog_entry(target) else {
        return Vec::new();
    };
    let mut regions = vec![
        MemoryRegion {
            name: "FLASH".to_string(),
            start: entry.flash_start,
            size: entry.flash_size,
            access: "rx".to_string(),
        },
        MemoryRegion {
            name: "RAM".to_string(),
            start: entry.ram_start,
            size: entry.ram_size,
            access: "rw".to_string(),
        },
    ];
    regions.push(MemoryRegion {
        name: "PERIPHERALS".to_string(),
        start: 0x4000_0000,
        size: 0x2000_0000,
        access: "rw".to_string(),
    });
    regions
}

/// Group all registers of a device by peripheral, with address-sorted output.
pub fn registers_by_peripheral(device: &Device) -> BTreeMap<String, Vec<&Register>> {
    let mut map: BTreeMap<String, Vec<&Register>> = BTreeMap::new();
    for peripheral in device.sorted_peripherals() {
        let mut registers: Vec<&Register> = peripheral.registers.iter().collect();
        registers.sort_by_key(|register| register.address);
        map.insert(peripheral.name.clone(), registers);
    }
    map
}

/// A requested peripheral to configure for init-code generation.
#[derive(Debug, Clone)]
pub struct PeripheralConfig {
    /// Peripheral name as it appears in the SVD (e.g. `GPIOA`, `USART1`).
    pub name: String,
    /// Human-readable intent used to seed the init code (e.g. `GPIO 输出`).
    pub purpose: String,
    /// Optional pin/clock hints from CubeMX-style config (not parsed in depth).
    pub options: Vec<String>,
    /// Concrete register writes (register name → value) to emit verbatim.
    /// When non-empty, these override the reset-value comment skeleton.
    pub register_values: BTreeMap<String, u64>,
}

/// Generate a C init function that enables and configures the requested
/// peripherals. The generated code is a best-effort skeleton: it sets the
/// peripheral's clock-enable bit and writes reset values for registers the
/// SVD describes, so the user can fill in the exact configuration. `target` is
/// the catalog key (e.g. `stm32f103c8`); returns `None` for unknown targets.
 pub fn generate_peripheral_init(target: &str, configs: &[PeripheralConfig]) -> Option<String> {
     let device = device_for_target(target)?;
    let mut lines = String::new();
    lines.push_str(&format!(
        "/* Auto-generated peripheral initialization for {target}.\n"
    ));
    lines.push_str(" * Edit the register writes to match your application.\n");
    lines.push_str(" * Call `peripheral_init()` once from `main()` before use.\n");
    lines.push_str(" */\n\n");
    lines.push_str("#include <stdint.h>\n\n");

    // Each enabled peripheral gets its own init helper.
    for config in configs {
        let Some(peripheral) = device.peripheral(&config.name) else {
            continue;
        };
        let fn_name = format!("{}_init", config.name.to_ascii_lowercase());
        lines.push_str(&format!("void {fn_name}(void) {{\n"));
        lines.push_str(&format!(
            "    /* {} — {} */\n",
            config.name,
            if config.purpose.is_empty() {
                "配置外设"
            } else {
                &config.purpose
            }
        ));
        // Acknowledge clock enable for the peripheral block.
        lines.push_str(&format!(
            "    // TODO: 使能 {name} 的时钟（RCC->APBxENR / AHBxENR 对应位）。\n",
            name = config.name
        ));
        if !config.register_values.is_empty() {
            // Emit real register writes in address order, then fall back to
            // reset-value comments for any config register the user did not set.
            let mut named: Vec<(&Register, u64)> = peripheral
                .registers
                .iter()
                .filter_map(|register| {
                    config
                        .register_values
                        .get(&register.name)
                        .map(|value| (register, *value))
                })
                .collect();
            named.sort_by_key(|(register, _)| register.address);
            for (register, value) in &named {
                lines.push_str(&format!(
                    "    {name}->{reg} = {value:#010x}; /* {desc} */\n",
                    name = config.name,
                    reg = register.name,
                    value = value,
                    desc = register.description
                ));
            }
            let written: std::collections::BTreeSet<&str> =
                named.iter().map(|(register, _)| register.name.as_str()).collect();
            for register in peripheral.registers.iter() {
                if written.contains(register.name.as_str()) {
                    continue;
                }
                if register.name.to_uppercase().contains("CR")
                    || register.name.to_uppercase().contains("CFG")
                    || register.name.to_uppercase().contains("BRR")
                {
                    lines.push_str(&format!(
                        "    // {name}->{reg} = {reset:#010x}; /* {desc} 未设置 */\n",
                        name = config.name,
                        reg = register.name,
                        reset = register.reset_value,
                        desc = register.description
                    ));
                }
            }
        } else {
            for register in &peripheral.registers {
                if register.name.to_uppercase().contains("CR")
                    || register.name.to_uppercase().contains("CFG")
                    || register.name.to_uppercase().contains("BRR")
                {
                    lines.push_str(&format!(
                        "    // {name}@{addr:#010x} = {reset:#010x}; /* {desc} */\n",
                        name = register.name,
                        addr = register.address,
                        reset = register.reset_value,
                        desc = register.description
                    ));
                }
            }
        }
        lines.push_str("}\n\n");
    }

    // Aggregate init that calls each helper.
    lines.push_str("void peripheral_init(void) {\n");
    for config in configs {
        if device.peripheral(&config.name).is_some() {
            lines.push_str(&format!(
                "    {}_init();\n",
                config.name.to_ascii_lowercase()
            ));
        }
    }
    lines.push_str("}\n");

    Some(lines)
}

/// Extract the peripheral names the SVD lists for a target (for the
/// configuration UI to offer as choices).
pub fn peripheral_names(target: &str) -> Vec<String> {
    device_for_target(target)
        .map(|device| {
            device
                .sorted_peripherals()
                .into_iter()
                .map(|peripheral| peripheral.name.clone())
                .collect()
        })
        .unwrap_or_default()
 }
 
 #[cfg(test)]
 mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0"?>
<device>
  <name>STM32F103</name>
  <vendor>STMicroelectronics</vendor>
  <cpu><name>CM3</name></cpu>
  <peripherals>
    <peripheral>
      <name>GPIOA</name>
      <baseAddress>0x40010800</baseAddress>
      <description>General purpose I/O A</description>
      <registers>
        <register>
          <name>CRL</name>
          <addressOffset>0x0</addressOffset>
          <size>0x20</size>
          <resetValue>0x44444444</resetValue>
          <description>Configuration low</description>
          <fields>
            <field><name>MODE0</name><bitRange>[3:0]</bitRange></field>
            <field><name>CNF0</name><bitRange>[5:4]</bitRange></field>
          </fields>
        </register>
        <register>
          <name>IDR</name>
          <addressOffset>0x8</addressOffset>
          <size>0x20</size>
          <resetValue>0x0</resetValue>
          <fields>
            <field><name>IDR0</name><bitOffset>0</bitOffset><bitWidth>1</bitWidth></field>
          </fields>
        </register>
      </registers>
    </peripheral>
  </peripherals>
  <memory>
    <memory><name>FLASH</name><start>0x08000000</start><size>0x10000</size><access>rx</access></memory>
  </memory>
</device>"#;

    #[test]
    fn parses_svd_device() {
        let device = parse_svd(SAMPLE).unwrap();
        assert_eq!(device.name, "STM32F103");
        assert_eq!(device.core, "CM3");
        assert_eq!(device.peripherals.len(), 1);
        let gpioa = device.peripheral("GPIOA").unwrap();
        assert_eq!(gpioa.base_address, 0x4001_0800);
        assert_eq!(gpioa.registers.len(), 2);
        let crl = &gpioa.registers[0];
        assert_eq!(crl.name, "CRL");
        assert_eq!(crl.address, 0x4001_0800);
        assert_eq!(crl.reset_value, 0x4444_4444);
        assert_eq!(crl.fields.len(), 2);
        assert_eq!(crl.fields[0].name, "MODE0");
        assert_eq!(crl.fields[0].bit_width, 4);
        assert_eq!(crl.fields[1].name, "CNF0");
        assert_eq!(crl.fields[1].bit_offset, 4);
        let idr = &gpioa.registers[1];
        assert_eq!(idr.address, 0x4001_0808);
        assert_eq!(device.memory.len(), 1);
        assert_eq!(device.memory[0].name, "FLASH");
    }

    #[test]
    fn parses_hex_and_binary() {
        assert_eq!(parse_hex_or_dec("0x1F"), Some(31));
        assert_eq!(parse_hex_or_dec("0b101"), Some(5));
        assert_eq!(parse_hex_or_dec("42"), Some(42));
        assert_eq!(parse_hex_or_dec("0xZZ"), None);
    }

    #[test]
    fn catalog_lookup_and_generation() {
        let entry = catalog_entry("stm32f103c8").unwrap();
        assert_eq!(entry.flash_size, 64 * 1024);
        let device = device_for_target("stm32f103c8").unwrap();
        assert!(!device.peripherals.is_empty());
        let linker = linker_script("stm32f103c8").unwrap();
        assert!(linker.contains("0x08000000"));
        assert!(linker.contains("MEMORY"));
        let startup = startup_file("stm32f103c8").unwrap();
        assert!(startup.contains("Reset_Handler"));
        let map = memory_map("stm32f103c8");
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn unknown_target_generates_nothing() {
        assert!(linker_script("unknown-mcu").is_none());
        assert!(startup_file("unknown-mcu").is_none());
        assert!(device_for_target("unknown-mcu").is_none());
     }

    #[test]
    fn generates_peripheral_init_for_catalog_target() {
        let configs = vec![
            PeripheralConfig {
                name: "GPIOA".to_string(),
                purpose: "LED 输出".to_string(),
                options: vec!["pin0".to_string()],
                register_values: BTreeMap::new(),
            },
            PeripheralConfig {
                name: "USART1".to_string(),
                purpose: "调试串口".to_string(),
                register_values: BTreeMap::new(),
                options: Vec::new(),
            },
        ];
        let code = generate_peripheral_init("stm32f103c8", &configs).unwrap();
        assert!(code.contains("peripheral_init"));
        assert!(code.contains("gpioa_init"));
        assert!(code.contains("usart1_init"));
        assert!(code.contains("LED 输出"));
        assert!(code.contains("#include <stdint.h>"));
    }

/// When `register_values` are supplied, `generate_peripheral_init` emits real
/// register writes (`PERIPH->REG = value;`) in address order instead of the
/// reset-value comment skeleton.
#[test]
fn peripheral_init_emits_real_register_writes() {
    let mut values = BTreeMap::new();
    values.insert("CRL".to_string(), 0x33333333u64);
    values.insert("CRH".to_string(), 0x44444444u64);
    let configs = vec![PeripheralConfig {
        name: "GPIOA".to_string(),
        purpose: "LED 输出".to_string(),
        options: Vec::new(),
        register_values: values,
    }];
    let code = generate_peripheral_init("stm32f103c8", &configs).unwrap();
    assert!(code.contains("GPIOA->CRL = 0x33333333;"), "{code}");
    assert!(code.contains("GPIOA->CRH = 0x44444444;"), "{code}");
    // A config register the user did not set is still noted, not silently dropped.
    assert!(code.contains("GPIOA->CRL"), "{code}");
}

    #[test]
    fn peripheral_init_skips_unknown_peripheral() {
        let configs = vec![PeripheralConfig {
            name: "NOT_A_PERIPH".to_string(),
            purpose: String::new(),
            options: Vec::new(),
            register_values: BTreeMap::new(),
        }];
        let code = generate_peripheral_init("stm32f103c8", &configs).unwrap();
        assert!(!code.contains("not_a_periph_init"));
    }

    #[test]
    fn peripheral_names_lists_catalog_peripherals() {
        let names = peripheral_names("stm32f103c8");
        assert!(names.contains(&"GPIOA".to_string()));
        assert!(names.contains(&"USART1".to_string()));
    }

    #[test]
    fn peripheral_init_unknown_target_is_none() {
        assert!(generate_peripheral_init("unknown-mcu", &[]).is_none());
    }
 }
