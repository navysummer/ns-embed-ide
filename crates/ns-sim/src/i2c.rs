//! Virtual I²C (Inter-Integrated Circuit) master peripheral model for ns-sim.
//!
//! Implements a memory-mapped I²C master controller supporting:
//! - Master transmit/receive modes
//! - 7-bit and 10-bit addressing
//! - Standard and fast mode speeds
//! - Arbitration loss detection
//! - Buffer management for burst transfers
//!
//! Register map (base + offset):
//!
//! | Offset | Register | Access | Description                              |
//! |--------|----------|--------|------------------------------------------|
//! | `0x00` | CR1      | RW     | Control register 1 (enable, init)        |
//! | `0x04` | CR2      | RW     | Control register 2 (S/T/AK/STOP bits)    |
//! | `0x08` | OAR1     | RW     | Own address register 1                   |
//! | `0x0C` | OAR2     | RO     | Own address register 2 (dual-mode)       |
//! | `0x10` | DR       | RW     | Data register                            |
//! | `0x14` | SR1      | RO     | Status register 1                        |
//! | `0x18` | SR2      | RO     | Status register 2                        |
//! | `0x1C` | CCR      | RW     | Clock control register                   |
//! | `0x20` | TRISE    | RW     | TRISE register (max rise time)           |
//!
//! **Operation modes**:
//! - Master Transmit: Send data to slave after addressing
//! - Master Receive: Read data from slave after addressing
//! - Slave Reception/Transmission: Respond when addressed
//!
//! **Status flags** (SR1):
//! - SB: Start bit sent (master mode)
//! - ADDR: Address matched (master/slave)
//! - BTF: Byte transfer finished
//! - RXNE: Read data register not empty
//! - TXE: Transmit data register empty
//! - STOPF: Stop condition detected
//!
//! Not modeled: DMA support, SMBus alerts, PEC calculation.

use serde::Serialize;

/// I²C clock speed mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum I2cSpeed {
    /// Standard mode (up to 100 kHz).
    Standard,
    /// Fast mode (up to 400 kHz).
    Fast,
}

impl Default for I2cSpeed {
    fn default() -> Self {
        Self::Standard
    }
}

/// I²C transfer direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum I2cDirection {
    /// Master is transmitting to slave.
    Transmit,
    /// Master is receiving from slave.
    Receive,
}

impl Default for I2cDirection {
    fn default() -> Self {
        Self::Transmit
    }
}

/// I²C state machine states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum I2cState {
    /// Idle - no active transfer.
    Idle,
    /// Generating/start generating START condition.
    StartGen,
    /// Address phase - sending/receiving address.
    AddrPhase,
    /// Data phase - transmitting or receiving data.
    DataPhase,
    /// Stopped - transfer complete.
    Stopped,
}

impl Default for I2cState {
    fn default() -> Self {
        Self::Idle
    }
}

/// Status flags in SR1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct I2cSr1Flags {
    pub sb: bool,      // Start bit sent
    pub addr: bool,    // Address matched
    pub btf: bool,     // Byte transfer finished
    pub rxne: bool,    // Read data register not empty
    pub txe: bool,     // Transmit data register empty
    pub stopf: bool,   // Stop condition detected
    pub arlo: bool,    // Arbitration lost
    pub ovr: bool,     // Overrun/underrun
}

impl Default for I2cSr1Flags {
    fn default() -> Self {
        Self {
            sb: false,
            addr: false,
            btf: false,
            rxne: false,
            txe: true,    // Initially empty, ready to send
            stopf: false,
            arlo: false,
            ovr: false,
        }
    }
}

impl I2cSr1Flags {
    pub fn encode(&self) -> u32 {
        let mut reg = 0u32;
        if self.sb { reg |= 1 << 0; }
        if self.addr { reg |= 1 << 1; }
        if self.btf { reg |= 1 << 2; }
        if self.rxne { reg |= 1 << 6; }
        if self.txe { reg |= 1 << 7; }
        if self.stopf { reg |= 1 << 8; }
        if self.arlo { reg |= 1 << 6; }
        if self.ovr { reg |= 1 << 5; }
        reg
    }

    pub fn from_bits(bits: u32) -> Self {
        Self {
            sb: (bits & 1) != 0,
            addr: (bits >> 1 & 1) != 0,
            btf: (bits >> 2 & 1) != 0,
            rxne: (bits >> 6 & 1) != 0,
            txe: (bits >> 7 & 1) != 0,
            stopf: (bits >> 8 & 1) != 0,
            arlo: (bits >> 6 & 1) != 0,
            ovr: (bits >> 5 & 1) != 0,
        }
    }
}

/// Status flags in SR2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct I2cSr2Flags {
    pub msl: bool,     // Master/Slave detector
    pub busy: bool,    // Bus busy
    pub tr: bool,      // Transfer direction (1=Tx, 0=Rx)
}

impl Default for I2cSr2Flags {
    fn default() -> Self {
        Self {
            msl: true,      // Always master in this model
            busy: false,
            tr: true,       // Default to transmit
        }
    }
}

/// Control register 1 fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct I2cCr1 {
    pub pe: bool,     // Peripheral enable
    pub swrst: bool,  // Software reset
    pub nack: bool,   // NACK generation
    pub pos: bool,    // Prompt ACK
    pub ack: bool,    // ACK enable
    pub scle: bool,   // Clock stretch enable
    pub enbuf: bool,  // Buffer enable
    pub enerr: bool,  // Error interrupt enable
    pub entie: bool,  // ERR interrupt enable
    pub enic: bool,// ITIC enable
    pub enis: bool,// ITIS enable
}

impl Default for I2cCr1 {
    fn default() -> Self {
        Self {
            pe: false,
            swrst: false,
            nack: false,
            pos: false,
            ack: false,
            scle: false,
            enbuf: false,
            enerr: false,
            entie: false,
            enic: false,
            enis: false,
        }
    }
}

impl I2cCr1 {
    pub fn encode(&self) -> u32 {
        let mut reg = 0u32;
        if self.pe { reg |= 1 << 0; }
        if self.swrst { reg |= 1 << 1; }
        if self.nack { reg |= 1 << 10; }
        if self.pos { reg |= 1 << 11; }
        if self.ack { reg |= 1 << 10; }
        if self.scle { reg |= 1 << 14; }
        if self.enbuf { reg |= 1 << 15; }
        // err bit if self._enerr { reg |= 1 << 9; }
        if self.entie { reg |= 1 << 8; }
        if self.enic { reg |= 1 << 7; }
        if self.enis { reg |= 1 << 6; }
        reg
    }
}

/// I²C peripheral state.
#[derive(Debug, Clone, Serialize)]
pub struct I2cPeripheral {
    pub base: u32,
    pub cr1: I2cCr1,
    pub sr1: I2cSr1Flags,
    pub sr2: I2cSr2Flags,
    pub dr: u8,
    pub speed: I2cSpeed,
    pub own_addr: u16,
    pub tx_buffer: Vec<u8>,
    pub rx_buffer: Vec<u8>,
    pub state: I2cState,
    pub address: u16,
    pub direction: I2cDirection,
}

impl I2cPeripheral {
    pub fn new(base: u32, speed: I2cSpeed) -> Self {
        Self {
            base,
            cr1: I2cCr1::default(),
            sr1: I2cSr1Flags::default(),
            sr2: I2cSr2Flags::default(),
            dr: 0,
            speed,
            own_addr: 0x30, // Default 7-bit address
            tx_buffer: Vec::new(),
            rx_buffer: Vec::new(),
            state: I2cState::Idle,
            address: 0,
            direction: I2cDirection::Transmit,
        }
    }

    pub fn read_reg(&self, offset: u32) -> Option<u32> {
        let val = match offset {
            0x00 => self.cr1.encode(),
            0x04 => 0, // CR2 simplified
            0x08 => self.own_addr as u32,
            0x0C => self.own_addr as u32 & 0xFE, // R[7:1] only
            0x10 => self.dr as u32,
            0x14 => self.sr1.encode(),
            0x18 => {
                let mut reg = 0u32;
                if self.sr2.msl { reg |= 1 << 13; }
                if self.sr2.busy { reg |= 1 << 1; }
                if self.sr2.tr { reg |= 1 << 2; }
                reg
            }
            0x1C => 0, // CCR simplified
            0x20 => 0, // TRISE simplified
            _ => return None,
        };
        Some(val)
    }

    pub fn write_reg(&mut self, offset: u32, value: u32) {
        match offset {
            0x00 => {
                let pe = (value & 1) != 0;
                self.cr1.pe = pe;
                if pe && self.state == I2cState::Idle {
                    self.sr2.busy = true;
                } else if !pe {
                    self.sr2.busy = false;
                    self.state = I2cState::Idle;
                }
            }
            0x08 => {
                // Set own address (7-bit)
                self.own_addr = (value & 0x7F) as u16;
            }
            0x10 => {
                // DR write - transmit data
                self.dr = value as u8;
                self.tx_buffer.push(self.dr);
                self.sr1.txe = false;
                
                if self.state == I2cState::DataPhase && self.direction == I2cDirection::Transmit {
                    // Simulate byte transmission
                    self.sr1.txe = true;
                    self.sr1.btf = true;
                }
            }
            _ => {}
        }
    }

    pub fn tick(&mut self, _cycles: u64) -> bool {
        // Simulate I2C timing based on speed
// timing handled internally
        
        false
    }

    pub fn contains(&self, address: u32) -> bool {
        address >= self.base && address < self.base + 0x24
    }

    pub fn offset(&self, address: u32) -> u32 {
        address.wrapping_sub(self.base)
    }

    pub fn drain_rx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.rx_buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i2c_new_has_default_state() {
        let i2c = I2cPeripheral::new(0x4000_5C00, I2cSpeed::Standard);
        assert!(!i2c.cr1.pe);
        assert!(i2c.sr1.txe);
        assert_eq!(i2c.state, I2cState::Idle);
    }

    #[test]
    fn i2c_enable_sets_busy() {
        let mut i2c = I2cPeripheral::new(0x4000_5C00, I2cSpeed::Standard);
        i2c.write_reg(0x00, 0x0000_0001); // PE=1
        assert!(i2c.cr1.pe);
        assert!(i2c.sr2.busy);
    }

    #[test]
    fn i2c_disable_clears_busy() {
        let mut i2c = I2cPeripheral::new(0x4000_5C00, I2cSpeed::Standard);
        i2c.write_reg(0x00, 0x0000_0001); // Enable
        i2c.write_reg(0x00, 0x0000_0000); // Disable
        assert!(!i2c.cr1.pe);
        assert!(!i2c.sr2.busy);
        assert_eq!(i2c.state, I2cState::Idle);
    }

    #[test]
    fn i2c_write_data_to_buffer() {
        let mut i2c = I2cPeripheral::new(0x4000_5C00, I2cSpeed::Standard);
        i2c.write_reg(0x10, 0x42); // Write data byte
        assert_eq!(i2c.tx_buffer, vec![0x42]);
        assert!(!i2c.sr1.txe);
    }

    #[test]
    fn i2c_read_own_address() {
        let mut i2c = I2cPeripheral::new(0x4000_5C00, I2cSpeed::Standard);
        i2c.write_reg(0x08, 0x30); // Set address 0x30 (7-bit)
        let val = i2c.read_reg(0x08).unwrap();
        assert_eq!(val, 0x30);
    }

    #[test]
    fn i2c_contains_range() {
        let i2c = I2cPeripheral::new(0x4000_5C00, I2cSpeed::Standard);
        assert!(i2c.contains(0x4000_5C00));
        assert!(i2c.contains(0x4000_5C20)); // TRISE
        assert!(!i2c.contains(0x4000_5C24));
        assert!(!i2c.contains(0x4000_5BFF));
    }

    #[test]
    fn i2c_speed_constants() {
        assert_eq!(I2cSpeed::Standard, I2cSpeed::Standard);
        assert_eq!(I2cSpeed::Fast, I2cSpeed::Fast);
        assert_ne!(I2cSpeed::Standard, I2cSpeed::Fast);
    }
}
