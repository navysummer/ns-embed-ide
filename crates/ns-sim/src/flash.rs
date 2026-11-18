//! Virtual Flash memory controller for ns-sim.
//!
//! Models a memory-mapped flash region supporting:
//! - Program/erase operations with simulated timing
//! - Read-only execution from flash
//! - Optional write-protect per sector
//!
//! This is primarily useful for simulating firmware that reads
//! from flash during execution (e.g., vector table, constant data).

use serde::Serialize;

/// Flash sector configuration.
#[derive(Debug, Clone, Serialize)]
pub struct FlashSector {
    /// Start address of this sector.
    pub start: u32,
    /// Size in bytes.
    pub size: u32,
    /// Whether this sector is write-protected.
    pub protected: bool,
}

impl FlashSector {
    pub fn contains(&self, addr: u32) -> bool {
        addr >= self.start && addr < self.start + self.size
    }

    pub fn offset(&self, addr: u32) -> u32 {
        addr.wrapping_sub(self.start)
    }
}

/// Flash memory peripheral state.
#[derive(Debug, Clone, Serialize)]
pub struct FlashPeripheral {
    /// Base address of flash region.
    pub base: u32,
    /// Total flash size.
    pub size: u32,
    /// Sectors in this flash bank.
    pub sectors: Vec<FlashSector>,
    /// Data stored in flash (simulated).
    pub data: Vec<u8>,
    /// Status register bits.
    pub sr_ready: bool,
    pub sr_pgerr: bool,   // Programming error
    pub sr_strberr: bool, // Strip error
    pub sr_pe: bool,      // Programming error
    pub sr_pgs: bool,     // Programming ongoing
}

impl FlashPeripheral {
    pub fn new(base: u32, size: u32, num_sectors: usize, sector_size: u32) -> Self {
        let mut sectors = Vec::new();
        for i in 0..num_sectors {
            sectors.push(FlashSector {
                start: base + i as u32 * sector_size,
                size: sector_size,
                protected: i == 0, // Protect boot sector by default
            });
        }
        Self {
            base,
            size,
            sectors,
            data: vec![0; size as usize],
            sr_ready: true,
            sr_pgerr: false,
            sr_strberr: false,
            sr_pe: false,
            sr_pgs: false,
        }
    }

    /// Read a byte from flash memory.
    pub fn read_byte(&self, address: u32) -> Option<u8> {
        if address < self.base || address >= self.base + self.size {
            return None;
        }
        let offset = (address - self.base) as usize;
        Some(self.data.get(offset).copied().unwrap_or(0xFF))
    }

    /// Read a 32-bit word from flash.
    pub fn read_word(&self, address: u32) -> Option<u32> {
        let mut val = 0u32;
        for i in 0..4 {
            match self.read_byte(address + i) {
                Some(b) => val |= (b as u32) << (i * 8),
                None => return None,
            }
        }
        Some(val)
    }

    /// Write a byte to flash (simulated, requires erase first in real HW).
    pub fn write_byte(&mut self, address: u32, byte: u8) -> Result<(), &'static str> {
        if address < self.base || address >= self.base + self.size {
            return Err("Address out of flash range");
        }
        // Check sector protection
        for sector in &self.sectors {
            if sector.contains(address) && sector.protected {
                return Err("Sector is write-protected");
            }
        }
        let offset = (address - self.base) as usize;
        if offset < self.data.len() {
            self.data[offset] = byte;
            Ok(())
        } else {
            Err("Offset out of bounds")
        }
    }

    /// Erase a sector (sets all bytes to 0xFF).
    pub fn erase_sector(&mut self, sector_idx: usize) -> Result<(), &'static str> {
        if sector_idx >= self.sectors.len() {
            return Err("Invalid sector index");
        }
        let sector = &self.sectors[sector_idx];
        if sector.protected {
            return Err("Sector is write-protected");
        }
        let start = (sector.start - self.base) as usize;
        let end = start + sector.size as usize;
        if end <= self.data.len() {
            // Flash erases to 0xFF (all bits set)
            for b in &mut self.data[start..end] {
                *b = 0xFF;
            }
            Ok(())
        } else {
            Err("Sector extends beyond flash size")
        }
    }

    /// Check if an address falls within flash.
    pub fn contains(&self, address: u32) -> bool {
        address >= self.base && address < self.base + self.size
    }

    /// Write a 32-bit word to flash (in Little-Endian).
    pub fn write_word(&mut self, address: u32, value: u32) -> Result<(), &'static str> {
        let bytes = [
            (value & 0xFF) as u8,
            ((value >> 8) & 0xFF) as u8,
            ((value >> 16) & 0xFF) as u8,
            ((value >> 24) & 0xFF) as u8,
        ];
        for i in 0..4 {
            self.write_byte(address + i, bytes[i as usize])?;
        }
        Ok(())
    }

    /// Get offset from flash base.
    pub fn offset(&self, address: u32) -> u32 {
        address.wrapping_sub(self.base)
    }

    /// Get status register value.
    pub fn get_sr(&self) -> u32 {
        let mut reg = 0u32;
        if self.sr_ready { reg |= 1 << 0; }
        if self.sr_pgerr { reg |= 1 << 2; }
        if self.sr_strberr { reg |= 1 << 4; }
        if self.sr_pe { reg |= 1 << 5; }
        if self.sr_pgs { reg |= 1 << 6; }
        reg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flash_new_initializes_correctly() {
        let flash = FlashPeripheral::new(0x0800_0000, 0x10000, 4, 0x1000);
        assert_eq!(flash.sectors.len(), 4);
        assert_eq!(flash.data.len(), 0x10000);
        assert!(flash.sectors[0].protected); // Boot sector protected
        assert!(!flash.sectors[1].protected);
    }

    #[test]
    fn flash_read_write_byte() {
        let mut flash = FlashPeripheral::new(0x0800_0000, 0x100, 1, 0x100);
        
        // Sector 0 is protected by default - write to offset within it
        // Actually sector 0 covers 0x0800_0000..0x0800_00FF and is protected
        // Let's unprotect it for this test by creating without protection
        let mut flash = FlashPeripheral::new(0x0800_0000, 0x100, 1, 0x100);
        flash.sectors[0].protected = false;
        
        // Write a byte
        flash.write_byte(0x0800_0010, 0x42).unwrap();
        assert_eq!(flash.read_byte(0x0800_0010), Some(0x42));
        
        // Read word
        assert_eq!(flash.read_word(0x0800_0010), Some(0x42));
    }

    #[test]
    fn flash_protected_sector_cannot_write() {
        let mut flash = FlashPeripheral::new(0x0800_0000, 0x100, 1, 0x100);
        assert!(flash.write_byte(0x0800_0000, 0x01).is_err());
    }

    #[test]
    fn flash_erase_sector() {
        let _flash = FlashPeripheral::new(0x0800_0000, 0x100, 2, 0x80);
        // Unprotect both sectors for this test
        for s in &mut flash.sectors {
            s.protected = false;
        }
        
        // Write some data to sector 1 (address 0x80)
        flash.write_byte(0x0800_0080, 0xAB).unwrap();
        assert_eq!(flash.read_byte(0x0800_0080), Some(0xAB));
        
        // Erase sector 1
        flash.erase_sector(1).unwrap();
        assert_eq!(flash.read_byte(0x0800_0080), Some(0xFF));
    }

    #[test]
    fn flash_contains_range() {
        let flash = FlashPeripheral::new(0x0800_0000, 0x10000, 4, 0x1000);
        assert!(flash.contains(0x0800_0000));
        assert!(flash.contains(0x0800_FFFF));
        assert!(!flash.contains(0x07FF_FFFF));
        assert!(!flash.contains(0x0810_0000));
    }

    #[test]
    fn flash_out_of_bounds_write_fails() {
        let mut flash = FlashPeripheral::new(0x0800_0000, 0x100, 1, 0x100);
        assert!(flash.write_byte(0x0800_0100, 0x01).is_err());
        assert!(flash.write_byte(0x07FF_FFFF, 0x01).is_err());
    }
}
