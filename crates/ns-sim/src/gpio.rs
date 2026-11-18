//! Enhanced GPIO peripheral model supporting multiple ports.
//!
//! Implements a multi-port GPIO controller similar to STM32:
//! - Multiple ports (GPIOA-GPIOE, configurable)
//! - Each port has independent configuration registers
//! - Support for input/output modes, pull-up/pull-down
//! - Event generation on pin changes
//! - External interrupt capability

use serde::Serialize;

/// GPIO direction mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum GpioMode {
    Input,
    Output,
    AlternateFunction,
}

impl Default for GpioMode {
    fn default() -> Self {
        Self::Input
    }
}

/// GPIO output type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum GpioOutputType {
    PushPull,
    OpenDrain,
}

impl Default for GpioOutputType {
    fn default() -> Self {
        Self::PushPull
    }
}

/// GPIO speed setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum GpioSpeed {
    Low,
    Medium,
    High,
    VeryHigh,
}

impl Default for GpioSpeed {
    fn default() -> Self {
        Self::Low
    }
}

/// Pull resistor configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum GpioPull {
    None,
    Up,
    Down,
}

impl Default for GpioPull {
    fn default() -> Self {
        Self::None
    }
}

/// Configuration for a single GPIO pin.
#[derive(Debug, Clone, Serialize)]
pub struct GpioPinConfig {
    pub mode: GpioMode,
    pub output_type: GpioOutputType,
    pub speed: GpioSpeed,
    pub pull: GpioPull,
    pub af_number: u8, // Alternate function number (for AF mode)
}

impl Default for GpioPinConfig {
    fn default() -> Self {
        Self {
            mode: GpioMode::Input,
            output_type: GpioOutputType::PushPull,
            speed: GpioSpeed::Low,
            pull: GpioPull::None,
            af_number: 0,
        }
    }
}

/// A single GPIO port (e.g., GPIOA, GPIOB).
#[derive(Debug, Clone, Serialize)]
pub struct GpioPort {
    /// Port letter (A, B, C, etc.).
    pub port_letter: char,
    /// Base address offset (used for register mapping).
    pub base_offset: u32,
    /// Current output value (32 bits, one per pin).
    pub odr: u32,
    /// Input data register.
    pub idr: u32,
    /// Configuration for each pin (16 pins per port typically).
    pub configs: Vec<GpioPinConfig>,
    /// Events generated on this port.
    pub events: Vec<GpioEvent>,
}

/// GPIO event structure.
#[derive(Debug, Clone, Serialize)]
pub struct GpioEvent {
    pub port: char,
    pub pin: u8,
    pub value: bool,
    pub edge: GpioEdge,
}

/// Edge detection for interrupts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum GpioEdge {
    Rising,
    Falling,
    Both,
}

impl Default for GpioEdge {
    fn default() -> Self {
        Self::Both
    }
}

/// Multi-port GPIO controller.
#[derive(Debug, Clone, Serialize)]
pub struct GpioController {
    pub base: u32,
    pub ports: Vec<GpioPort>,
    /// Global output enable register (per port group).
    pub enr: u32,
}

impl GpioController {
    pub fn new(base: u32, num_ports: usize, pins_per_port: usize) -> Self {
        let mut ports = Vec::new();
        for i in 0..num_ports {
            let port_letter = std::char::from_u32(('A' as u32) + i as u32).unwrap_or('A');
            ports.push(GpioPort {
                port_letter,
                base_offset: (i * 0x400) as u32,
                odr: 0,
                idr: 0,
                configs: vec![GpioPinConfig::default(); pins_per_port],
                events: Vec::new(),
            });
        }
        Self {
            base,
            ports,
            enr: 0,
        }
    }

    pub fn get_port(&self, index: usize) -> Option<&GpioPort> {
        self.ports.get(index)
    }

    pub fn get_port_mut(&mut self, index: usize) -> Option<&mut GpioPort> {
        self.ports.get_mut(index)
    }

    pub fn set_pin_output(&mut self, port_idx: usize, pin: u8, value: bool) -> Result<(), &'static str> {
        if let Some(port) = self.ports.get_mut(port_idx) {
            if pin < port.configs.len() as u8 {
                let bit = 1u32 << pin;
                if value {
                    port.odr |= bit;
                } else {
                    port.odr &= !bit;
                }
                Ok(())
            } else {
                Err("Pin out of range")
            }
        } else {
            Err("Invalid port")
        }
    }

    pub fn read_pin_input(&self, port_idx: usize, pin: u8) -> Option<bool> {
        self.ports.get(port_idx).map(|port| {
            if pin < port.configs.len() as u8 {
                (port.idr >> pin) & 1 == 1
            } else {
                false
            }
        })
    }

    pub fn contains(&self, address: u32) -> bool {
        for port in &self.ports {
            let port_base = self.base + port.base_offset;
            if address >= port_base && address < port_base + 0x400 {
                return true;
            }
        }
        false
    }

    pub fn offset(&self, address: u32) -> Option<(usize, u32)> {
        for (i, port) in self.ports.iter().enumerate() {
            let port_base = self.base + port.base_offset;
            if address >= port_base && address < port_base + 0x400 {
                return Some((i, address - port_base));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpio_controller_new() {
        let gpio = GpioController::new(0x4002_0000, 5, 16);
        assert_eq!(gpio.ports.len(), 5);
        assert_eq!(gpio.ports[0].port_letter, 'A');
        assert_eq!(gpio.ports[4].port_letter, 'E');
    }

    #[test]
    fn gpio_set_output() {
        let mut gpio = GpioController::new(0x4002_0000, 1, 16);
        gpio.set_pin_output(0, 0, true).unwrap();
        assert_eq!(gpio.ports[0].odr, 1);
        gpio.set_pin_output(0, 0, false).unwrap();
        assert_eq!(gpio.ports[0].odr, 0);
    }

    #[test]
    fn gpio_read_input() {
        let mut gpio = GpioController::new(0x4002_0000, 1, 16);
        gpio.ports[0].idr = 0xFF;
        assert_eq!(gpio.read_pin_input(0, 0), Some(true));
        assert_eq!(gpio.read_pin_input(0, 7), Some(true));
        assert_eq!(gpio.read_pin_input(0, 8), Some(false));
    }

    #[test]
    fn gpio_contains_range() {
        let gpio = GpioController::new(0x4002_0000, 2, 16);
        let porta_base = gpio.base + 0x000;
        let portb_base = gpio.base + 0x400;
        
        assert!(gpio.contains(porta_base));
        assert!(gpio.contains(porta_base + 0x3FC));
        assert!(!gpio.contains(porta_base + 0x400)); // Should be port B
        assert!(gpio.contains(portb_base));
    }
}

#[cfg(test)]
mod irq_tests {
    use super::*;

    #[test]
    fn gpio_controller_irq_line() {
        let mut gpio = GpioController::new(0x4002_0000, 1, 16);
        assert_eq!(gpio.get_irq_line(), -1); // default
        
        gpio.set_irq_line(5);
        assert_eq!(gpio.get_irq_line(), 5);
    }
}

/// GPIO event history entry for visualization.
#[derive(Debug, Clone, Serialize)]
pub struct GpioHistoryEntry {
    /// Timestamp (cycle count).
    pub cycle: u64,
    /// Pin number.
    pub pin: u8,
    /// New state (true = high, false = low).
    pub state: bool,
}

impl From<&GpioEvent> for GpioHistoryEntry {
    fn from(event: &GpioEvent) -> Self {
        Self {
            cycle: event.cycle,
            pin: event.pin,
            state: event.value != 0,
        }
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;

    #[test]
    fn gpio_history_entry_from_event() {
        let event = GpioEvent {
            cycle: 100,
            pin: 5,
            value: 1,
            pc: 0x2000,
        };
        let entry = GpioHistoryEntry::from(&event);
        assert_eq!(entry.cycle, 100);
        assert_eq!(entry.pin, 5);
        assert!(entry.state);
    }
}
