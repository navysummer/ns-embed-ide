//! Virtual SysTick timer peripheral for ARM Cortex-M simulators.

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};

/// SysTick control and status register fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SysTickCsr {
    pub enable: bool,
    pub intena: bool,
    pub clksrc: bool,
}

impl Default for SysTickCsr {
    fn default() -> Self {
        Self {
            enable: false,
            intena: false,
            clksrc: false,
        }
    }
}

impl SysTickCsr {
    pub fn encode(&self) -> u32 {
        let mut reg = 0u32;
        if self.enable { reg |= 1 << 0; }
        if self.intena { reg |= 1 << 1; }
        if self.clksrc { reg |= 1 << 2; }
        reg
    }

    pub fn from_bits(bits: u32) -> Self {
        Self {
            enable: (bits & 1) != 0,
            intena: (bits >> 1 & 1) != 0,
            clksrc: (bits >> 2 & 1) != 0,
        }
    }
}

/// SysTick peripheral state.
#[derive(Debug, Serialize)]
pub struct SysTickPeripheral {
    pub base: u32,
    pub csr: SysTickCsr,
    pub cnr: u32,
    pub rvr: u32,
    pub calib: u32,
    count_flag: AtomicBool,
    pub interrupt_count: u64,
}

impl SysTickPeripheral {
    pub fn new(base: u32) -> Self {
        Self {
            base,
            csr: SysTickCsr::default(),
            cnr: 0,
            rvr: 0xFFFF_FFFF,
            calib: 0,
            count_flag: AtomicBool::new(false),
            interrupt_count: 0,
        }
    }

    pub fn set_reload(&mut self, reload: u32) {
        self.rvr = reload;
        if self.cnr >= reload {
            self.cnr = reload - 1;
        }
    }

    /// Read a 32-bit register value at offset from base.
    pub fn read_reg(&self, offset: u32) -> Option<u32> {
        let val = match offset {
            0x00 => self.calib,
            0x04 => self.cnr,
            0x08 => self.rvr,
            0x0C => {
                // CVR: reading clears COUNTFLAG
                let val = self.cnr;
                self.count_flag.store(false, Ordering::SeqCst);
                val
            }
            0x10 => self.csr.encode(),
            _ => return None,
        };
        Some(val)
    }

    /// Write a 32-bit register value at offset from base.
    pub fn write_reg(&mut self, offset: u32, value: u32) {
        match offset {
            0x04 => {
                // VR: writing any value clears COUNTFLAG and loads this into counter
                self.cnr = value;
                self.count_flag.store(false, Ordering::SeqCst);
            }
            0x08 => {
                // AR: auto-reload value
                self.rvr = value;
                if self.cnr > value {
                    self.cnr = value;
                }
            }
            0x10 => {
                // CSRs: control and status register
                self.csr = SysTickCsr::from_bits(value);
                // Clear COUNTFLAG by writing 1 to bit 16
                if value & (1 << 16) != 0 {
                    self.count_flag.store(false, Ordering::SeqCst);
                }
                // Enable timer
                if self.csr.enable && self.rvr > 0 {
                    self.cnr = self.rvr;
                }
            }
            _ => {}
        }
    }

    /// Advance by one cycle. Returns true if an interrupt should fire.
    pub fn tick(&mut self) -> bool {
        if !self.csr.enable || self.rvr == 0 {
            return false;
        }

        // Decrement counter
        self.cnr = self.cnr.saturating_sub(1);
        
        // When counter reaches 0, reload and signal interrupt
        if self.cnr == 0 {
            self.count_flag.store(true, Ordering::SeqCst);
            self.cnr = self.rvr; // reload
            
            if self.csr.intena {
                self.interrupt_count += 1;
                return true;
            }
        }

        false
    }

    pub fn has_count_flag(&self) -> bool {
        self.count_flag.load(Ordering::SeqCst)
    }

    pub fn get_counter(&self) -> u32 {
        self.cnr
    }

    pub fn contains(&self, address: u32) -> bool {
        address >= self.base && address < self.base.wrapping_add(0x10)
    }

    pub fn offset(&self, address: u32) -> u32 {
        address.wrapping_sub(self.base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systick_new_has_default_state() {
        let systick = SysTickPeripheral::new(0xE000_E010);
        assert!(!systick.csr.enable);
        assert!(!systick.csr.intena);
        assert_eq!(systick.get_counter(), 0);
        assert_eq!(systick.rvr, 0xFFFF_FFFF);
    }

    #[test]
    fn systick_enable_starts_counter() {
        let mut systick = SysTickPeripheral::new(0xE000_E010);
        systick.set_reload(100);
        systick.write_reg(0x10, 0x07); // CR enabled;
        assert!(systick.csr.enable);
        assert_eq!(systick.get_counter(), 100);
    }

    #[test]
    fn systick_tick_decrements() {
        let mut systick = SysTickPeripheral::new(0xE000_E010);
        systick.set_reload(10);
        systick.write_reg(0x10, 0x07); // CR enabled;
        for _ in 0..5 {
            systick.tick();
        }
        assert_eq!(systick.get_counter(), 5);
    }

    #[test]
    fn systick_interrupt_on_zero() {
        let mut systick = SysTickPeripheral::new(0xE000_E010);
        systick.set_reload(3);
        systick.write_reg(0x10, 0x03); // CR with intena;
        
        let irq = systick.tick(); assert_eq!(irq, false);
        let irq = systick.tick(); assert_eq!(irq, false);
        let irq = systick.tick(); assert!(irq);
        assert_eq!(systick.interrupt_count, 1);
        assert!(systick.has_count_flag());
    }

    #[test]
    fn systick_read_registers() {
        let systick = SysTickPeripheral::new(0xE000_E010);
        assert_eq!(systick.read_reg(0x00).unwrap(), 0);
        assert_eq!(systick.read_reg(0x04).unwrap(), 0);
        assert_eq!(systick.read_reg(0x08).unwrap(), 0xFFFF_FFFF);
        assert_eq!(systick.read_reg(0x10).unwrap(), 0);
    }

    #[test]
    fn systick_write_clears_flag() {
        let mut systick = SysTickPeripheral::new(0xE000_E010);
        systick.set_reload(5);
        systick.write_reg(0x10, 0x07); // enable + intena + clksrc
        
        // Tick 6 times: 5->4->3->2->1->0 (interrupt fires on transition 0->5)
        systick.tick(); // cnt=4
        systick.tick(); // cnt=3
        systick.tick(); // cnt=2
        systick.tick(); // cnt=1
        systick.tick(); // cnt=0
        systick.tick(); // cnt wraps to 5, interrupt fires
        
        assert!(systick.has_count_flag());
        
        // Writing to VR clears COUNTFLAG
        systick.write_reg(0x04, 3);
        assert!(!systick.has_count_flag());
        assert_eq!(systick.get_counter(), 3);
    }

    #[test]
    fn systick_contains_range() {
        let systick = SysTickPeripheral::new(0xE000_E010);
        assert!(systick.contains(0xE000_E010));
        assert!(systick.contains(0xE000_E018)); // CVR at offset 0x0C
        assert!(systick.contains(0xE000_E01C)); // CALIB at offset 0x0C (valid)
        assert!(!systick.contains(0xE000_E01D));
        assert!(!systick.contains(0xE000_E00F));
    }

    #[test]
    fn systick_offset_calculation() {
        let systick = SysTickPeripheral::new(0xE000_E010);
        assert_eq!(systick.offset(0xE000_E010), 0);
        assert_eq!(systick.offset(0xE000_E014), 4);
        assert_eq!(systick.offset(0xE000_E01C), 0x0C);
    }
}
