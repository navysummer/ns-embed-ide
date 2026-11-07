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
        key: "esp32c3",
        label: "ESP32-C3 · RISC-V · 4MB Flash",
        core: "RISC-V",
        flash_start: 0x3FC0_0000,
        flash_size: 4 * 1024 * 1024,
        ram_start: 0x3FC8_0000,
        ram_size: 64 * 1024,
        svd: include_str!("svd/esp32c3.svd"),
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
}
