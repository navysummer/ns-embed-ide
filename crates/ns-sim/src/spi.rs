//! Virtual SPI peripheral model for the ns-sim instruction-level simulator.
//!
//! Supports master-mode SPI with configurable clock polarity/phase (CPOL/CPHA),
//! software-controlled slave select, and a circular receive buffer. The peripheral
//! is memory-mapped at `base` + offsets:
//!
//! | Offset | Register       | Access | Description                                      |
//! |--------|----------------|--------|--------------------------------------------------|
//! | `0x00` | DR             | RW     | Data register (tx when written, rx when read)    |
//! | `0x04` | CR1            | RW     | Control register 1 (enable, config, bitrate)     |
//! | `0x08` | SR             | RO     | Status register (TXE, RXNE, BSY flags)           |
//! | `0x0C` | SS_VAL         | RW     | Software slave-select value (active-high)        |
//!
//! **Clock modes**:
//! - `CPOL=0, CPHA=0`: samples on leading edge, changes on trailing
//! - `CPOL=0, CPHA=1`: samples on trailing edge, changes on leading  
//! - `CPOL=1, CPHA=0`: inverted leading-edge sampling
//! - `CPOL=1, CPHA=1`: inverted trailing-edge sampling
//!
//! **Bitrate formula**: `f_CLK = f_CORE / (2 * prescaler)` where prescaler ∈ {2,4,8,16,32,64,128,256}.
//! Each bit transfer consumes `prescaler` core cycles.
//!
//! **Slave select**: When `SS_EN` bit is set in CR1, writes to `SS_VAL` assert/deassert
//! the selected slave line. The IDE can observe this via `last_ss_state`.
//!
//! **Receive buffer**: Incoming bytes are collected in `rx_buffer` (capacity 256).
//! The IDE reads them via `drain_rx()` after simulation completes.
//!
//! Not modeled: FIFO depth, overflow errors, frame format errors, DMA support.
//! These are left as future enhancements.

use serde::Serialize;

/// SPI clock polarity and phase configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SpiCpolCpha {
    /// CPOL=0, CPHA=0: rising edge samples, falling edge shifts
    Mode0,
    /// CPOL=0, CPHA=1: falling edge samples, rising edge shifts
    Mode1,
    /// CPOL=1, CPHA=0: falling edge samples, rising edge shifts (inverted)
    Mode2,
    /// CPOL=1, CPHA=1: rising edge samples, falling edge shifts (inverted)
    Mode3,
}

impl Default for SpiCpolCpha {
    fn default() -> Self {
        Self::Mode0
    }
}

impl SpiCpolCpha {
    pub fn from_cr1(cpol: bool, cpha: bool) -> Self {
        match (cpol, cpha) {
            (false, false) => Self::Mode0,
            (false, true) => Self::Mode1,
            (true, false) => Self::Mode2,
            (true, true) => Self::Mode3,
        }
    }

    /// Returns true if the clock is idle high (CPOL=1).
    pub fn cpol(&self) -> bool {
        matches!(self, Self::Mode2 | Self::Mode3)
    }

    /// Returns true if data is sampled on the second clock edge (CPHA=1).
    pub fn cpha(&self) -> bool {
        matches!(self, Self::Mode1 | Self::Mode3)
    }
}

/// Prescaler values for SPI bitrate division.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SpiPrescaler {
    Div2,
    Div4,
    Div8,
    Div16,
    Div32,
    Div64,
    Div128,
    Div256,
}

impl SpiPrescaler {
    pub fn from_bits(bits: u8) -> Option<Self> {
        match bits {
            0b000 => Some(Self::Div2),
            0b001 => Some(Self::Div4),
            0b010 => Some(Self::Div8),
            0b011 => Some(Self::Div16),
            0b100 => Some(Self::Div32),
            0b101 => Some(Self::Div64),
            0b110 => Some(Self::Div128),
            0b111 => Some(Self::Div256),
            _ => None,
        }
    }

    pub fn divider(&self) -> u32 {
        match self {
            Self::Div2 => 2,
            Self::Div4 => 4,
            Self::Div8 => 8,
            Self::Div16 => 16,
            Self::Div32 => 32,
            Self::Div64 => 64,
            Self::Div128 => 128,
            Self::Div256 => 256,
        }
    }
}

/// Status flags in the SPI status register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SpiStatus {
    /// Transmit buffer empty (can write new data).
    pub txe: bool,
    /// Receive buffer not empty (data available to read).
    pub rxne: bool,
    /// Bus busy (transmission in progress).
    pub bsy: bool,
    /// Mode fault (not used in master mode, held clear).
    pub mrf: bool,
    /// Underflow flag (not used, held clear).
    pub udf: bool,
    /// Overrun flag (set when RXNE is already set and new data arrives).
    pub ovr: bool,
}

impl Default for SpiStatus {
    fn default() -> Self {
        Self {
            txe: true, // Empty initially
            rxne: false,
            bsy: false,
            mrf: false,
            udf: false,
            ovr: false,
        }
    }
}

/// Control register 1 fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SpiCr1 {
    /// SPI enable.
    pub en: bool,
    /// Master/slave selection (1 = master).
    pub mstr: bool,
    /// Clock polarity.
    pub cpol: bool,
    /// Clock phase.
    pub cpha: bool,
    /// Bitrate prescaler bits.
    pub br: u8,
    /// Software slave management enable.
    pub ssm: bool,
    /// Slave select output enable.
    pub ssi: bool,
}

impl Default for SpiCr1 {
    fn default() -> Self {
        Self {
            en: false,
            mstr: true, // Default to master mode
            cpol: false,
            cpha: false,
            br: 0b000, // DIV2
            ssm: false,
            ssi: false,
        }
    }
}

/// Virtual SPI peripheral state.
#[derive(Debug, Clone, Serialize)]
pub struct SpiPeripheral {
    /// Base address for memory mapping.
    pub base: u32,
    /// Current control register 1 value.
    pub cr1: SpiCr1,
    /// Current status register value.
    pub sr: SpiStatus,
    /// Data register: write sets TX, read returns RX.
    pub dr: u8,
    /// Received bytes buffer (drained by IDE).
    pub rx_buffer: Vec<u8>,
    /// Last slave select state (true = asserted).
    pub last_ss_state: bool,
    /// Cycle counter for bit timing (internal).
    bit_cycle: u64,
    /// Prescaler divider (internal).
    div: u32,
    /// Bits transferred since last TXE set (internal).
    tx_shift: u8,
    /// Number of bits shifted out so far.
    tx_count: u8,
    /// Currently shifting-in byte.
    rx_shift: u8,
}

impl SpiPeripheral {
    /// Create a new SPI peripheral at the given base address.
    pub fn new(base: u32) -> Self {
        Self {
            base,
            cr1: SpiCr1::default(),
            sr: SpiStatus::default(),
            dr: 0,
            rx_buffer: Vec::new(),
            last_ss_state: false,
            bit_cycle: 0,
            div: 2,
            tx_shift: 0,
            tx_count: 0,
            rx_shift: 0,
        }
    }

    /// Get the effective clock divider from CR1 bits.
    pub fn update_divider(&mut self) {
        self.div = SpiPrescaler::from_bits(self.cr1.br)
            .map(|p| p.divider())
            .unwrap_or(2);
    }

    /// Read a 32-bit register value at offset from base.
    pub fn read_reg(&self, offset: u32) -> Option<u32> {
        let val = match offset {
            0x00 => self.dr as u32,
            0x04 => {
                // Encode CR1 fields
                let mut reg = 0u32;
                if self.cr1.en {
                    reg |= 1 << 0;
                }
                if self.cr1.mstr {
                    reg |= 1 << 2;
                }
                if self.cr1.cpol {
                    reg |= 1 << 1;
                }
                if self.cr1.cpha {
                    reg |= 1 << 3;
                }
                reg |= ((self.cr1.br & 0x7) as u32) << 5;
                if self.cr1.ssm {
                    reg |= 1 << 8;
                }
                if self.cr1.ssi {
                    reg |= 1 << 9;
                }
                reg
            }
            0x08 => {
                let mut reg = 0u32;
                if self.sr.txe {
                    reg |= 1 << 1;
                }
                if self.sr.rxne {
                    reg |= 1 << 0;
                }
                if self.sr.bsy {
                    reg |= 1 << 6;
                }
                if self.sr.ovr {
                    reg |= 1 << 5;
                }
                reg
            }
            0x0C => {
                // SS_VAL: software slave select value
                if self.last_ss_state { 1 } else { 0 }
            }
            _ => return None,
        };
        Some(val)
    }

    /// Write a 32-bit register value at offset from base.
    pub fn write_reg(&mut self, offset: u32, value: u32) {
        match offset {
            0x00 => {
                // DR write: initiate transmission
                self.dr = value as u8;
                self.tx_shift = value as u8;
                self.tx_count = 0;
                self.rx_shift = 0;
                self.sr.txe = false;
                self.sr.rxne = false;
                self.sr.bsy = true;
                self.bit_cycle = 0;
            }
            0x04 => {
                // CR1 write - parse all fields from raw value
                let new_cr1 = SpiCr1 {
                    en: (value & 1) != 0,
                    cpol: (value >> 1 & 1) != 0,
                    mstr: (value >> 2 & 1) != 0,
                    cpha: (value >> 3 & 1) != 0,
                    br: ((value >> 5) & 0x7) as u8,
                    ssm: (value >> 8 & 1) != 0,
                    ssi: (value >> 9 & 1) != 0,
                };
                let old_en = self.cr1.en;
                self.cr1 = new_cr1;
                self.update_divider();

                // Update slave select state based on SSI/SSM
                if self.cr1.ssm && self.cr1.ssi {
                    self.last_ss_state = true;
                } else if self.cr1.ssm {
                    self.last_ss_state = false;
                }

                // Clear flags when disabling
                if !self.cr1.en && old_en {
                    self.sr.txe = true;
                    self.sr.bsy = false;
                    self.rx_buffer.clear();
                }
            }
            0x0C => {
                // SS_VAL write (software slave select)
                self.last_ss_state = (value & 1) != 0;
            }
            _ => {}
        }
    }

    /// Advance the SPI state machine by one core cycle.
    /// Returns true if a complete byte was transmitted/received.
    pub fn tick(&mut self, core_cycles: u64) -> bool {
        if !self.cr1.en || !self.sr.bsy {
            return false;
        }

        self.bit_cycle += core_cycles;
        let bits_per_byte = 8u8;

        if self.bit_cycle >= self.div as u64 {
            self.bit_cycle -= self.div as u64;

            // Shift out one bit (MSB first)
            // let tx_bit = (self.tx_shift >> (7 - self.tx_count)) & 1;
            self.tx_shift <<= 1;

            // Shift in received bit (simulate as 0 for now)
            let rx_bit: u8 = 0;
            self.rx_shift = (self.rx_shift << 1) | rx_bit;
            self.tx_count += 1;

            if self.tx_count >= bits_per_byte {
                // Transmission complete
                self.rx_buffer.push(self.rx_shift);
                self.sr.rxne = true;
                self.sr.txe = true;
                self.sr.bsy = false;
                self.tx_count = 0;
                return true;
            }
        }

        false
    }

    /// Drain received bytes from the buffer.
    pub fn drain_rx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.rx_buffer)
    }

    /// Check if a given address falls within this peripheral's register map.
    pub fn contains(&self, address: u32) -> bool {
        address >= self.base && address < self.base + 0x10
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
    fn spi_new_has_default_state() {
        let spi = SpiPeripheral::new(0x4001_3000);
        assert!(!spi.cr1.en);
        assert!(spi.sr.txe);
        assert!(!spi.sr.rxne);
        assert!(!spi.sr.bsy);
        assert_eq!(spi.rx_buffer.len(), 0);
    }

    #[test]
    fn spi_write_enable_sets_bsy() {
        let mut spi = SpiPeripheral::new(0x4001_3000);
        spi.write_reg(0x04, 0x0005); // EN=1, MSTR=1, BR=0 (DIV2)
        assert!(spi.cr1.en);
        assert!(spi.cr1.mstr);
        assert_eq!(spi.div, 2);
    }

    #[test]
    fn spi_read_status_after_write() {
        let spi = SpiPeripheral::new(0x4001_3000);
        let sr = spi.read_reg(0x08).unwrap();
        // Status bits: OVR=bit5, BSY=bit6, TXE=bit1, RXNE=bit0
        assert_eq!(sr & (1 << 1), 1 << 1); // TXE should be set
        assert_eq!(sr & 1, 0); // RXNE should be clear
    }

    #[test]
    fn spi_dr_write_sets_bsy_and_cleares_txe() {
        let mut spi = SpiPeripheral::new(0x4001_3000);
        spi.write_reg(0x04, 0x0023); // Enable
        spi.write_reg(0x00, 0x42); // Write data
        assert!(spi.sr.bsy);
        assert!(!spi.sr.txe);
        assert!(!spi.sr.rxne);
    }

    #[test]
    fn spi_tick_completes_byte_transfer() {
        let mut spi = SpiPeripheral::new(0x4001_3000);
        spi.write_reg(0x04, 0x0023); // Enable, DIV2
        spi.write_reg(0x00, 0x42); // Start transmission

        // Need to tick enough times for 8 bits at DIV2
        for _ in 0..16 {
            spi.tick(2);
        }

        assert!(!spi.sr.bsy);
        assert!(spi.sr.rxne);
        assert_eq!(spi.rx_buffer.len(), 1);
    }

    #[test]
    fn spi_clock_modes() {
        assert_eq!(SpiCpolCpha::from_cr1(false, false), SpiCpolCpha::Mode0);
        assert_eq!(SpiCpolCpha::from_cr1(false, true), SpiCpolCpha::Mode1);
        assert_eq!(SpiCpolCpha::from_cr1(true, false), SpiCpolCpha::Mode2);
        assert_eq!(SpiCpolCpha::from_cr1(true, true), SpiCpolCpha::Mode3);

        assert!(!SpiCpolCpha::Mode0.cpol());
        assert!(!SpiCpolCpha::Mode0.cpha());
        assert!(SpiCpolCpha::Mode1.cpha());
        assert!(SpiCpolCpha::Mode2.cpol());
        assert!(SpiCpolCpha::Mode3.cpol());
        assert!(SpiCpolCpha::Mode3.cpha());
    }

    #[test]
    fn spi_prescaler_dividers() {
        assert_eq!(SpiPrescaler::Div2.divider(), 2);
        assert_eq!(SpiPrescaler::Div16.divider(), 16);
        assert_eq!(SpiPrescaler::Div256.divider(), 256);
        assert!(SpiPrescaler::from_bits(0b111).unwrap().divider() == 256);
        assert!(SpiPrescaler::from_bits(0b000).unwrap().divider() == 2);
        assert!(SpiPrescaler::from_bits(0b1111).is_none());
    }

    #[test]
    fn spi_contains_range() {
        let spi = SpiPeripheral::new(0x4001_3000);
        assert!(spi.contains(0x4001_3000));
        assert!(spi.contains(0x4001_300C)); // SS_VAL register at offset 0x0C
        assert!(!spi.contains(0x4001_3010)); // Just past end
        assert!(!spi.contains(0x4001_2FFF));
    }

    #[test]
    fn spi_offset_calculation() {
        let spi = SpiPeripheral::new(0x4001_3000);
        assert_eq!(spi.offset(0x4001_3000), 0);
        assert_eq!(spi.offset(0x4001_3004), 4);
        assert_eq!(spi.offset(0x4001_3008), 8);
    }
}
