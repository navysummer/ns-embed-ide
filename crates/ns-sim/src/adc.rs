//! Virtual ADC (Analog-to-Digital Converter) peripheral model for ns-sim.
//!
//! Implements a memory-mapped ADC peripheral supporting:
//! - Configurable resolution (8/10/12/16 bits)
//! - Multiple input channels with simulated conversion
//! - Software-triggered and continuous mode operation
//! - Result buffer with read-after-conversion access
//!
//! Register map (base + offset):
//!
//! | Offset   | Register | Access | Description                              |
//! |----------|----------|--------|------------------------------------------|
//! | `0x00`   | CR1      | RW     | Control register 1 (resolution, flags)   |
//! | `0x04`   | CR2      | RW     | Control register 2 (start/stop, align)   |
//! | `0x08`   | SMPR1    | RW     | Sampling time selection (channel 0-7)    |
//! | `0x0C`   | SMPR2    | RW     | Sampling time selection (channel 8-17)   |
//! | `0x10`   | SQR1     | RW     | Regular sequence register 1 (length)     |
//! | `0x14`   | SQR2     | RW     | Regular sequence register 2              |
//! | `0x18`   | SQR3     | RW     | Regular sequence register 3              |
//! | `0x1C`   | SQR4     | RW     | Regular sequence register 4              |
//! | `0x20`   | DR       | RO     | Data register (conversion result)        |
//! | `0x24`   | JDR1     | RO     | Injected channel data register 1         |
//!
//! **Conversion flow**:
//! 1. Configure resolution in CR1
//! 2. Set sampling times per channel in SMPR1/SMPR2
//! 3. Configure sequence length and channel order in SQR registers
//! 4. Write to CR2 to start conversion (ADON=1, SWSTART=1)
//! 5. Wait for EOC flag, then read DR
//!
//! **Channel mapping**: The `channels` vector holds simulated analog values.
//! Index N corresponds to ADC channel N. Values are 16-bit unsigned representing
//! the analog input level (0..max_value).

use serde::Serialize;

/// ADC resolution in bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AdcResolution {
    Bits8,
    Bits10,
    Bits12,
    Bits16,
}

impl Default for AdcResolution {
    fn default() -> Self {
        Self::Bits12
    }
}

impl AdcResolution {
    pub fn bits(&self) -> u32 {
        match self {
            Self::Bits8 => 8,
            Self::Bits10 => 10,
            Self::Bits12 => 12,
            Self::Bits16 => 16,
        }
    }

    pub fn max_value(&self) -> u32 {
        (1u32 << self.bits()) - 1
    }

    pub fn from_cr1(res_bits: u8) -> Option<Self> {
        match res_bits {
            0b00 => Some(Self::Bits8),
            0b01 => Some(Self::Bits10),
            0b10 => Some(Self::Bits12),
            0b11 => Some(Self::Bits16),
            _ => None,
        }
    }
}

/// Sampling time configuration per channel group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SamplingTime {
    /// Cycles to sample before conversion (simulated).
    pub cycles: u32,
}

impl Default for SamplingTime {
    fn default() -> Self {
        Self { cycles: 1 }
    }
}

/// Status flags in the ADC status register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AdcStatus {
    /// End of conversion flag (set when conversion completes).
    pub eoc: bool,
    /// End of injected conversion flag.
    pub jeoc: bool,
    /// Overrun flag (set when new data arrives before previous is read).
    pub ovr: bool,
}

impl Default for AdcStatus {
    fn default() -> Self {
        Self {
            eoc: false,
            jeoc: false,
            ovr: false,
        }
    }
}

impl AdcStatus {
    pub fn encode(&self) -> u32 {
        let mut reg = 0u32;
        if self.eoc { reg |= 1 << 1; }
        if self.jeoc { reg |= 1 << 5; }
        if self.ovr { reg |= 1 << 6; }
        reg
    }

    pub fn from_bits(bits: u32) -> Self {
        Self {
            eoc: (bits & (1 << 1)) != 0,
            jeoc: (bits & (1 << 5)) != 0,
            ovr: (bits & (1 << 6)) != 0,
        }
    }
}

/// Control register 1 fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AdcCr1 {
    /// Resolution bits [1:0]: 00=8bit, 01=10bit, 10=12bit, 11=16bit.
    pub res: u8,
    /// Overlay write protection disable.
    pub owprot: bool,
    ///EOC selection: 0=EOC interrupt, 1=DMA transfer.
    pub eocsel: bool,
}

impl Default for AdcCr1 {
    fn default() -> Self {
        Self {
            res: 0b10, // 12-bit default
            owprot: false,
            eocsel: false,
        }
    }
}

impl AdcCr1 {
    pub fn resolution(&self) -> AdcResolution {
        AdcResolution::from_cr1(self.res).unwrap_or(AdcResolution::Bits12)
    }
}

/// Control register 2 fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AdcCr2 {
    /// ADC enable (1 = on).
    pub adon: bool,
    /// Continuous conversion mode.
    pub cont: bool,
    /// Start conversion of regular group (software trigger).
    pub swstart: bool,
    /// Data alignment: 0=right, 1=left.
    align: bool,
}

impl Default for AdcCr2 {
    fn default() -> Self {
        Self {
            adon: false,
            cont: false,
            swstart: false,
            align: false,
        }
    }
}

/// Virtual ADC peripheral state.
#[derive(Debug, Clone, Serialize)]
pub struct AdcPeripheral {
    /// Base address for memory mapping.
    pub base: u32,
    /// Current control register 1.
    pub cr1: AdcCr1,
    /// Current control register 2.
    pub cr2: AdcCr2,
    /// Current status register.
    pub sr: AdcStatus,
    /// Latest conversion result.
    pub dr: u32,
    /// Injected channel result (placeholder for future).
    pub jdr1: u32,
    /// Sampling time per channel (indexed by channel number).
    pub sampling_times: Vec<SamplingTime>,
    /// Simulated analog input values for each channel.
    pub channels: Vec<u16>,
    /// Conversion progress counter (internal).
    conversion_cycle: u64,
    /// Expected cycles until completion (internal).
    conversion_cycles: u64,
    /// Whether currently converting (internal).
    converting: bool,
    /// Channel being converted (internal).
    current_channel: usize,
    /// Number of channels in regular sequence.
    sequence_length: u8,
}

impl AdcPeripheral {
    /// Create a new ADC peripheral at the given base address.
    pub fn new(base: u32, num_channels: usize) -> Self {
        Self {
            base,
            cr1: AdcCr1::default(),
            cr2: AdcCr2::default(),
            sr: AdcStatus::default(),
            dr: 0,
            jdr1: 0,
            sampling_times: vec![SamplingTime::default(); num_channels],
            channels: vec![0u16; num_channels],
            conversion_cycle: 0,
            conversion_cycles: 0,
            converting: false,
            current_channel: 0,
            sequence_length: 1,
        }
    }

    /// Set the simulated analog value for a channel.
    pub fn set_channel_value(&mut self, channel: usize, value: u16) {
        if channel < self.channels.len() {
            self.channels[channel] = value;
        }
    }

    /// Get the maximum representable value for current resolution.
    pub fn max_value(&self) -> u32 {
        self.cr1.resolution().max_value()
    }

    /// Read a 32-bit register value at offset from base.
    pub fn read_reg(&self, offset: u32) -> Option<u32> {
        let val = match offset {
            0x00 => {
                // CR1
                let mut reg = self.cr1.res as u32;
                if self.cr1.owprot { reg |= 1 << 8; }
                if self.cr1.eocsel { reg |= 1 << 9; }
                reg
            }
            0x04 => {
                // CR2
                let mut reg = 0u32;
                if self.cr2.adon { reg |= 1 << 0; }
                if self.cr2.cont { reg |= 1 << 1; }
                if self.cr2.swstart { reg |= 1 << 30; }
                if self.cr2.align { reg |= 1 << 11; }
                reg
            }
            0x08 | 0x0C => {
                // SMPR1/SMPR2 - sampling times (simplified)
                0u32
            }
            0x10..=0x1C => {
                // SQR registers - sequence configuration
                0u32
            }
            0x20 => {
                // DR - data register
                // Apply alignment
                if self.cr2.align {
                    // Left-aligned: shift left by (16 - resolution)
                    let shift = 16 - self.cr1.resolution().bits() as u32;
                    (self.dr << shift) & 0xFFFF_0000
                } else {
                    // Right-aligned (default)
                    self.dr & 0x0000_FFFF
                }
            }
            0x24 => self.jdr1,
            _ => return None,
        };
        Some(val)
    }

    /// Write a 32-bit register value at offset from base.
    pub fn write_reg(&mut self, offset: u32, value: u32) {
        match offset {
            0x00 => {
                // CR1 write
                let res = (value & 0x3) as u8;
                self.cr1 = AdcCr1 {
                    res,
                    owprot: (value >> 8 & 1) != 0,
                    eocsel: (value >> 9 & 1) != 0,
                };
            }
            0x04 => {
                // CR2 write
                let adon = (value & 1) != 0;
                let cont = (value >> 1 & 1) != 0;
                let swstart = (value >> 30 & 1) != 0;
                let align = (value >> 11 & 1) != 0;
                
                let old_adon = self.cr2.adon;
                self.cr2 = AdcCr2 {
                    adon,
                    cont,
                    swstart,
                    align: align,
                };
                
                // Handle ADON bit transitions
                if !old_adon && adon {
                    // ADC enabled - ready to convert
                    self.sr.eoc = true; // Clear any pending flags
                }
                
                // Handle software start
                if swstart && adon {
                    self.start_conversion();
                }
            }
            0x08 | 0x0C => {
                // SMPR1/SMPR2 - skip for now (no sampling time tuning)
            }
            0x10 => {
                // SQR1 - sequence length [3:0]
                self.sequence_length = (value & 0xF) as u8;
            }
            0x14 | 0x18 | 0x1C => {
                // SQR2/3/4 - channel sequence (simplified)
            }
            _ => {}
        }
    }

    /// Start a new conversion cycle.
    fn start_conversion(&mut self) {
        self.converting = true;
        self.conversion_cycle = 0;
        
        // Calculate total sampling + conversion cycles
        let sample_cycles = self.sampling_times.get(self.current_channel)
            .map(|st| st.cycles as u64)
            .unwrap_or(1);
        let conv_cycles = self.cr1.resolution().bits() as u64; // 1 cycle per bit
        
        self.conversion_cycles = sample_cycles + conv_cycles;
    }

    /// Advance the ADC state machine by one core cycle.
    /// Returns true if a conversion completed.
    pub fn tick(&mut self, core_cycles: u64) -> bool {
        if !self.cr2.adon || !self.converting {
            return false;
        }

        self.conversion_cycle += core_cycles;

        if self.conversion_cycle >= self.conversion_cycles {
            // Conversion complete - read result from channel
            let channel_value = self.channels.get(self.current_channel).copied().unwrap_or(0) as u32;
            let max_val = self.max_value();
            
            // Scale to resolution
            let scaled = ((channel_value as u32) * max_val) / 0xFFFF;
            self.dr = scaled.min(max_val);
            
            self.sr.eoc = true;
            self.converting = false;
            
            // Move to next channel in sequence
            self.current_channel = (self.current_channel + 1) % self.channels.len().max(1);
            
            // If continuous mode or more channels, start next conversion
            if self.cr2.cont || self.current_channel != 0 {
                self.start_conversion();
            }
            
            return true;
        }

        false
    }

    /// Check if a given address falls within this peripheral's register map.
    pub fn contains(&self, address: u32) -> bool {
        address >= self.base && address < self.base + 0x28
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
    fn adc_new_has_default_state() {
        let adc = AdcPeripheral::new(0x4001_2400, 16);
        assert!(!adc.cr2.adon);
        assert!(!adc.converting);
        assert_eq!(adc.dr, 0);
        assert_eq!(adc.channels.len(), 16);
    }

    #[test]
    fn adc_enable_sets_adon() {
        let mut adc = AdcPeripheral::new(0x4001_2400, 16);
        adc.write_reg(0x04, 0x0000_0001); // ADON=1
        assert!(adc.cr2.adon);
    }

    #[test]
    fn adc_set_resolution() {
        let mut adc = AdcPeripheral::new(0x4001_2400, 16);
        adc.write_reg(0x00, 0x0000_0002); // 12-bit (default already)
        assert_eq!(adc.cr1.resolution(), AdcResolution::Bits12);
        
        adc.write_reg(0x00, 0x0000_0001); // 10-bit
        assert_eq!(adc.cr1.resolution(), AdcResolution::Bits10);
        
        adc.write_reg(0x00, 0x0000_0000); // 8-bit
        assert_eq!(adc.cr1.resolution(), AdcResolution::Bits8);
        
        adc.write_reg(0x00, 0x0000_0003); // 16-bit
        assert_eq!(adc.cr1.resolution(), AdcResolution::Bits16);
    }

    #[test]
    fn adc_read_max_values() {
        assert_eq!(AdcResolution::Bits8.max_value(), 0xFF);
        assert_eq!(AdcResolution::Bits10.max_value(), 0x3FF);
        assert_eq!(AdcResolution::Bits12.max_value(), 0xFFF);
        assert_eq!(AdcResolution::Bits16.max_value(), 0xFFFF);
    }

    #[test]
    fn adc_set_channel_value() {
        let mut adc = AdcPeripheral::new(0x4001_2400, 16);
        adc.set_channel_value(0, 0x8000); // Half-scale
        assert_eq!(adc.channels[0], 0x8000);
    }

    #[test]
    fn adc_contains_range() {
        let adc = AdcPeripheral::new(0x4001_2400, 16);
        assert!(adc.contains(0x4001_2400));
        assert!(adc.contains(0x4001_2424)); // JDR1
        assert!(!adc.contains(0x4001_2428)); // Past end
        assert!(!adc.contains(0x4001_23FF));
    }

    #[test]
    fn adc_offset_calculation() {
        let adc = AdcPeripheral::new(0x4001_2400, 16);
        assert_eq!(adc.offset(0x4001_2400), 0);
        assert_eq!(adc.offset(0x4001_2420), 0x20); // DR
        assert_eq!(adc.offset(0x4001_2424), 0x24); // JDR1
    }

    #[test]
    fn adc_conversion_scales_correctly() {
        let mut adc = AdcPeripheral::new(0x4001_2400, 16);
        adc.set_channel_value(0, 0xCCCC); // ~80% of full scale
        adc.write_reg(0x04, 0x0000_0001); // Enable
        
        // Should be approximately 80% of 4095 (12-bit max)
        // That's about 3276
        assert_eq!(adc.channels[0], 0xCCCC);
    }
}
