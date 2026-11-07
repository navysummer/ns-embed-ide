//! ZR standard library (`zr-std`) foundation.
//!
//! Describes the built-in hardware-abstraction API that the ZR compiler
//! (`zrc`) and `zr-lsp` expose to embedded programs. These are declarations
//! only: the actual device register mappings are target-specific and resolved
//! by the linker/startup code generated from the target's SVD.
//!
//! The types here are intentionally small and mirror the memory-mapped
//! register patterns of the virtual peripherals in `ns-sim`, so ZR programs
//! can be simulated and flashed consistently.

use std::collections::BTreeMap;

/// A memory-mapped GPIO port.
///
/// Register offsets follow the `ns-sim` virtual GPIO layout:
/// `OUT` at +0x00, `OUT_SET` at +0x04, `OUT_CLR` at +0x08, `DIR` at +0x0C.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpioPort {
    /// Base address of the port's register block.
    pub base: u32,
}

impl GpioPort {
    /// Read the current output latch (bit per pin).
    pub const fn out(self) -> u32 {
        0
    }
    /// Set bits (1 = drive high).
    pub const fn set(self, _mask: u32) {}
    /// Clear bits (1 = drive low).
    pub const fn clear(self, _mask: u32) {}
    /// Configure the direction mask (1 = output).
    pub const fn set_dir(self, _mask: u32) {}
}

/// A memory-mapped UART peripheral.
///
/// Register offsets follow the `ns-sim` virtual UART layout: data at +0x00,
/// status at +0x04 (RXNE bit 5, TXE bit 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Uart {
    pub base: u32,
}

impl Uart {
    /// Write a single byte to the transmit register (blocking poll on TXE).
    pub const fn write_byte(self, _byte: u8) {}
    /// Read a single byte from the receive register (assumes RXNE set).
    pub const fn read_byte(self) -> u8 {
        0
    }
    /// Whether a received byte is available (RXNE).
    pub const fn rx_ready(self) -> bool {
        false
    }
    /// Whether the transmit register is empty (TXE).
    pub const fn tx_ready(self) -> bool {
        true
    }
    /// Blocking `write_byte` for each byte in `bytes`.
    pub const fn write(self, bytes: &[u8]) {
        let _ = bytes;
    }
}

/// A memory-mapped general-purpose timer.
///
/// Register offsets follow the `ns-sim` virtual timer layout: `CR` at +0x00
/// (bit 0 = enable), `PSC` at +0x04, `CNT` at +0x08, `ARR` at +0x0C, `SR` at
/// +0x10 (bit 0 = update interrupt flag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timer {
    pub base: u32,
}

impl Timer {
    /// Enable the timer (starts counting).
    pub const fn start(self) {}
    /// Disable the timer.
    pub const fn stop(self) {}
    /// Read the current counter value.
    pub const fn counter(self) -> u32 {
        0
    }
    /// Set the prescaler (0 = divide by 1).
    pub const fn set_prescaler(self, _prescaler: u32) {}
    /// Set the auto-reload value (counter wraps at this value).
    pub const fn set_auto_reload(self, _reload: u32) {}
    /// Clear the update interrupt flag in `SR`.
    pub const fn clear_update(self) {}
}

/// Describes a single peripheral for the ZR stdlib registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeripheralDef {
    pub name: &'static str,
    pub base: u32,
    pub kind: PeripheralKind,
}

/// The kind of a ZR stdlib peripheral.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeripheralKind {
    Gpio,
    Uart,
    Timer,
}

/// The canonical `zr-std` peripheral registry.
///
/// Maps a peripheral name to its memory-mapped base address and kind. This is
/// the authoritative list the ZR compiler uses to resolve `@gpio("GPIOA")`-style
/// bindings and the LSP uses for completion/validation.
pub fn std_registry() -> BTreeMap<&'static str, PeripheralDef> {
    let mut map = BTreeMap::new();
    for def in [
        PeripheralDef { name: "GPIOA", base: 0x4800_0000, kind: PeripheralKind::Gpio },
        PeripheralDef { name: "GPIOB", base: 0x4800_0400, kind: PeripheralKind::Gpio },
        PeripheralDef { name: "GPIOC", base: 0x4800_0800, kind: PeripheralKind::Gpio },
        PeripheralDef { name: "USART1", base: 0x4001_3800, kind: PeripheralKind::Uart },
        PeripheralDef { name: "USART2", base: 0x4000_4400, kind: PeripheralKind::Uart },
        PeripheralDef { name: "TIM2", base: 0x4000_0000, kind: PeripheralKind::Timer },
        PeripheralDef { name: "TIM3", base: 0x4000_0400, kind: PeripheralKind::Timer },
    ] {
        map.insert(def.name, def);
    }
    map
}

/// Look up a peripheral definition by name.
pub fn peripheral(name: &str) -> Option<&'static PeripheralDef> {
    static REGISTRY: std::sync::LazyLock<BTreeMap<&'static str, PeripheralDef>> =
        std::sync::LazyLock::new(std_registry);
    REGISTRY.get(name)
}

/// The built-in ZR stdlib API surface as a list of `(name, signature)` entries,
/// suitable for the LSP's completion and signature-help features.
pub fn std_api() -> Vec<(&'static str, &'static str)> {
    vec![
        ("gpio.set", "fn gpio.set(port: GpioPort, mask: u32)"),
        ("gpio.clear", "fn gpio.clear(port: GpioPort, mask: u32)"),
        ("gpio.set_dir", "fn gpio.set_dir(port: GpioPort, mask: u32)"),
        ("uart.write_byte", "fn uart.write_byte(uart: Uart, byte: u8)"),
        ("uart.read_byte", "fn uart.read_byte(uart: Uart) -> u8"),
        ("uart.rx_ready", "fn uart.rx_ready(uart: Uart) -> bool"),
        ("uart.write", "fn uart.write(uart: Uart, bytes: &[u8])"),
        ("timer.start", "fn timer.start(t: Timer)"),
        ("timer.stop", "fn timer.stop(t: Timer)"),
        ("timer.counter", "fn timer.counter(t: Timer) -> u32"),
        ("timer.set_prescaler", "fn timer.set_prescaler(t: Timer, prescaler: u32)"),
        ("timer.set_auto_reload", "fn timer.set_auto_reload(t: Timer, reload: u32)"),
        ("delay.cycles", "fn delay.cycles(n: u32)"),
    ]
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_common_peripherals() {
        let registry = std_registry();
        assert!(registry.contains_key("GPIOA"));
        assert!(registry.contains_key("USART1"));
        assert!(registry.contains_key("TIM2"));
        assert_eq!(registry["GPIOA"].kind, PeripheralKind::Gpio);
        assert_eq!(registry["USART1"].kind, PeripheralKind::Uart);
    }

    #[test]
    fn std_api_is_non_empty_and_well_formed() {
        let api = std_api();
        assert!(!api.is_empty());
        for (name, signature) in &api {
            assert!(name.contains('.'));
            assert!(signature.starts_with("fn "));
        }
    }
}
