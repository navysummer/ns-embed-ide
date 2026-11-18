//! Virtual SysTick timer peripheral for ARM Cortex-M simulators.
//!
//! Implements the ARM System Timer (SysTick) which provides:
//! - Configurable reload value (period)
//! - Current counter value
//! - Control/status register with enable, interrupt, and clock source options
//! - Periodic interrupt generation
//!
//! Register map (memory-mapped):
//!
//! | Offset | Register | Access | Description                              |
//! |--------|----------|--------|------------------------------------------|
//! | `0x00` | STC_CALIB | RO     | Calibration value                        |
//! | `0x04` | STC_CNR   | RW     | Current value counter                    |
//! | `0x08` | STC_RVR   | RW     | Reload value                             |
//! | `0x0C` | STC_CVR   | RW     | Current value (read clears COUNTFLAG)    |
//! | `0x10` | STC_CSR   | RW     | Control and status register              |
//!
//! The CSR bits are:
//! - Bit 0: ENABLE - SysTick enable
//! - Bit 1: INTENA - Interrupt enable  
//! - Bit 2: CLKSRC - Clock source (0=external, 1=processor clock)
//! - Bit 3: COUNTFLAG - Count flag (set when counter reaches 0)

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// SysTick control and status register fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SysTickCsr {
    /// Enable bit (bit 0).
    pub enable: bool,
    /// Interrupt enable bit (bit 1).
    pub intena: bool,
    /// Clock source bit (bit 2).
    pub clksrc: bool,
}

impl Default for SysTickCsr {
    fn default() -> Self {
        Self {
            enable: false,
            intena: false,
            clksrc: true, // Default to processor clock
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
    /// Base address for memory mapping (typically 0xE000_E010 for ARM).
    pub base: u32,
    /// Control and status register.
    pub csr: SysTickCsr,
    /// Current counter value.
    pub cnr: u32,
    /// Reload value (counter counts down to 0).
    pub rvr: u32,
    /// Calibration value (read-only, fixed).
    pub calib: u32,
    /// Whether COUNTFLAG is set (cleared on read of CVR).
    count_flag: AtomicBool,
    /// Number of interrupts generated.
    pub interrupt_count: u64,
}

impl SysTickPeripheral {
    /// Create a new SysTick peripheral at the given base address.
    pub fn new(base: u32) -> Self {
        Self {
            base,
            csr: SysTickCsr::default(),
            cnr: 0,
            rvr: 0xFFFF_FFFF, // Full range by default
            calib: 0,
            count_flag: AtomicBool::new(false),
            interrupt_count: 0,
        }
    }

    /// Set the reload value (period).
    pub fn set_reload(&mut self, reload: u32) {
        self.rvr = reload;
        if self.cnr >= reload {
            self.cnr = reload - 1;
        }
    }

    /// Read a 32-bit register value at offset from base.
    pub fn read_reg(&self, offset: u32) -> Option<u32> {
        let val = match offset {
            0x00 => self.calib, // CALIB - read-only
            0x04 => self.cnr,   // CNTR - current value
            0x08 => self.rvr,   // RVR - reload value
            0x0C => {
                // CVR - reading clears COUNTFLAG
                let val = self.cnr;
                self.count_flag.store(false, Ordering::SeqCst);
                val
            }
            0x10 => self.csr.encode(), // CSR
            _ => return None,
        };
        Some(val)
    }

    /// Write a 32-bit register value at offset from base.
    pub fn write_reg(&mut self, offset: u32, value: u32) {
        match offset {
            0x04 => {
                // Write to CNTR clears the COUNTFLAG and sets counter to this value
                self.cnr = value;
                self.count_flag.store(false, Ordering::SeqCst);
            }
            0x08 => {
                // Write to RVR sets reload value
                self.rvr = value;
                if self.cnr > value {
                    self.cnr = value;
                }
            }
            0x10 => {
                // Write to CSR
                self.csr = SysTickCsr::from_bits(value);
                
                // Writing 1 to bit 16 clears COUNTFLAG
                if value & (1 << 16) != 0 {
                    self.count_flag.store(false, Ordering::SeqCst);
                }
                
                // Enable with non-zero reload starts counting
                if self.csr.enable && self.rvr > 0 {
                    self.cnr = self.rvr;
                }
            }
            _ => {}
        }
    }

    /// Advance the SysTick counter by one cycle.
    /// Returns true if an interrupt should be generated.
    pub fn tick(&mut self) -> bool {
        if !self.csr.enable || self.rvr == 0 {
            return false;
        }

        if self.cnr > 0 {
            self.cnr -= 1;
        }

        if self.cnr == 0 {
            // Counter reached zero - reload and generate interrupt
            self.cnr = self.rvr;
            self.count_flag.store(true, Ordering::SeqCst);
            
            if self.csr.intena {
                self.interrupt_count += 1;
                return true;
            }
        }

        false
    }

    /// Check if COUNTFLAG is set.
    pub fn has_count_flag(&self) -> bool {
        self.count_flag.load(Ordering::SeqCst)
    }

    /// Get current counter value.
    pub fn get_counter(&self) -> u32 {
        self.cnr
    }

    /// Get reload value.
    pub fn get_reload(&self) -> u32 {
        self.rvr
    }

    /// Check if a given address falls within this peripheral's register map.
    pub fn contains(&self, address: u32) -> bool {
        address >= self.base && address < self.base + 0x14
    }

    /// Get the register offset for a given address.
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
        assert_eq!(systick.cnr, 0);
        assert_eq!(systick.rvr, 0xFFFF_FFFF);
        assert_eq!(systick.interrupt_count, 0);
    }

    #[test]
    fn systick_enable_starts_counter() {
        let mut systick = SysTickPeripheral::new(0xE000_E010);
        systick.set_reload(100);
        systick.write_reg(0x10, 0x07); // Enable + interrupt + clock src
        
        assert!(systick.csr.enable);
        assert_eq!(systick.get_counter(), 100);
    }

    #[test]
    fn systick_tick_decrements_counter() {
        let mut systick = SysTickPeripheral::new(0xE000_E010);
        systick.set_reload(10);
        systick.write_reg(0x10, 0x07); // Enable
        
        // Tick 5 times
        for _ in 0..5 {
            systick.tick();
        }
        
        assert_eq!(systick.get_counter(), 5);
        assert!(!systick.has_count_flag());
    }

    #[test]
    fn systick_interrupt_on_zero() {
        let mut systick = SysTickPeripheral::new(0xE000_E010);
        systick.set_reload(3);
        systick.write_reg(0x10, 0x03); // Enable + interrupt
        
        // Tick 3 times to reach zero
        let interrupted = systick.tick(); // cnt=2
        assert!(!interrupted);
        
        let interrupted = systick.tick(); // cnt=1
        assert!(!interrupted);
        
        let interrupted = systick.tick(); // cnt=0, interrupt!
        assert!(interrupted);
        assert_eq!(systick.interrupt_count, 1);
        assert!(systick.has_count_flag());
    }

    #[test]
    fn systick_read_registers() {
        let systick = SysTickPeripheral::new(0xE000_E010);
        
        assert_eq!(systick.read_reg(0x00).unwrap(), 0); // CALIB
        assert_eq!(systick.read_reg(0x04).unwrap(), 0); // CNTR
        assert_eq!(systick.read_reg(0x08).unwrap(), 0xFFFF_FFFF); // RVR
        assert_eq!(systick.read_reg(0x10).unwrap(), 0); // CSR
    }

    #[test]
    fn systick_write_clears_countflag() {
        let mut systick = SysTickPeripheral::new(0xE000_E0010);
        systick.set_reload(5);
        systick.write_reg(0x10, 0x07);
        
        // Generate interrupt
        systick.tick();
        systick.tick();
        systick.tick();
        assert!(systick.has_count_flag());
        
        // Write to CNTR should clear flag
        systick.write_reg(0x04, 3);
        assert!(!systick.has_count_flag());
        assert_eq!(systick.get_counter(), 3);
    }

    #[test]
    fn systick_contains_range() {
        let systick = SysTickPeripheral::new(0xE000_E010);
        assert!(systick.contains(0xE000_E010));
        assert!(systick.contains(0xE000_E01C)); // CSR at offset 0x10
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
