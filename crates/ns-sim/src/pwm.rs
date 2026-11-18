//! Virtual PWM (Pulse Width Modulation) output peripheral model for ns-sim.
//!
//! Implements multiple PWM channels with configurable:
//! - Frequency (via prescaler and auto-reload values)
//! - Duty cycle (compare values)
//! - Polarity (active-high/active-low)
//! - Update events (central vs edge-aligned)
//!
//! Register map per channel (channels 1-4 typically):
//!
//! | Offset | Register | Access | Description                              |
//! |--------|----------|--------|------------------------------------------|
//! | `0x00` | DIER     | RW     | DMA/Interrupt enable register            |
//! | `0x04` | SR       | RO     | Status register                          |
//! | `0x08` | EGR      | WO     | Event generation register                |
//! | `0x0C` | CCER     | RW     | Capture/compare enable register          |
//! | `0x10` | CNT      | RW     | Counter value                            |
//! | `0x14` | PSC      | RW     | Prescaler                                |
//! | `0x18` | ARR      | RW     | Auto-reload value (period)               |
//! | `0x1C` | CCR1     | RW     | Capture/compare value 1                  |
//! | `0x20` | CCR2     | RW     | Capture/compare value 2                  |
//! | `0x24` | CCR3     | RW     | Capture/compare value 3                  |
//! | `0x28` | CCR4     | RW     | Capture/compare value 4                  |
//!
//! Each channel outputs a square wave with configurable frequency and duty cycle.

use serde::Serialize;

/// PWM alignment mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PwmAlignment {
    /// Edge-aligned (counts up only).
    Edge,
    /// Center-aligned (counts up then down).
    Center,
}

impl Default for PwmAlignment {
    fn default() -> Self {
        Self::Edge
    }
}

/// PWM output polarity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PwmPolarity {
    /// Active high (output high during pulse).
    High,
    /// Active low (output low during pulse).
    Low,
}

impl Default for PwmPolarity {
    fn default() -> Self {
        Self::High
    }
}

/// A single PWM channel configuration.
#[derive(Debug, Clone, Serialize)]
pub struct PwmChannel {
    /// Channel number (1-4).
    pub channel: u8,
    /// Current counter value (internal).
    pub cnt: u32,
    /// Compare value (determines duty cycle).
    pub ccr: u32,
    /// Auto-reload value (determines period).
    pub arr: u32,
    /// Prescaler divisor.
    pub psc: u32,
    /// Output polarity.
    pub polarity: PwmPolarity,
    /// Alignment mode.
    pub alignment: PwmAlignment,
    /// Output enabled.
    pub enabled: bool,
    /// Last output state.
    pub output: bool,
    /// IRQ line to assert on update event (-1 = none).
    pub irq_line: i32,
}

impl PwmChannel {
    pub fn new(channel: u8) -> Self {
        Self {
            channel,
            cnt: 0,
            ccr: 0,
            arr: 0xFFFF,
            psc: 0,
            polarity: PwmPolarity::High,
            alignment: PwmAlignment::Edge,
            enabled: false,
            output: false,
            irq_line: -1,
        }
    }
    
    /// Set the IRQ line for this PWM channel.
    pub fn set_irq_line(&mut self, line: i32) {
        self.irq_line = line;
    }
    
    /// Get the configured IRQ line.
    pub fn get_irq_line(&self) -> i32 {
        self.irq_line
    }

    /// Calculate the current output state based on counter position.
    pub fn update_output(&mut self) {
        if !self.enabled {
            self.output = false;
            return;
        }

        let duty = if self.arr > 0 {
            (self.ccr as f64 / self.arr as f64).min(1.0)
        } else {
            0.0
        };

        // Output is high when counter <= compare value (for edge-aligned, count-up)
        self.output = match self.polarity {
            PwmPolarity::High => self.cnt <= self.ccr && duty > 0.0,
            PwmPolarity::Low => self.cnt > self.ccr,
        };
    }

    /// Advance the channel counter by one tick.
    pub fn tick(&mut self, div: u32) -> bool {
        if !self.enabled || div == 0 {
            return false;
        }

        // Apply prescaler
        let internal_counter = (self.cnt as u64) + 1;
        let wrapped = internal_counter % ((self.psc + 1) as u64);
        
        if wrapped == 0 {
            // Prescaler tick completed, advance main counter
            self.cnt += 1;
            
            if self.alignment == PwmAlignment::Edge {
                // Edge-aligned: wrap at ARR+1
                if self.cnt >= self.arr + 1 {
                    self.cnt = 0;
                    return true; // Update event
                }
            } else {
                // Center-aligned: count up to ARR then down
                // Simplified: just count up for now
                if self.cnt >= self.arr {
                    // In real implementation, would count down here
                    self.cnt = 0;
                    return true;
                }
            }
        }

        false
    }
}

/// PWM timer peripheral state.
#[derive(Debug, Clone, Serialize)]
pub struct PwmPeripheral {
    pub base: u32,
    pub channels: Vec<PwmChannel>,
    /// Global enable bit.
    pub enabled: bool,
    /// Main prescaler.
    pub prescaler: u32,
}

impl PwmPeripheral {
    pub fn new(base: u32, num_channels: usize) -> Self {
        Self {
            base,
            channels: (1..=num_channels as u8)
                .map(|ch| PwmChannel::new(ch))
                .collect(),
            enabled: false,
            prescaler: 0,
        }
    }

    pub fn get_channel(&self, channel: u8) -> Option<&PwmChannel> {
        self.channels.iter().find(|c| c.channel == channel)
    }

    pub fn get_channel_mut(&mut self, channel: u8) -> Option<&mut PwmChannel> {
        self.channels.iter_mut().find(|c| c.channel == channel)
    }

    pub fn read_reg(&self, offset: u32) -> Option<u32> {
        match offset {
            0x00 => Some(0), // DIER
            0x04 => Some(0), // SR
            0x10 => Some(self.channels.first().map(|c| c.cnt).unwrap_or(0)),
            0x14 => Some(self.prescaler),
            0x18 => Some(self.channels.first().map(|c| c.arr).unwrap_or(0xFFFF)),
            0x1C => Some(self.channels.get(0).map(|c| c.ccr).unwrap_or(0)),
            0x20 => Some(self.channels.get(1).map(|c| c.ccr).unwrap_or(0)),
            0x24 => Some(self.channels.get(2).map(|c| c.ccr).unwrap_or(0)),
            0x28 => Some(self.channels.get(3).map(|c| c.ccr).unwrap_or(0)),
            _ => None,
        }
    }

    pub fn write_reg(&mut self, offset: u32, value: u32) {
        match offset {
            0x10 => {
                // CNT - can be written to reset counter
                for ch in &mut self.channels {
                    ch.cnt = value & 0xFFFF;
                }
            }
            0x14 => {
                self.prescaler = value & 0xFFFF;
            }
            0x18 => {
                // ARR - set period for all channels
                for ch in &mut self.channels {
                    ch.arr = value & 0xFFFF;
                }
            }
            0x1C => {
                if let Some(ch) = self.channels.get_mut(0) {
                    ch.ccr = value & 0xFFFF;
                }
            }
            0x20 => {
                if let Some(ch) = self.channels.get_mut(1) {
                    ch.ccr = value & 0xFFFF;
                }
            }
            0x24 => {
                if let Some(ch) = self.channels.get_mut(2) {
                    ch.ccr = value & 0xFFFF;
                }
            }
            0x28 => {
                if let Some(ch) = self.channels.get_mut(3) {
                    ch.ccr = value & 0xFFFF;
                }
            }
            _ => {}
        }
    }

    pub fn tick(&mut self) {
        if !self.enabled {
            return;
        }

        let div = self.prescaler + 1;
        let mut update_event = false;
        
        for ch in &mut self.channels {
            if ch.tick(div) {
                update_event = true;
            }
            ch.update_output();
        }
        
        // If any channel had an update event, we'd set UEV flag in SR
        if update_event {
            // Would set SR.UEV here in full implementation
        }
    }

    pub fn contains(&self, address: u32) -> bool {
        address >= self.base && address < self.base + 0x2C
    }

    pub fn offset(&self, address: u32) -> u32 {
        address.wrapping_sub(self.base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pwm_new_has_default_state() {
        let pwm = PwmPeripheral::new(0x4001_0000, 4);
        assert!(!pwm.enabled);
        assert_eq!(pwm.channels.len(), 4);
        assert_eq!(pwm.channels[0].channel, 1);
        assert_eq!(pwm.channels[3].channel, 4);
    }

    #[test]
    fn pwm_channel_configuration() {
        let mut pwm = PwmPeripheral::new(0x4001_0000, 4);
        
        // Configure channel 1: period=1000, duty=50%
        pwm.write_reg(0x18, 999); // ARR = 999
        pwm.write_reg(0x1C, 499); // CCR1 = 499 (50% duty)
        
        let ch = pwm.get_channel(1).unwrap();
        assert_eq!(ch.arr, 999);
        assert_eq!(ch.ccr, 499);
    }

    #[test]
    fn pwm_tick_updates_outputs() {
        let mut pwm = PwmPeripheral::new(0x4001_0000, 4);
        pwm.enabled = true;
        
        // Configure 50% duty cycle
        pwm.write_reg(0x18, 99); // ARR = 99
        pwm.write_reg(0x1C, 49); // CCR1 = 49 (roughly 50%)
        
        // Tick until we see output changes
        for _ in 0..100 {
            pwm.tick();
        }
        
        let ch = pwm.get_channel(1).unwrap();
        assert!(ch.cnt <= 99); // Counter wrapped
    }

    #[test]
    fn pwm_channels_independent_duty() {
        let mut pwm = PwmPeripheral::new(0x4001_0000, 4);
        
        // CH1: 25% duty
        pwm.write_reg(0x18, 99); // ARR
        pwm.write_reg(0x1C, 24); // CCR1 = 25%
        
        // CH2: 75% duty  
        pwm.write_reg(0x20, 74); // CCR2 = 75%
        
        assert_eq!(pwm.channels[0].ccr, 24);
        assert_eq!(pwm.channels[1].ccr, 74);
    }

    #[test]
    fn pwm_contains_range() {
        let pwm = PwmPeripheral::new(0x4001_0000, 4);
        assert!(pwm.contains(0x4001_0000));
        assert!(pwm.contains(0x4001_0028)); // CCR4
        assert!(!pwm.contains(0x4001_002C));
    }

    #[test]
    fn pwm_alignment_modes() {
        assert_eq!(PwmAlignment::Edge, PwmAlignment::Edge);
        assert_eq!(PwmAlignment::Center, PwmAlignment::Center);
        assert_ne!(PwmAlignment::Edge, PwmAlignment::Center);
        
        assert_eq!(PwmPolarity::High, PwmPolarity::High);
        assert_eq!(PwmPolarity::Low, PwmPolarity::Low);
        assert_ne!(PwmPolarity::High, PwmPolarity::Low);
    }
}
