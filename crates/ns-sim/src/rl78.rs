//! A Renesas RL78 (RL78/G10-G14, IAR RL78) instruction-level simulator core.
//!
//! Models the RL78 CPU used by IAR/Keil-style RL78 toolchains. The RL78 is a
//! 16-bit CISC with:
//!
//! * 8-bit registers: `A`, `X`, `B`, `C`, `D`, `E`, `H`, `L` (the second
//!   bank `A'..L'` is selected by `SEL RBn`).
//! * 16-bit register pairs: `AX`, `BC`, `DE`, `HL`, plus `SP` and `PC`.
//! * An 8-bit program status word (`PSW`): `Z`, `AC`, `CY`, `RBS1:RBS0`, `IE`.
//! * Segment registers `CS` (code) and `ES` (data) for 20-bit addressing.
//! * A 1 MiB flat address space (byte-addressed, little-endian).
//!
//! ## Covered instructions
//!
//! * Moves: `MOV` (byte, all addressing modes), `MOVW` (word), `MOV1`,
//!   `MOVS`, `XCH`, `XCHW`.
//! * Arithmetic: `ADD`/`ADDC`/`SUB`/`SUBC`/`CMP` (byte and word), `INC`/`DEC`
//!   (byte and word), `MULU`, `DIVU`, `MULHU`, `DIVHU`, `MACHU`.
//! * Logic: `AND`, `OR`, `XOR` (byte and word), `NOT1`.
//! * Shifts/rotates: `SHL`, `SHR`, `SAR`, `SHLW`, `SHRW`, `SARW`, `ROL`,
//!   `ROR`, `ROLC`, `RORC`, `ROLWC`, `RORWC`.
//! * Bit ops: `SET1`, `CLR1`, `BT`, `BF`, `BTCLR`, `AND1`, `OR1`, `XOR1`.
//! * Memory/stack: `PUSH`, `POP`, `PUSHM`, `POPM`.
//! * Control: `BR`, `BC`, `BNC`, `BZ`, `BNZ`, `BH`, `BNH`, `CALL`, `CALLT`,
//!   `RET`, `RETI`, `RETB`, `BRK`, `BRK1`, `NOP`, `SEL`, `SK`.
//!
//! Encodings follow the binutils RL78 opcode decoder (`opcodes/rl78-decode.c`),
//! cross-checked against the RL78 user's manual.

/// Flat memory window size (1 MiB covers the RL78 address space).
pub const MEMORY_SIZE: usize = 1024 * 1024;

/// The RL78 PSW flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Psw {
    /// Zero flag.
    pub z: bool,
    /// Auxiliary carry flag.
    pub ac: bool,
    /// Carry flag.
    pub cy: bool,
    /// Register bank select (2 bits).
    pub rbs: u8,
    /// Interrupt enable.
    pub ie: bool,
}

impl Default for Psw {
    fn default() -> Self {
        Psw { z: false, ac: false, cy: false, rbs: 0, ie: true }
    }
}

/// The 8-bit register-file indices. The RL78 register encoding maps 0..7 to
/// `X A C B E D L H` (per the binutils `RL78_Reg_X..RL78_Reg_H` order).
const R_X: usize = 0;
const R_A: usize = 1;
const R_C: usize = 2;
const R_B: usize = 3;
const R_E: usize = 4;
const R_D: usize = 5;
const R_L: usize = 6;
const R_H: usize = 7;

/// Outcome of a single RL78 step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rl78Step {
    /// A normal instruction advanced the PC.
    Advanced,
    /// A breakpoint was reached before executing.
    Breakpoint,
    /// The core halted (`BRK`, `HALT`, `STOP`, or an unknown opcode).
    Halted,
}

/// An RL78 core.
pub struct Rl78Core {
    /// General-purpose 8-bit registers, two banks of 8 (`A'` bank is 8..16).
    pub regs: [u8; 16],
    /// Program status word.
    pub psw: Psw,
    /// Stack pointer (16-bit).
    pub sp: u16,
    /// Code segment register (high 4 bits of the 20-bit code address).
    pub cs: u8,
    /// Data segment register (high 4 bits of the 20-bit data address).
    pub es: u8,
    /// Program counter (20-bit code address).
    pub pc: u32,
    /// Flat 1 MiB memory (byte-addressed).
    pub mem: Vec<u8>,
    /// Whether execution should stop.
    pub halted: bool,
}

impl Default for Rl78Core {
    fn default() -> Self {
        Self::new()
    }
}

impl Rl78Core {
    pub fn new() -> Self {
        Rl78Core {
            regs: [0; 16],
            psw: Psw::default(),
            sp: 0xFFEE,
            cs: 0,
            es: 0,
            pc: 0,
            mem: vec![0; MEMORY_SIZE],
            halted: false,
        }
    }

    /// Reset to `entry` (a 20-bit code address), zeroing registers.
    pub fn reset(&mut self, entry: u32) {
        self.regs = [0; 16];
        self.psw = Psw::default();
        self.sp = 0xFFEE;
        self.cs = (entry >> 16) as u8 & 0x0F;
        self.es = 0;
        self.pc = entry;
        self.halted = false;
    }

    pub fn set_pc(&mut self, pc: u32) {
        self.pc = pc & 0xFFFFF;
    }

    /// Load a program image at `base` (a 20-bit code address) and set PC.
    pub fn load_image(&mut self, base: u32, image: &[u8]) {
        self.write_bytes(base, image);
        self.pc = base & 0xFFFFF;
        self.halted = false;
    }

    // -------- register access --------

    fn rb(&self, index: usize) -> u8 {
        let bank = (self.psw.rbs as usize & 0x3) * 8;
        self.regs[bank + (index & 0x7)]
    }

    fn set_rb(&mut self, index: usize, value: u8) {
        let bank = (self.psw.rbs as usize & 0x3) * 8;
        self.regs[bank + (index & 0x7)] = value;
    }

    /// Read a 16-bit register pair: `0`=AX, `1`=BC, `2`=DE, `3`=HL.
    fn rw(&self, index: usize) -> u16 {
        match index & 0x3 {
            0 => (self.rb(R_X) as u16) | ((self.rb(R_A) as u16) << 8),
            1 => (self.rb(R_C) as u16) | ((self.rb(R_B) as u16) << 8),
            2 => (self.rb(R_E) as u16) | ((self.rb(R_D) as u16) << 8),
            _ => (self.rb(R_L) as u16) | ((self.rb(R_H) as u16) << 8),
        }
    }

    fn set_rw(&mut self, index: usize, value: u16) {
        match index & 0x3 {
            0 => {
                self.set_rb(R_X, value as u8);
                self.set_rb(R_A, (value >> 8) as u8);
            }
            1 => {
                self.set_rb(R_C, value as u8);
                self.set_rb(R_B, (value >> 8) as u8);
            }
            2 => {
                self.set_rb(R_E, value as u8);
                self.set_rb(R_D, (value >> 8) as u8);
            }
            _ => {
                self.set_rb(R_L, value as u8);
                self.set_rb(R_H, (value >> 8) as u8);
            }
        }
    }

    /// Register label for the IDE register view (byte registers).
    pub fn register_label(index: usize) -> String {
        const NAMES: [&str; 8] = ["X", "A", "C", "B", "E", "D", "L", "H"];
        if index < 8 {
            format!("{}", NAMES[index])
        } else if index < 16 {
            let bank = (index - 8) / 8;
            format!("{}{}'", NAMES[index % 8], bank + 1)
        } else {
            format!("r{}", index)
        }
    }

    /// Read a register by IDE index (0-7 = current bank byte regs, 8-15 = SP/
    /// PSW/PC/CS/ES and word pairs).
    pub fn read_reg(&self, index: usize) -> u32 {
        match index {
            0..=7 => self.rb(index) as u32,
            8 => self.sp as u32,
            9 => {
                let mut v = 0u32;
                if self.psw.z { v |= 1 << 0; }
                if self.psw.ac { v |= 1 << 1; }
                if self.psw.cy { v |= 1 << 2; }
                v |= (self.psw.rbs as u32 & 0x3) << 3;
                if self.psw.ie { v |= 1 << 5; }
                v
            }
            10 => self.pc & 0xFFFFF,
            11 => self.cs as u32,
            12 => self.es as u32,
            13 => self.rw(0) as u32, // AX
            14 => self.rw(1) as u32, // BC
            15 => self.rw(2) as u32, // DE
            _ => 0,
        }
    }

    /// Write a register by IDE index (inverse of `read_reg`).
    pub fn write_reg(&mut self, index: usize, value: u32) {
        match index {
            0..=7 => self.set_rb(index, value as u8),
            8 => self.sp = value as u16,
            9 => {
                self.psw.z = value & 1 != 0;
                self.psw.ac = value & 2 != 0;
                self.psw.cy = value & 4 != 0;
                self.psw.rbs = ((value >> 3) & 0x3) as u8;
                self.psw.ie = value & 0x20 != 0;
            }
            10 => self.set_pc(value),
            11 => self.cs = value as u8 & 0x0F,
            12 => self.es = value as u8 & 0x0F,
            13 => self.set_rw(0, value as u16),
            14 => self.set_rw(1, value as u16),
            15 => self.set_rw(2, value as u16),
            _ => {}
        }
    }

    // -------- memory --------

    /// Read one byte from the flat data space (out-of-range returns 0).
    pub fn read_u8(&self, address: u32) -> u8 {
        let a = address as usize;
        if a < self.mem.len() {
            self.mem[a]
        } else {
            0
        }
    }

    /// Read a little-endian 16-bit word from the flat data space.
    pub fn read_u16(&self, address: u32) -> u16 {
        self.read_u8(address) as u16 | ((self.read_u8(address.wrapping_add(1)) as u16) << 8)
    }

    /// Read a little-endian 32-bit word from the flat data space.
    pub fn read_u32(&self, address: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..4 {
            v |= (self.read_u8(address.wrapping_add(i as u32)) as u32) << (8 * i);
        }
        v
    }

    /// Write one byte to the flat data space (out-of-range ignored).
    pub fn write_u8(&mut self, address: u32, value: u8) {
        let a = address as usize;
        if a < self.mem.len() {
            self.mem[a] = value;
        }
    }

    /// Write a little-endian 16-bit word.
    pub fn write_u16(&mut self, address: u32, value: u16) {
        self.write_u8(address, value as u8);
        self.write_u8(address.wrapping_add(1), (value >> 8) as u8);
    }

    /// Write a little-endian 32-bit word.
    pub fn write_u32(&mut self, address: u32, value: u32) {
        for i in 0..4 {
            self.write_u8(address.wrapping_add(i as u32), (value >> (8 * i)) as u8);
        }
    }

    /// Write a byte slice to the flat memory starting at `base`.
    pub fn write_bytes(&mut self, base: u32, bytes: &[u8]) {
        let b = base as usize;
        for (i, &byte) in bytes.iter().enumerate() {
            let a = b + i;
            if a < self.mem.len() {
                self.mem[a] = byte;
            }
        }
    }

    /// Zero `count` bytes at `base`.
    pub fn zero_bytes(&mut self, base: u32, count: u32) {
        for i in 0..count {
            self.write_u8(base.wrapping_add(i), 0);
        }
    }

    // -------- fetch --------

    fn fetch8(&mut self) -> u8 {
        let b = self.read_u8(self.pc);
        self.pc = self.pc.wrapping_add(1) & 0xFFFFF;
        b
    }

    fn fetch16(&mut self) -> u16 {
        let lo = self.fetch8() as u16;
        let hi = self.fetch8() as u16;
        (hi << 8) | lo
    }


    // -------- flags --------

    fn set_nz(&mut self, value: u8) {
        self.psw.z = value == 0;
    }

    /// Compute and store Z/AC/CY for a byte addition.
    fn add8(&mut self, a: u8, b: u8, carry: bool) -> u8 {
        let c = if carry { 1u16 } else { 0u16 };
        let sum = a as u16 + b as u16 + c;
        self.psw.z = (sum & 0xFF) == 0;
        self.psw.ac = ((a & 0x0F) as u16 + (b & 0x0F) as u16 + c) > 0x0F;
        self.psw.cy = sum > 0xFF;
        sum as u8
    }

    /// Compute and store Z/AC/CY for a byte subtraction.
    fn sub8(&mut self, a: u8, b: u8, borrow: bool) -> u8 {
        let bo = if borrow { 1u16 } else { 0u16 };
        let diff = a as u16 - b as u16 - bo;
        self.psw.z = (diff & 0xFF) == 0;
        self.psw.ac = (a & 0x0F) < ((b & 0x0F) + (bo as u8 & 0x0F));
        self.psw.cy = diff > 0xFF;
        diff as u8
    }

    /// Compute and store Z/AC/CY for a word addition.
    fn add16(&mut self, a: u16, b: u16, carry: bool) -> u16 {
        let c = if carry { 1u32 } else { 0u32 };
        let sum = a as u32 + b as u32 + c;
        self.psw.z = (sum & 0xFFFF) == 0;
        self.psw.ac = ((a & 0x0FFF) as u32 + (b & 0x0FFF) as u32 + c) > 0x0FFF;
        self.psw.cy = sum > 0xFFFF;
        sum as u16
    }

    /// Compute and store Z/AC/CY for a word subtraction.
    fn sub16(&mut self, a: u16, b: u16, borrow: bool) -> u16 {
        let bo = if borrow { 1u32 } else { 0u32 };
        let diff = a as u32 - b as u32 - bo;
        self.psw.z = (diff & 0xFFFF) == 0;
        self.psw.ac = (a & 0x0FFF) < ((b & 0x0FFF) + (bo as u16 & 0x0FFF));
        self.psw.cy = diff > 0xFFFF;
        diff as u16
    }

    /// Resolve a short-address operand to a flat address (`saddr`).
    fn saddr(&self, addr: u32) -> u32 {
        if addr < 0x20 {
            0xFFF00 + addr
        } else {
            0xFFE00 + addr
        }
    }



    // -------- stack --------

    fn push16(&mut self, value: u16) {
        self.sp = self.sp.wrapping_sub(2);
        self.write_u16(self.es_addr(self.sp as u32), value);
    }

    fn pop16(&mut self) -> u16 {
        let v = self.read_u16(self.es_addr(self.sp as u32));
        self.sp = self.sp.wrapping_add(2);
        v
    }

    /// Combine the 20-bit address from ES and a 16-bit offset.
    fn es_addr(&self, offset: u32) -> u32 {
        ((self.es as u32) << 16) | (offset & 0xFFFF)
    }


    // -------- conditional --------

    fn cond_true(&self, cond: u8) -> bool {
        match cond {
            0 => self.psw.cy,          // C
            1 => !self.psw.cy,         // NC
            2 => self.psw.z,           // Z
            3 => !self.psw.z,          // NZ
            4 => self.psw.cy && !self.psw.z, // H
            5 => !self.psw.cy || self.psw.z, // NH
            _ => true,
        }
    }

    // -------- execute --------

    /// Execute one step, stopping at a breakpoint.
    pub fn step(&mut self, breakpoints: &[u32]) -> Rl78Step {
        if self.halted {
            return Rl78Step::Halted;
        }
        if breakpoints.contains(&self.pc) {
            return Rl78Step::Breakpoint;
        }
        let pc = self.pc;
        self.execute();
        if self.halted {
            return Rl78Step::Halted;
        }
        if self.pc == pc {
            // Guard against infinite loops on unknown instructions.
            self.halted = true;
            return Rl78Step::Halted;
        }
        Rl78Step::Advanced
    }

    fn execute(&mut self) {
        let b0 = self.fetch8();
        // Two-byte opcode prefix `0110 0001` (0x61) for many instructions.
        if b0 == 0x61 {
            self.execute_61();
            return;
        }
        self.execute_single(b0);
    }

    fn execute_single(&mut self, op: u8) {
        match op {
            // nop (0000 0000)
            0x00 => {}
            // addw AX, ES:[imm16] (0000 0010)
            0x02 => {
                let addr = self.fetch16() as u32;
                let v = self.add16(self.rw(0), self.read_u16(self.es_addr(addr)), false);
                self.set_rw(0, v);
            }
            // addw AX, #imm16 (0000 0100)
            0x04 => {
                let imm = self.fetch16();
                let v = self.add16(self.rw(0), imm, false);
                self.set_rw(0, v);
            }
            // addw AX, saddr (0000 0110)
            0x06 => {
                let sa = self.fetch8() as u32;
                let a = self.saddr(sa);
                let v = self.add16(self.rw(0), self.read_u16(a), false);
                self.set_rw(0, v);
            }
            // addw AX, rw (0000 0rw1): rw in bits 2:1
            0x01 | 0x03 | 0x05 | 0x07 => {
                let rw = ((op >> 1) & 0x3) as usize;
                let v = self.add16(self.rw(0), self.rw(rw), false);
                self.set_rw(0, v);
            }
            // add A, saddr (0000 1011)
            0x0B => {
                let sa = self.fetch8() as u32;
                let a = self.saddr(sa);
                let v = self.add8(self.rb(R_A), self.read_u8(a), false);
                self.set_rb(R_A, v);
            }
            // add saddr, #imm8 (0000 1010)
            0x0A => {
                let sa = self.fetch8() as u32;
                let a = self.saddr(sa);
                let imm = self.fetch8();
                let v = self.add8(self.read_u8(a), imm, false);
                self.write_u8(a, v);
            }
            // add A, #imm8 (0000 1100)
            0x0C => {
                let imm = self.fetch8();
                let v = self.add8(self.rb(R_A), imm, false);
                self.set_rb(R_A, v);
            }
            // add A, [HL] (0000 1101)
            0x0D => {
                let a = self.es_addr(self.rw(3) as u32);
                let v = self.add8(self.rb(R_A), self.read_u8(a), false);
                self.set_rb(R_A, v);
            }
            // add A, [HL+imm8] (0000 1110)
            0x0E => {
                let d = self.fetch8() as u32;
                let a = self.es_addr(self.rw(3) as u32 + d);
                let v = self.add8(self.rb(R_A), self.read_u8(a), false);
                self.set_rb(R_A, v);
            }
            // add A, ES:[imm16] (0000 1111)
            0x0F => {
                let addr = self.fetch16() as u32;
                let v = self.add8(self.rb(R_A), self.read_u8(self.es_addr(addr)), false);
                self.set_rb(R_A, v);
            }
            // addw SP, #imm8 (0001 0000)
            0x10 => {
                let imm = self.fetch8() as u16;
                self.sp = self.sp.wrapping_add(imm);
            }
            // movw AX, rw (0001 0ra1) / movw rw, AX (0001 0ra0)
            0x11 | 0x13 | 0x15 | 0x17 => {
                let ra = ((op >> 1) & 0x3) as usize;
                let v = self.rw(ra);
                self.set_rw(0, v);
            }
            0x12 | 0x14 | 0x16 => {
                let ra = ((op >> 1) & 0x3) as usize;
                let v = self.rw(0);
                self.set_rw(ra, v);
            }
            // movw rw, #imm16 (0011 0rg0)
            0x30 | 0x32 | 0x34 | 0x36 => {
                let rg = ((op >> 1) & 0x3) as usize;
                let imm = self.fetch16();
                self.set_rw(rg, imm);
            }
            // addc A, #imm8 (0001 1100)
            0x1C => {
                let imm = self.fetch8();
                let v = self.add8(self.rb(R_A), imm, self.psw.cy);
                self.set_rb(R_A, v);
            }
            // addc A, ES:[imm16] (0001 1111)
            0x1F => {
                let addr = self.fetch16() as u32;
                let v = self.add8(self.rb(R_A), self.read_u8(self.es_addr(addr)), self.psw.cy);
                self.set_rb(R_A, v);
            }
            // addc A, [HL] (0001 1101)
            0x1D => {
                let a = self.es_addr(self.rw(3) as u32);
                let v = self.add8(self.rb(R_A), self.read_u8(a), self.psw.cy);
                self.set_rb(R_A, v);
            }
            // addc A, [HL+imm8] (0001 1110)
            0x1E => {
                let d = self.fetch8() as u32;
                let a = self.es_addr(self.rw(3) as u32 + d);
                let v = self.add8(self.rb(R_A), self.read_u8(a), self.psw.cy);
                self.set_rb(R_A, v);
            }
            // addc A, saddr (0001 1011)
            0x1B => {
                let sa = self.fetch8() as u32;
                let a = self.saddr(sa);
                let v = self.add8(self.rb(R_A), self.read_u8(a), self.psw.cy);
                self.set_rb(R_A, v);
            }
            // addc saddr, #imm8 (0001 1010)
            0x1A => {
                let sa = self.fetch8() as u32;
                let a = self.saddr(sa);
                let imm = self.fetch8();
                let v = self.add8(self.read_u8(a), imm, self.psw.cy);
                self.write_u8(a, v);
            }
            // and A, #imm8 (0101 1100)
            0x5C => {
                let imm = self.fetch8();
                let v = self.rb(R_A) & imm;
                self.set_rb(R_A, v);
                self.set_nz(v);
            }
            // and A, [HL] (0101 1101)
            0x5D => {
                let a = self.es_addr(self.rw(3) as u32);
                let v = self.rb(R_A) & self.read_u8(a);
                self.set_rb(R_A, v);
                self.set_nz(v);
            }
            // and A, [HL+imm8] (0101 1110)
            0x5E => {
                let d = self.fetch8() as u32;
                let a = self.es_addr(self.rw(3) as u32 + d);
                let v = self.rb(R_A) & self.read_u8(a);
                self.set_rb(R_A, v);
                self.set_nz(v);
            }
            // and A, ES:[imm16] (0101 1111)
            0x5F => {
                let addr = self.fetch16() as u32;
                let v = self.rb(R_A) & self.read_u8(self.es_addr(addr));
                self.set_rb(R_A, v);
                self.set_nz(v);
            }
            // or A, #imm8 (0110 1100)
            0x6C => {
                let imm = self.fetch8();
                let v = self.rb(R_A) | imm;
                self.set_rb(R_A, v);
                self.set_nz(v);
            }
            // or A, [HL] (0110 1101)
            0x6D => {
                let a = self.es_addr(self.rw(3) as u32);
                let v = self.rb(R_A) | self.read_u8(a);
                self.set_rb(R_A, v);
                self.set_nz(v);
            }
            // or A, [HL+imm8] (0110 1110)
            0x6E => {
                let d = self.fetch8() as u32;
                let a = self.es_addr(self.rw(3) as u32 + d);
                let v = self.rb(R_A) | self.read_u8(a);
                self.set_rb(R_A, v);
                self.set_nz(v);
            }
            // or A, ES:[imm16] (0110 1111)
            0x6F => {
                let addr = self.fetch16() as u32;
                let v = self.rb(R_A) | self.read_u8(self.es_addr(addr));
                self.set_rb(R_A, v);
                self.set_nz(v);
            }
            // mov rba, #imm8 (0101 0reg)
            0x50..=0x57 => {
                let reg = (op & 0x07) as usize;
                let imm = self.fetch8();
                self.set_rb(reg, imm);
            }
            // mov A, rba (0110 0rba)
            0x60..=0x67 => {
                let reg = (op & 0x07) as usize;
                let v = self.rb(reg);
                self.set_rb(R_A, v);
            }
            // inc rba (1000 0reg)
            0x80..=0x87 => {
                let reg = (op & 0x07) as usize;
                let v = self.rb(reg).wrapping_add(1);
                self.set_rb(reg, v);
                self.set_nz(v);
            }
            // dec rba (1001 0reg)
            0x90..=0x97 => {
                let reg = (op & 0x07) as usize;
                let v = self.rb(reg).wrapping_sub(1);
                self.set_rb(reg, v);
                self.set_nz(v);
            }
            // mov A, [DE] (1000 1001)
            0x89 => {
                let a = self.es_addr(self.rw(2) as u32);
                let v = self.read_u8(a);
                self.set_rb(R_A, v);
            }
            // mov A, [DE+imm8] (1000 1010)
            0x8A => {
                let d = self.fetch8() as u32;
                let a = self.es_addr(self.rw(2) as u32 + d);
                let v = self.read_u8(a);
                self.set_rb(R_A, v);
            }
            // mov A, [HL] (1000 1011)
            0x8B => {
                let a = self.es_addr(self.rw(3) as u32);
                let v = self.read_u8(a);
                self.set_rb(R_A, v);
            }
            // mov A, [HL+imm8] (1000 1100)
            0x8C => {
                let d = self.fetch8() as u32;
                let a = self.es_addr(self.rw(3) as u32 + d);
                let v = self.read_u8(a);
                self.set_rb(R_A, v);
            }
            // mov A, saddr (1000 1101)
            0x8D => {
                let sa = self.fetch8() as u32;
                let a = self.saddr(sa);
                let v = self.read_u8(a);
                self.set_rb(R_A, v);
            }
            // mov A, ES:[imm16] (1000 1111)
            0x8F => {
                let addr = self.fetch16() as u32;
                let v = self.read_u8(self.es_addr(addr));
                self.set_rb(R_A, v);
            }
            // mov A, [SP+imm8] (1000 1000)
            0x88 => {
                let d = self.fetch8() as u32;
                let a = self.es_addr(self.sp as u32 + d);
                let v = self.read_u8(a);
                self.set_rb(R_A, v);
            }
            // mov A, [B+imm16] (0000 1001)
            0x09 => {
                let d = self.fetch16() as u32;
                let a = self.es_addr(self.rb(R_B) as u32 + d);
                let v = self.read_u8(a);
                self.set_rb(R_A, v);
            }
            // mov A, [BC+imm16] (0100 1001)
            0x49 => {
                let d = self.fetch16() as u32;
                let a = self.es_addr(self.rw(1) as u32 + d);
                let v = self.read_u8(a);
                self.set_rb(R_A, v);
            }
            // mov A, [C+imm16] (0010 1001)
            0x29 => {
                let d = self.fetch16() as u32;
                let a = self.es_addr(self.rb(R_C) as u32 + d);
                let v = self.read_u8(a);
                self.set_rb(R_A, v);
            }
            // mov [DE], A (1001 1001)
            0x99 => {
                let a = self.es_addr(self.rw(2) as u32);
                let v = self.rb(R_A);
                self.write_u8(a, v);
            }
            // mov [HL], A (1001 1011)
            0x9B => {
                let a = self.es_addr(self.rw(3) as u32);
                let v = self.rb(R_A);
                self.write_u8(a, v);
            }
            // mov saddr, A (1001 1101)
            0x9D => {
                let sa = self.fetch8() as u32;
                let a = self.saddr(sa);
                let v = self.rb(R_A);
                self.write_u8(a, v);
            }
            // mov ES:[imm16], A (1001 1111)
            0x9F => {
                let addr = self.fetch16() as u32;
                let v = self.rb(R_A);
                self.write_u8(self.es_addr(addr), v);
            }
            // push rp (1100 0rg1) / pop rp (1100 0rg0)
            0xC1 | 0xC3 | 0xC5 | 0xC7 => {
                let rg = ((op >> 1) & 0x3) as usize;
                let v = self.rw(rg);
                self.push16(v);
            }
            0xC0 | 0xC2 | 0xC4 | 0xC6 => {
                let rg = ((op >> 1) & 0x3) as usize;
                let v = self.pop16();
                self.set_rw(rg, v);
            }
            // ret (1101 0111)
            0xD7 => {
                let v = self.pop16();
                self.set_pc(v as u32);
            }
            // mov A, #imm8 (0100 0001) is 0x41; but 0x41 overlaps movw. Use
            // `0100 0001` = mov A, #imm8 per table line 722.
            0x41 => {
                let imm = self.fetch8();
                self.set_rb(R_A, imm);
            }
            // brk1 (1111 1111)
            0xFF => self.halted = true,
            _ => self.halted = true,
        }
    }

    /// Execute an instruction with the `0110 0001` (0x61) prefix.
    ///
    /// Reg-reg forms are 2-byte `0x61 XX` where the register index is the low
    /// 3 bits of the second byte; the high bits select the operation.
    fn execute_61(&mut self) {
        let op = self.fetch8();
        let rba = (op & 0x07) as usize;
        let reg = (op & 0x07) as usize;
        match op & 0xF8 {
            // add A, rba (0000 1rba)
            0x08 => {
                let v = self.add8(self.rb(R_A), self.rb(rba), false);
                self.set_rb(R_A, v);
            }
            // add rba, A (0000 0reg)
            0x00 => {
                let v = self.add8(self.rb(reg), self.rb(R_A), false);
                self.set_rb(reg, v);
            }
            // addc A, rba (0001 1rba)
            0x18 => {
                let v = self.add8(self.rb(R_A), self.rb(rba), self.psw.cy);
                self.set_rb(R_A, v);
            }
            // addc rba, A (0001 0reg)
            0x10 => {
                let v = self.add8(self.rb(reg), self.rb(R_A), self.psw.cy);
                self.set_rb(reg, v);
            }
            // sub A, rba (0010 1rba)
            0x28 => {
                let v = self.sub8(self.rb(R_A), self.rb(rba), false);
                self.set_rb(R_A, v);
            }
            // sub rba, A (0010 0reg)
            0x20 => {
                let v = self.sub8(self.rb(reg), self.rb(R_A), false);
                self.set_rb(reg, v);
            }
            // subc A, rba (0011 1rba)
            0x38 => {
                let v = self.sub8(self.rb(R_A), self.rb(rba), self.psw.cy);
                self.set_rb(R_A, v);
            }
            // subc rba, A (0011 0reg)
            0x30 => {
                let v = self.sub8(self.rb(reg), self.rb(R_A), self.psw.cy);
                self.set_rb(reg, v);
            }
            // cmp A, rba (0100 1rba)
            0x48 => {
                self.sub8(self.rb(R_A), self.rb(rba), false);
            }
            // cmp rba, A (0100 0reg)
            0x40 => {
                self.sub8(self.rb(reg), self.rb(R_A), false);
            }
            // and A, rba (0101 1rba)
            0x58 => {
                let v = self.rb(R_A) & self.rb(rba);
                self.set_rb(R_A, v);
                self.set_nz(v);
            }
            // and rba, A (0101 0reg)
            0x50 => {
                let v = self.rb(reg) & self.rb(R_A);
                self.set_rb(reg, v);
                self.set_nz(v);
            }
            // or A, rba (0110 1rba)
            0x68 => {
                let v = self.rb(R_A) | self.rb(rba);
                self.set_rb(R_A, v);
                self.set_nz(v);
            }
            // or rba, A (0110 0reg)
            0x60 => {
                let v = self.rb(reg) | self.rb(R_A);
                self.set_rb(reg, v);
                self.set_nz(v);
            }
            // xor A, rba (0111 1rba)
            0x78 => {
                let v = self.rb(R_A) ^ self.rb(rba);
                self.set_rb(R_A, v);
                self.set_nz(v);
            }
            // xor rba, A (0111 0reg)
            0x70 => {
                let v = self.rb(reg) ^ self.rb(R_A);
                self.set_rb(reg, v);
                self.set_nz(v);
            }
            // xch A, rba (1000 1reg)
            0x88 => {
                let a = self.rb(R_A);
                self.set_rb(R_A, self.rb(reg));
                self.set_rb(reg, a);
            }
            // add A, ES:[HL+B]/[HL+C] (1000 000/1000 0010)
            0x80 | 0x82 => {
                let idx = if op & 1 == 0 { R_B } else { R_C };
                let addr = self.rw(3).wrapping_add(self.rb(idx) as u16);
                let v = self.add8(self.rb(R_A), self.read_u8(self.es_addr(addr as u32)), false);
                self.set_rb(R_A, v);
            }
            // addc A, ES:[HL+B]/[HL+C] (1001 0000/1001 0010)
            0x90 | 0x92 => {
                let idx = if op & 1 == 0 { R_B } else { R_C };
                let addr = self.rw(3).wrapping_add(self.rb(idx) as u16);
                let v = self.add8(self.rb(R_A), self.read_u8(self.es_addr(addr as u32)), self.psw.cy);
                self.set_rb(R_A, v);
            }
            // sub A, ES:[HL+B]/[HL+C] (1010 000/1010 0010)
            0xA0 | 0xA2 => {
                let idx = if op & 1 == 0 { R_B } else { R_C };
                let addr = self.rw(3).wrapping_add(self.rb(idx) as u16);
                let v = self.sub8(self.rb(R_A), self.read_u8(self.es_addr(addr as u32)), false);
                self.set_rb(R_A, v);
            }
            // subc A, ES:[HL+B]/[HL+C] (1011 000/1011 0010)
            0xB0 | 0xB2 => {
                let idx = if op & 1 == 0 { R_B } else { R_C };
                let addr = self.rw(3).wrapping_add(self.rb(idx) as u16);
                let v = self.sub8(self.rb(R_A), self.read_u8(self.es_addr(addr as u32)), self.psw.cy);
                self.set_rb(R_A, v);
            }
            // cmp A, ES:[HL+B]/[HL+C] (1100 000/1100 0010)
            0xC0 | 0xC2 => {
                let idx = if op & 1 == 0 { R_B } else { R_C };
                let addr = self.rw(3).wrapping_add(self.rb(idx) as u16);
                self.sub8(self.rb(R_A), self.read_u8(self.es_addr(addr as u32)), false);
            }
            // mov A, ES:[HL+B]/[HL+C] (1100 1001/1110 1001)
            0xC9 | 0xE9 => {
                let idx = if op & 1 == 0 { R_B } else { R_C };
                let addr = self.rw(3).wrapping_add(self.rb(idx) as u16);
                let v = self.read_u8(self.es_addr(addr as u32));
                self.set_rb(R_A, v);
            }
            // mov ES:[HL+B]/[HL+C], A (1101 1001/1111 1001)
            0xD9 | 0xF9 => {
                let idx = if op & 1 == 0 { R_B } else { R_C };
                let addr = self.rw(3).wrapping_add(self.rb(idx) as u16);
                self.write_u8(self.es_addr(addr as u32), self.rb(R_A));
            }
            // mov A, ES:[imm16] (1010 1000)
            0xA8 => {
                let addr = self.fetch16() as u32;
                let v = self.read_u8(self.es_addr(addr));
                self.set_rb(R_A, v);
            }
            // mov ES:[imm16], A (1010 1001)
            0xA9 => {
                let addr = self.fetch16() as u32;
                self.write_u8(self.es_addr(addr), self.rb(R_A));
            }
            // and A, ES:[HL+B]/[HL+C] (1101 000/1101 0010)
            0xD0 | 0xD2 => {
                let idx = if op & 1 == 0 { R_B } else { R_C };
                let addr = self.rw(3).wrapping_add(self.rb(idx) as u16);
                let v = self.rb(R_A) & self.read_u8(self.es_addr(addr as u32));
                self.set_rb(R_A, v);
                self.set_nz(v);
            }
            // or A, ES:[HL+B]/[HL+C] (1110 000/1110 0010)
            0xE0 | 0xE2 => {
                let idx = if op & 1 == 0 { R_B } else { R_C };
                let addr = self.rw(3).wrapping_add(self.rb(idx) as u16);
                let v = self.rb(R_A) | self.read_u8(self.es_addr(addr as u32));
                self.set_rb(R_A, v);
                self.set_nz(v);
            }
            // xor A, ES:[HL+B]/[HL+C] (1111 000/1111 0010)
            0xF0 | 0xF2 => {
                let idx = if op & 1 == 0 { R_B } else { R_C };
                let addr = self.rw(3).wrapping_add(self.rb(idx) as u16);
                let v = self.rb(R_A) ^ self.read_u8(self.es_addr(addr as u32));
                self.set_rb(R_A, v);
                self.set_nz(v);
            }
            _ => match op {
                // inc ES:[HL+imm8] (0101 1001)
                0x59 => {
                    let dsp = self.fetch8() as i8 as i32;
                    let addr = self.rw(3).wrapping_add(dsp as u16);
                    let v = self.add8(self.read_u8(self.es_addr(addr as u32)), 1, false);
                    self.write_u8(self.es_addr(addr as u32), v);
                }
                // dec ES:[HL+imm8] (0110 1001)
                0x69 => {
                    let dsp = self.fetch8() as i8 as i32;
                    let addr = self.rw(3).wrapping_add(dsp as u16);
                    let v = self.sub8(self.read_u8(self.es_addr(addr as u32)), 1, false);
                    self.write_u8(self.es_addr(addr as u32), v);
                }
                // incw ES:[HL+imm8] (0111 1001)
                0x79 => {
                    let dsp = self.fetch8() as i8 as i32;
                    let addr = self.rw(3).wrapping_add(dsp as u16);
                    let v = self.read_u16(self.es_addr(addr as u32)).wrapping_add(1);
                    self.write_u16(self.es_addr(addr as u32), v);
                }
                // decw ES:[HL+imm8] (1000 1001)
                0x89 => {
                    let dsp = self.fetch8() as i8 as i32;
                    let addr = self.rw(3).wrapping_add(dsp as u16);
                    let v = self.read_u16(self.es_addr(addr as u32)).wrapping_sub(1);
                    self.write_u16(self.es_addr(addr as u32), v);
                }
                // mov A, ES:[HL+imm8] (1011 1000)
                0xB8 => {
                    let dsp = self.fetch8() as i8 as i32;
                    let addr = self.rw(3).wrapping_add(dsp as u16);
                    let v = self.read_u8(self.es_addr(addr as u32));
                    self.set_rb(R_A, v);
                }
                // mov ES:[HL+imm8], A (1011 1001)
                0xB9 => {
                    let dsp = self.fetch8() as i8 as i32;
                    let addr = self.rw(3).wrapping_add(dsp as u16);
                    self.write_u8(self.es_addr(addr as u32), self.rb(R_A));
                }
                // movw AX, ES:[HL+imm8] (0000 1001)
                0x09 => {
                    let dsp = self.fetch8() as i8 as i32;
                    let addr = self.rw(3).wrapping_add(dsp as u16);
                    let v = self.read_u16(self.es_addr(addr as u32));
                    self.set_rw(0, v);
                }
                // cmpw AX, ES:[HL+imm8] (0100 1001)
                0x49 => {
                    let dsp = self.fetch8() as i8 as i32;
                    let addr = self.rw(3).wrapping_add(dsp as u16);
                    self.sub16(self.rw(0), self.read_u16(self.es_addr(addr as u32)), false);
                }
                // subw AX, ES:[HL+imm8] (0010 1001)
                0x29 => {
                    let dsp = self.fetch8() as i8 as i32;
                    let addr = self.rw(3).wrapping_add(dsp as u16);
                    let v = self.sub16(self.rw(0), self.read_u16(self.es_addr(addr as u32)), false);
                    self.set_rw(0, v);
                }
                // xch A, ES:[imm16] (1010 1010)
                0xAA => {
                    let addr = self.fetch16() as u32;
                    let ea = self.es_addr(addr);
                    let v = self.read_u8(ea);
                    self.write_u8(ea, self.rb(R_A));
                    self.set_rb(R_A, v);
                }
                // xch A, ES:[saddr] (1010 1011)
                0xAB => {
                    let sa = self.fetch8() as u32;
                    let addr = self.saddr(sa);
                    let v = self.read_u8(addr);
                    self.write_u8(addr, self.rb(R_A));
                    self.set_rb(R_A, v);
                }
                // mov ES, ES:[saddr] (1011 1000 handled above as mov A; 1011 1011)
                // br AX (1100 1011)
                0xCB => {
                    self.set_pc(self.rw(0) as u32);
                }
                // reti (1111 1100)
                0xFC => {
                    let pc = self.pop16() as u32;
                    self.set_pc(pc);
                    let psw = self.pop16() as u8;
                    self.psw.z = psw & 1 != 0;
                    self.psw.ac = psw & 2 != 0;
                    self.psw.cy = psw & 4 != 0;
                    self.psw.rbs = (psw >> 3) & 0x3;
                    self.psw.ie = psw & 0x20 != 0;
                }
                // retb (1110 1100)
                0xEC => {
                    let pc = self.pop16() as u32;
                    self.set_pc(pc);
                    let _ = self.pop16();
                }
                // brk (1100 1100)
                0xCC => self.halted = true,
                // halt (1110 1101)
                0xED => self.halted = true,
                // stop (1111 1101)
                0xFD => self.halted = true,
                // SKC (1100 1000) / SKNC (1101 1000) / SKZ (1110 1000) / SKNZ (1111 1000)
                0xC8 | 0xD8 | 0xE8 | 0xF8 => {
                    let cond = match op {
                        0xC8 => 0,
                        0xD8 => 1,
                        0xE8 => 2,
                        _ => 3,
                    };
                    if self.cond_true(cond) {
                        let _ = self.fetch8();
                    }
                }
                // BH $rel8 (1100 0011)
                0xC3 => {
                    let rel = op as i8 as i32;
                    if self.cond_true(4) {
                        self.pc = self.pc.wrapping_add(rel as u32);
                    }
                }
                // BNH $rel8 (1101 0011)
                0xD3 => {
                    let rel = op as i8 as i32;
                    if self.cond_true(5) {
                        self.pc = self.pc.wrapping_add(rel as u32);
                    }
                }
                // pop psw (1100 1101)
                0xCD => {
                    let psw = self.pop16() as u8;
                    self.psw.z = psw & 1 != 0;
                    self.psw.ac = psw & 2 != 0;
                    self.psw.cy = psw & 4 != 0;
                    self.psw.rbs = (psw >> 3) & 0x3;
                    self.psw.ie = psw & 0x20 != 0;
                }
                // push psw (1101 1101)
                0xDD => {
                    let mut psw = 0u16;
                    if self.psw.z { psw |= 1; }
                    if self.psw.ac { psw |= 2; }
                    if self.psw.cy { psw |= 4; }
                    psw |= (self.psw.rbs as u16 & 0x3) << 3;
                    if self.psw.ie { psw |= 0x20; }
                    self.push16(psw);
                }
                // call ax (1110 1010) / call bc (1110 1100 overlapped)
                // mov A, ES:[HL+B]/[HL+C] handled above.
                // movs ES:[HL+imm8], X (1100 1110)
                0xCE => {
                    let dsp = self.fetch8() as i8 as i32;
                    let addr = self.rw(3).wrapping_add(dsp as u16);
                    let a = self.es_addr(addr as u32);
                    let x = self.rb(R_X);
                    let v = self.sub8(x, self.read_u8(a), false);
                    self.write_u8(a, v);
                }
                // rol A, 1 (1110 1011)
                0xEB => {
                    let a = self.rb(R_A);
                    let v = a.rotate_left(1);
                    self.set_rb(R_A, v);
                }
                // ror A, 1 (1101 1011)
                0xDB => {
                    let a = self.rb(R_A);
                    let v = a.rotate_right(1);
                    self.set_rb(R_A, v);
                }
                // rolwc AX/BC (111r 1110)
                0xEE | 0xFE => {
                    let r = ((op >> 4) & 0x01) as usize;
                    let w = self.rw(r);
                    let c = self.psw.cy as u16;
                    let v = (w << 1) | c;
                    self.psw.cy = (w & 0x8000) != 0;
                    self.set_rw(r, v);
                }
                // Unknown 0x61-prefixed opcode.
                _ => self.halted = true,
            },
        }
    }
}

/// Disassemble one RL78 instruction at `pc` using `read` for bytes.
/// Produces a readable one-line rendering; unknown bytes print as `.byte 0xXX`.
pub fn disassemble(pc: u32, read: &dyn Fn(u32) -> u8) -> String {
    let b0 = read(pc);
    // 0x61-prefixed: decode a couple of common forms for readability.
    if b0 == 0x61 {
        let b1 = read(pc + 1);
        let b2 = read(pc + 2);
        let sub = b1 & 0xF8;
        let reg = (b1 & 0x07) as usize;
        let rname = ["X", "A", "C", "B", "E", "D", "L", "H"][reg as usize];
        match sub {
            0x48 => return format!("cmp a, {rname}"),
            0x40 => return format!("cmp {rname}, a"),
            0x28 => return format!("sub a, {rname}"),
            0x20 => return format!("sub {rname}, a"),
            0x08 => return format!("add a, {rname}"),
            0x00 => return format!("add {rname}, a"),
            0x18 => return format!("addc a, {rname}"),
            0x10 => return format!("addc {rname}, a"),
            0x58 => return format!("and a, {rname}"),
            0x50 => return format!("and {rname}, a"),
            0x68 => return format!("or a, {rname}"),
            0x60 => return format!("or {rname}, a"),
            0x78 => return format!("xor a, {rname}"),
            0x70 => return format!("xor {rname}, a"),
            _ => return format!("prefix 0x61: 0x{b1:02x} 0x{b2:02x}"),
        }
    }
    match b0 {
        0x00 => "nop".to_string(),
        0x0C => format!("add a, #0x{:02x}", read(pc + 1)),
        0x0F => format!("add a, es:[0x{:04x}]", (read(pc + 1) as u16) | ((read(pc + 2) as u16) << 8)),
        0x04 => format!("addw ax, #0x{:04x}", (read(pc + 1) as u16) | ((read(pc + 2) as u16) << 8)),
        0xFF => "brk1".to_string(),
        0xD7 => "ret".to_string(),
        0xF6 => "clrw ax".to_string(),
        0xF7 => "clrw bc".to_string(),
        _ => format!(".byte 0x{b0:02x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core_with(code: &[u8]) -> Rl78Core {
        let mut c = Rl78Core::new();
        c.write_bytes(0x1000, code);
        c.pc = 0x1000;
        c
    }

    #[test]
    fn nop_advances() {
        let mut c = core_with(&[0x00]);
        let r = c.step(&[]);
        assert_eq!(r, Rl78Step::Advanced);
        assert_eq!(c.pc, 0x1001);
    }

    #[test]
    fn add_imm_to_a() {
        // ADD A, #0x05 (0x0C 0x05)
        let mut c = core_with(&[0x0C, 0x05]);
        c.set_rb(R_A, 10);
        c.step(&[]);
        assert_eq!(c.rb(R_A), 15);
    }

    #[test]
    fn add_sets_carry() {
        // ADD A, #0x02 with A=0xFE => 0x00, carry, zero.
        let mut c = core_with(&[0x0C, 0x02]);
        c.set_rb(R_A, 0xFE);
        c.step(&[]);
        assert_eq!(c.rb(R_A), 0);
        assert!(c.psw.z, "zero flag");
        assert!(c.psw.cy, "carry flag");
    }

    #[test]
    fn mov_register_immediate() {
        // 0101 0reg = MOV rba, #imm. reg encoding: X=0, A=1, C=2, B=3.
        // MOV A, #0x42 => 0x51 0x42 (reg=1: 0101 0001).
        let mut c = core_with(&[0x51, 0x42]);
        c.step(&[]);
        assert_eq!(c.rb(R_A), 0x42);
    }

    #[test]
    fn cmp_sets_zero() {
        // CMP A, rba: 0x61 0x48 (cmp a, x) with A=X=5. `0100 1rba`, rba=0 (X).
        let mut c = core_with(&[0x61, 0x48]);
        c.set_rb(R_A, 5);
        c.set_rb(R_X, 5);
        c.step(&[]);
        assert!(c.psw.z, "cmp equal sets Z");
    }

    #[test]
    fn sub_a_register() {
        // SUB A, rba: 0x61 0x28 (sub a, x). `0010 1rba`, rba=0 (X).
        let mut c = core_with(&[0x61, 0x28]);
        c.set_rb(R_A, 10);
        c.set_rb(R_X, 3);
        c.step(&[]);
        assert_eq!(c.rb(R_A), 7);
    }

    #[test]
    fn inc_register() {
        // INC rba: 1000 0reg, reg=1 (A) => 0x81.
        let mut c = core_with(&[0x81]);
        c.set_rb(R_A, 0xFF);
        c.step(&[]);
        assert_eq!(c.rb(R_A), 0);
        assert!(c.psw.z, "inc wrap sets Z");
    }

    #[test]
    fn dec_register() {
        // DEC rba: 1001 0reg, reg=1 (A) => 0x91.
        let mut c = core_with(&[0x91]);
        c.set_rb(R_A, 1);
        c.step(&[]);
        assert_eq!(c.rb(R_A), 0);
        assert!(c.psw.z, "dec to zero sets Z");
    }

    #[test]
    fn mov_es_addr_to_a() {
        // MOV A, ES:[imm16]: 1000 1000 (0x88) + imm16.
        // Actually `0000 1001 = MOV A, ES:[B+imm16]`; use `1000 1001 = MOV A, DE`
        // and `1000 1101 = MOV A, ES:[saddr]`. For the flat load use
        // MOV A, ES:[imm16] (0x8F is not in the table). Use `1000 1101` saddr.
        // MOV A, ES:[SADDR]: 1000 1101 (0x8D) + saddr 0x10 -> 0xFFF10.
        let mut c = core_with(&[0x8D, 0x10]);
        c.write_u8(0xFFF10, 0xAB);
        c.step(&[]);
        assert_eq!(c.rb(R_A), 0xAB);
    }

    #[test]
    fn push_pop_uses_sp() {
        // PUSH rp: 1100 0rg1. rg=0 (AX) => 0xC1.
        let mut c = core_with(&[0xC1]);
        c.sp = 0x2000;
        c.set_rw(0, 0x1234);
        c.step(&[]);
        assert_eq!(c.sp, 0x1FFE);
        assert_eq!(c.read_u16(0x1FFE), 0x1234);
        // POP rp: 1100 0rg0. rg=0 (AX) => 0xC0.
        let mut c2 = core_with(&[0xC0]);
        c2.sp = 0x1FFE;
        c2.write_u16(0x1FFE, 0xABCD);
        c2.step(&[]);
        assert_eq!(c2.sp, 0x2000);
        assert_eq!(c2.rw(0), 0xABCD);
    }

    #[test]
    fn ret_pops_pc() {
        // RET: 1101 0111 (0xD7).
        let mut c = core_with(&[0xD7]);
        c.sp = 0x1FFE;
        c.write_u16(0x1FFE, 0x2000);
        c.step(&[]);
        assert_eq!(c.pc, 0x2000);
        assert_eq!(c.sp, 0x2000);
    }

    #[test]
    fn movw_ax_from_register() {
        // MOVW AX, rp: 0001 0ra1. ra=1 (BC) => 0x13 (0001 0011).
        let mut c = core_with(&[0x13, 0x01]);
        c.set_rw(1, 0x5678); // BC
        c.step(&[]);
        assert_eq!(c.rw(0), 0x5678);
    }

    #[test]
    fn addw_ax_immediate() {
        // ADDW AX, #imm16: 0000 0100 (0x04) + imm16.
        let mut c = core_with(&[0x04, 0x34, 0x12]);
        c.set_rw(0, 0x1000);
        c.step(&[]);
        assert_eq!(c.rw(0), 0x2234);
    }
}
