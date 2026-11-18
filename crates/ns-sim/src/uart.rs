//! Virtual UART (Universal Asynchronous Receiver/Transmitter) peripheral model.
//!
//! Supports full-duplex asynchronous serial communication with:
//! - Configurable baud rate (via divisor)
//! - 5-8 data bits, no/odd/even/forced parity, 1-2 stop bits
//! - Interrupt on receive/data register empty/transmit complete
//! - FIFO support for burst transfers

use serde::Serialize;

/// Parity mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum UartParity {
    None,
    Even,
    Odd,
    One,   // forced to 1
    Zero,  // forced to 0
}

impl Default for UartParity {
    fn default() -> Self {
        Self::None
    }
}

/// UART status flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct UartSrFlags {
    pub dr: bool,     // Data Ready (receiver not empty)
    pub thre: bool,   // Transmitter Holding Register Empty
    pub teng: bool,   // Transmitter Empty
    pub bri: bool,    // Break Receive Interrupt
    pub te: bool,     // Timeout Error
    pub pe: bool,     // Parity Error
    pub fe: bool,     // Framing Error
    pub oe: bool,     // Overrun Error
}

impl Default for UartSrFlags {
    fn default() -> Self {
        Self {
            dr: false,
            thre: true,   // Initially empty, ready to send
            teng: true,   // Initially empty
            bri: false,
            te: false,
            pe: false,
            fe: false,
            oe: false,
        }
    }
}

impl UartSrFlags {
    pub fn encode(&self) -> u32 {
        let mut reg = 0u32;
        if self.dr { reg |= 1 << 0; }
        if self.thre { reg |= 1 << 5; }
        if self.teng { reg |= 1 << 6; }
        if self.bri { reg |= 1 << 3; }
        if self.te { reg |= 1 << 2; }
        if self.pe { reg |= 1 << 1; }
        if self.fe { reg |= 1 << 4; }
        if self.oe { reg |= 1 << 7; }
        reg
    }
}

/// UART line control register fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct UartLcr {
    pub wb5: bool,   // Word Length 5 bits
    pub wb6: bool,   // Word Length 6 bits  
    pub wb7: bool,   // Word Length 7 bits
    pub wb8: bool,   // Word Length 8 bits (default)
    pub sp: bool,    // Stick Parity
    pub eps: bool,   // Even Parity Select
    pub pen: bool,   // Parity Enable
    pub sts: bool,   // Stop Bits (0=1, 1=2)
    pub brk: bool,   // Break Control
}

impl Default for UartLcr {
    fn default() -> Self {
        Self {
            wb5: false, wb6: false, wb7: false, wb8: true,
            sp: false, eps: false, pen: false, sts: false, brk: false,
        }
    }
}

impl UartLcr {
    pub fn data_bits(&self) -> u8 {
        if self.wb8 { 8 } else if self.wb7 { 7 } else if self.wb6 { 6 } else { 5 }
    }

    pub fn parity(&self) -> UartParity {
        if !self.pen { return UartParity::None; }
        match (self.eps, self.sp) {
            (false, false) => UartParity::Odd,
            (true, false) => UartParity::Even,
            (false, true) => UartParity::One,
            (true, true) => UartParity::Zero,
        }
    }
}

/// UART interrupt enable register fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct UartIer {
    pub rbui: bool, // RX Buffer Full Interrupt Enable
    pub thoie: bool,// TX Holding Register Empty IE
    pub rbei: bool, // RX Buffer Full Interrupt Enable
    pub rtoie: bool,// RX Timeout Interrupt Enable
    pub lipsie: bool,// Line Status Interrupt Enable
    pub mie: bool,  // Modem Interrupt Enable
}

impl Default for UartIer {
    fn default() -> Self {
        Self {
            rbui: false, thoie: false, rbei: false,
            rtoie: false, lipsie: false, mie: false,
        }
    }
}

/// Virtual UART peripheral.
#[derive(Debug, Clone)]
pub struct UartPeripheral {
    pub base: u32,
    pub ier: UartIer,
    pub iir: u8,       // Interrupt Identification Register (read-only, cleared on read)
    pub lcr: UartLcr,
    pub mcr: u8,       // Modem Control Register
    pub lsr: UartSrFlags,
    pub msr: u8,       // Modem Status Register
    pub scr: u8,       // Scratch register
    pub div: u16,      // Baud rate divisor
    /// Received bytes waiting to be read
    pub rx_buffer: Vec<u8>,
    /// Transmitted bytes
    pub tx_buffer: Vec<u8>,
}

impl UartPeripheral {
    pub fn new(base: u32) -> Self {
        Self {
            base,
            ier: UartIer::default(),
            iir: 0,
            lcr: UartLcr::default(),
            mcr: 0,
            lsr: UartSrFlags::default(),
            msr: 0,
            scr: 0,
            div: 1,
            rx_buffer: Vec::new(),
            tx_buffer: Vec::new(),
        }
    }

    pub fn peek_rx(&self) -> Option<u8> {
        self.rx_buffer.first().copied()
    }

    pub fn drain_rx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.rx_buffer)
    }

    pub fn inject_tx(&mut self, byte: u8) {
        self.tx_buffer.push(byte);
    }

    pub fn read_reg(&self, offset: u32) -> Option<u32> {
        let val = match offset {
            0x00 => {
                // RBR / DLL (Divisor Latch Low when DLAB=1)
                if self.lcr.brk { // DLAB bit is bit 7
                    self.div as u32
                } else if let Some(b) = self.rx_buffer.first() {
                    *b as u32
                } else {
                    0
                }
            }
            0x04 => {
                // THR / DLM (Divisor Latch High when DLAB=1)
                if self.lcr.brk {
                    (self.div >> 8) as u32
                } else {
                    0
                }
            }
            0x08 => self.iir as u32, // IIR (read clears interrupts)
            0x0C => self.lcr.encode() as u32, // LCR
            0x10 => self.mcr as u32, // MCR
            0x14 => self.lsr.encode(), // LSR
            0x18 => self.msr as u32, // MSR
            0x1C => self.scr as u32, // SCR
            _ => return None,
        };
        Some(val)
    }

    pub fn write_reg(&mut self, offset: u32, value: u32) {
        match offset {
            0x00 => {
                // RBR write = THR write (transmit)
                if self.lcr.brk { // DLAB set
                    self.div = ((self.div & 0xFF00) | (value & 0xFF) as u16);
                } else {
                    self.inject_tx(value as u8);
                    self.lsr = UartSrFlags { thre: false, teng: false, ..self.lsr };
                }
            }
            0x04 => {
                if self.lcr.brk {
                    self.div = ((self.div & 0x00FF) | ((value & 0xFF) as u16) << 8);
                }
            }
            0x08 => {
                // IIR write clears interrupt flags
                self.lsr = UartSrFlags::default();
                self.lsr.thre = true;
                self.lsr.teng = true;
            }
            0x0C => {
                // LCR write
                let new_lcr = UartLcr {
                    wb5: (value & 1) != 0,
                    wb6: (value >> 1 & 1) != 0,
                    wb7: (value >> 2 & 1) != 0,
                    wb8: (value >> 3 & 1) != 0,
                    sp: (value >> 4 & 1) != 0,
                    eps: (value >> 5 & 1) != 0,
                    pen: (value >> 6 & 1) != 0,
                    sts: (value >> 7 & 1) != 0,
                    brk: (value >> 8 & 1) != 0, // DLAB in some implementations
                };
                self.lcr = new_lcr;
            }
            0x10 => {
                self.mcr = value as u8;
            }
            0x14 => {
                // Write to LSR clears specific flags
                if value & 1 != 0 { self.lsr.dr = false; }
                if value & (1 << 5) != 0 { self.lsr.thre = false; }
                if value & (1 << 6) != 0 { self.lsr.teng = false; }
            }
            0x1C => {
                self.scr = value as u8;
            }
            _ => {}
        }
    }

    /// Simulate receiving a byte (inject from external source).
    pub fn rx_byte(&mut self, byte: u8) {
        self.rx_buffer.push(byte);
        self.lsr.dr = true;
    }

    /// Check if there's space in transmitter buffer.
    pub fn can_transmit(&self) -> bool {
        self.lsr.thre || self.lsr.teng
    }

    pub fn contains(&self, address: u32) -> bool {
        address >= self.base && address < self.base + 0x20
    }

    pub fn offset(&self, address: u32) -> u32 {
        address.wrapping_sub(self.base)
    }
}

impl UartLcr {
    pub fn encode(&self) -> u32 {
        let mut reg = 0u32;
        if self.wb5 { reg |= 1 << 0; }
        if self.wb6 { reg |= 1 << 1; }
        if self.wb7 { reg |= 1 << 2; }
        if self.wb8 { reg |= 1 << 3; }
        if self.sp { reg |= 1 << 4; }
        if self.eps { reg |= 1 << 5; }
        if self.pen { reg |= 1 << 6; }
        if self.sts { reg |= 1 << 7; }
        if self.brk { reg |= 1 << 8; } // DLAB
        reg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uart_new_has_default_state() {
        let uart = UartPeripheral::new(0x4000_4000);
        assert!(!uart.ier.rbui);
        assert_eq!(uart.rx_buffer.len(), 0);
        assert_eq!(uart.tx_buffer.len(), 0);
    }

    #[test]
    fn uart_write_to_thr_injects_tx() {
        let mut uart = UartPeripheral::new(0x4000_4000);
        uart.write_reg(0x00, 0x42); // Write to THR
        assert_eq!(uart.tx_buffer, vec![0x42]);
        assert!(!uart.lsr.thre);
    }

    #[test]
    fn uart_read_from_rbr_gets_rx() {
        let mut uart = UartPeripheral::new(0x4000_4000);
        uart.rx_byte(0xFF);
        let val = uart.read_reg(0x00).unwrap();
        assert_eq!(val, 0xFF);
    }

    #[test]
    fn uart_set_baud_divisor() {
        let mut uart = UartPeripheral::new(0x4000_4000);
        uart.lcr.brk = true; // Set DLAB
        uart.write_reg(0x00, 0x01); // DLL = 1
        uart.write_reg(0x04, 0x00); // DLM = 0
        uart.lcr.brk = false; // Clear DLAB
        assert_eq!(uart.div, 1);
    }

    #[test]
    fn uart_contains_range() {
        let uart = UartPeripheral::new(0x4000_4000);
        assert!(uart.contains(0x4000_4000));
        assert!(uart.contains(0x4000_401C)); // SCR at +0x1C
        assert!(!uart.contains(0x4000_4020));
    }

    #[test]
    fn uart_offset_calculation() {
        let uart = UartPeripheral::new(0x4000_4000);
        assert_eq!(uart.offset(0x4000_4000), 0);
        assert_eq!(uart.offset(0x4000_4008), 8);
        assert_eq!(uart.offset(0x4000_4010), 0x10);
    }
}
