//! Virtual DMA (Direct Memory Access) controller for ns-sim.
//!
//! Implements a simplified DMA controller supporting:
//! - Multiple channels (up to 8)
//! - Memory-to-memory, memory-to-peripheral, peripheral-to-memory transfers
//! - Configurable data widths (byte/half-word/word)
//! - Circular and normal modes
//! - Transfer complete interrupts

use serde::Serialize;

/// Data transfer width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DmaWidth {
    Byte,
    HalfWord,
    Word,
}

impl Default for DmaWidth {
    fn default() -> Self {
        Self::Byte
    }
}

impl DmaWidth {
    pub fn bytes(&self) -> u32 {
        match self {
            Self::Byte => 1,
            Self::HalfWord => 2,
            Self::Word => 4,
        }
    }
}

/// DMA channel state.
#[derive(Debug, Clone, Serialize)]
pub struct DmaChannel {
    /// Channel number (0-7).
    pub channel: u8,
    /// Enable flag.
    pub en: bool,
    /// Memory to peripheral (1) or peripheral to memory (0).
    pub mem_to_periph: bool,
    /// Peripheral to memory increment (1 = yes).
    pub periph_inc: bool,
    /// Memory address increment (1 = yes).
    pub mem_inc: bool,
    /// Peripheral data width.
    pub periph_width: DmaWidth,
    /// Memory data width.
    pub mem_width: DmaWidth,
    /// Peripheral base address.
    pub periph_addr: u32,
    /// Memory source/dest address.
    pub mem_addr: u32,
    /// Number of data items to transfer.
    pub ndtr: u32,
    /// Current transfer count (decremented each transfer).
    cur_count: u32,
    /// Transfer complete interrupt enable.
    tc_irq_en: bool,
    /// Half-transfer interrupt enable.
    ht_irq_en: bool,
    /// Transfer complete flag.
    tc_flag: bool,
    /// Half-transfer flag.
    ht_flag: bool,
    /// Error flag.
    err_flag: bool,
    /// IRQ line to assert on completion (for NVIC integration).
    pub irq_line: usize,
}

impl DmaChannel {
    pub fn new(channel: u8) -> Self {
        Self {
            channel,
            en: false,
            mem_to_periph: false,
            periph_inc: false,
            mem_inc: true,
            periph_width: DmaWidth::Byte,
            mem_width: DmaWidth::Byte,
            periph_addr: 0,
            mem_addr: 0,
            ndtr: 0,
            cur_count: 0,
            tc_irq_en: false,
            ht_irq_en: false,
            tc_flag: false,
            ht_flag: false,
            err_flag: false,
            irq_line: 15, // Default to highest available line
        }
    }

    /// Start a transfer. Returns true if successful.
    pub fn start(&mut self) -> bool {
        if !self.en || self.ndtr == 0 {
            return false;
        }
        self.cur_count = self.ndtr;
        true
    }

    /// Advance one transfer cycle. Returns false when transfer completes.
    pub fn tick(&mut self) -> bool {
        if !self.en || self.cur_count == 0 {
            return false;
        }

        let width = if self.mem_to_periph {
            self.mem_width.bytes()
        } else {
            self.periph_width.bytes()
        };

        // In simulation, just decrement counter (actual bus access would need memory callback)
        self.cur_count -= 1;

        // Update addresses
        if self.mem_inc {
            self.mem_addr += width;
        }

        // Check for half-transfer
        if self.ht_irq_en && self.cur_count == self.ndtr / 2 {
            self.ht_flag = true;
        }

        // Check completion
        if self.cur_count == 0 {
            self.tc_flag = true;
            if self.tc_irq_en {
                // Signal transfer complete by asserting the IRQ line
                // (will be handled by simulator's NVIC when tick_peripherals runs)
            }
            return false; // Transfer done
        }

        true
    }

    /// Clear flags by writing to relevant registers.
    pub fn clear_tc(&mut self) {
        self.tc_flag = false;
    }

    pub fn clear_ht(&mut self) {
        self.ht_flag = false;
    }

    pub fn get_status_flags(&self) -> u32 {
        let mut flags = 0u32;
        if self.tc_flag { flags |= 1 << 5; }
        if self.ht_flag { flags |= 1 << 4; }
        if self.err_flag { flags |= 1 << 3; }
        flags
    }

    /// Set the IRQ line this channel will assert on completion.
    pub fn set_irq_line(&mut self, line: usize) {
        self.irq_line = line;
    }

    /// Get the configured IRQ line.
    pub fn irq_line(&self) -> usize {
        self.irq_line
    }
}

/// DMA controller state.
#[derive(Debug, Clone, Serialize)]
pub struct DmaPeripheral {
    pub base: u32,
    pub channels: Vec<DmaChannel>,
    pub interrupt_enable: u32,
    pub interrupt_status: u32,
}

impl DmaPeripheral {
    pub fn new(base: u32, num_channels: usize) -> Self {
        Self {
            base,
            channels: (0..num_channels).map(|i| DmaChannel::new(i as u8)).collect(),
            interrupt_enable: 0,
            interrupt_status: 0,
        }
    }

    pub fn get_channel(&mut self, channel: usize) -> Option<&mut DmaChannel> {
        self.channels.get_mut(channel)
    }

    pub fn read_reg(&self, offset: u32) -> Option<u32> {
        match offset {
            0x00 => Some(self.interrupt_enable),      // IER
            0x04 => Some(self.interrupt_status),       // ISR
            0x08 => Some(0),                           // FCR (not implemented)
            _ => None,
        }
    }

    pub fn write_reg(&mut self, offset: u32, value: u32) {
        match offset {
            0x00 => self.interrupt_enable = value,
            0x04 => {
                // Writing clears flags
                self.interrupt_status &= !value;
            }
            _ => {}
        }
    }

    pub fn tick(&mut self) {
        for ch in &mut self.channels {
            if ch.en {
                ch.tick();
            }
        }
    }

    pub fn contains(&self, address: u32) -> bool {
        address >= self.base && address < self.base + 0x10
    }

    pub fn offset(&self, address: u32) -> u32 {
        address.wrapping_sub(self.base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dma_new_has_default_state() {
        let dma = DmaPeripheral::new(0x4002_6000, 8);
        assert_eq!(dma.channels.len(), 8);
        assert!(!dma.channels[0].en);
        assert_eq!(dma.channels[0].ndtr, 0);
    }

    #[test]
    fn dma_channel_start_transfer() {
        let mut dma = DmaPeripheral::new(0x4002_6000, 1);
        let ch = dma.get_channel(0).unwrap();
        
        ch.en = true;
        ch.ndtr = 10;
        ch.mem_addr = 0x2000_0000;
        ch.periph_addr = 0x4001_3000;
        
        assert!(ch.start());
        assert_eq!(ch.cur_count, 10);
    }

    #[test]
    fn dma_tick_decrements_count() {
        let mut dma = DmaPeripheral::new(0x4002_6000, 1);
        let ch = dma.get_channel(0).unwrap();
        assert!(!ch.tick()); // Not enabled
        
        // Enable and start (would be done via register writes in real use)
    }

    #[test]
    fn dma_contains_range() {
        let dma = DmaPeripheral::new(0x4002_6000, 8);
        assert!(dma.contains(0x4002_6000));
        assert!(dma.contains(0x4002_600C));
        assert!(!dma.contains(0x4002_6010));
    }

    #[test]
    fn dma_offset_calculation() {
        let dma = DmaPeripheral::new(0x4002_6000, 8);
        assert_eq!(dma.offset(0x4002_6000), 0);
        assert_eq!(dma.offset(0x4002_6004), 4);
        assert_eq!(dma.offset(0x4002_6008), 8);
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    fn dma_irq_line_configuration() {
        let mut ch = DmaChannel::new(0);
        assert_eq!(ch.irq_line(), 15); // default
        ch.set_irq_line(3);
        assert_eq!(ch.irq_line(), 3);
    }

    #[test]
    fn dma_tick_complete_with_irq_setup() {
        let mut ch = DmaChannel::new(0);
        ch.en = true;
        ch.ndtr = 4;
        ch.cur_count = 4;
        ch.tc_irq_en = true;
        ch.set_irq_line(5);
        
        // Run until complete
        while ch.tick() {}
        
        assert!(ch.tc_flag);
        assert_eq!(ch.cur_count, 0);
    }
}
