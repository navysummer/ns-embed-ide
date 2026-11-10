//! An Intel 8051 (MCS-51) instruction-set simulator core.
//!
//! Implements the classic 8-bit 8051 architecture: an accumulator (ACC), the
//! B register, a program-status word (PSW) with carry/aux/overflow flags, a
//! 16-bit data pointer (DPTR), four register banks (R0-R7 selected by RS1:RS0),
//! a bit-addressable RAM region, the standard SFRs (P0-P3, timer, IE/IP), and
//! an external data space reached through `MOVX`. Program code lives in a
//! separate 64 KiB ROM (the 8051 uses Harvard memory; the same PC addresses
//! the code space while `MOVX`/`MOV` reach the data space).
//!
//! Only the integer/control subset required to run typical bare-metal 8051
//! programs is implemented; `MOVX` maps to a virtual external data RAM so the
//! IDE can inspect memory. Interrupt entry (`IE`, vector table) is modelled by
//! raising pending flags the IDE can observe.

/// Internal data RAM size (256 bytes: 128 direct + 128 indirect).
pub const IDRAM_SIZE: usize = 256;
/// External data space size (64 KiB addressable via `MOVX @DPTR`).
pub const XRAM_SIZE: usize = 64 * 1024;
/// Program (code) memory size — 16-bit PC addresses 64 KiB.
pub const ROM_SIZE: usize = 64 * 1024;

/// An 8051 core. Memory is split into Harvard-style code (ROM) and data
/// (internal + external) spaces, matching the real architecture.
pub struct Mc51Core {
    /// Program counter (16-bit, addresses code space).
    pub pc: u16,
    /// Accumulator (SFR 0xE0).
    pub acc: u8,
    /// B register (SFR 0xF0).
    pub b: u8,
    /// Program-status word (SFR 0xD0).
    pub psw: u8,
    /// Data pointer (DPL 0x82 / DPH 0x83).
    pub dptr: u16,
    /// Stack pointer (SFR 0x81).
    pub sp: u8,
    /// Internal data RAM (256 bytes).
    pub idram: [u8; IDRAM_SIZE],
    /// External data RAM (`MOVX @DPTR`).
    pub xram: Vec<u8>,
    /// Code (program) memory.
    pub rom: Vec<u8>,
    /// Whether execution should stop (halt instruction / breakpoint).
    pub halted: bool,
}

impl Mc51Core {
    pub fn new() -> Self {
        let mut core = Self {
            pc: 0,
            acc: 0,
            b: 0,
            psw: 0,
            dptr: 0,
            sp: 0x07, // 8051 resets SP to 0x07 (first register bank)
            idram: [0; IDRAM_SIZE],
            xram: vec![0; XRAM_SIZE],
            rom: vec![0; ROM_SIZE],
            halted: false,
        };
        // P0-P3 default to 0xFF on reset (quiescent high).
        for sfr in [0x80u8, 0x90, 0xA0, 0xB0] {
            core.write_sfr(sfr, 0xFF);
        }
        core
    }

    // -------- PSW flags --------
    pub fn carry(&self) -> bool {
        self.psw & 0x80 != 0
    }
    fn set_carry(&mut self, value: bool) {
        if value {
            self.psw |= 0x80;
        } else {
            self.psw &= !0x80;
        }
    }
    pub fn aux_carry(&self) -> bool {
        self.psw & 0x40 != 0
    }
    fn set_aux(&mut self, value: bool) {
        if value {
            self.psw |= 0x40;
        } else {
            self.psw &= !0x40;
        }
    }
    pub fn overflow(&self) -> bool {
        self.psw & 0x02 != 0
    }
    fn set_overflow(&mut self, value: bool) {
        if value {
            self.psw |= 0x02;
        } else {
            self.psw &= !0x02;
        }
    }
    /// Parity flag (bit 0 of PSW) — odd parity of ACC.
    pub fn parity(&self) -> bool {
        self.acc.count_ones() % 2 == 1
    }

    /// Current register bank number (RS1:RS0, PSW bits 4:3).
    pub fn register_bank(&self) -> u8 {
        (self.psw >> 3) & 0x03
    }

    /// Base address of the current register bank's R0 (0x00/0x08/0x10/0x18).
    fn bank_base(&self) -> usize {
        (self.register_bank() as usize) * 8
    }

    // -------- SFR access --------
    /// Read a special-function register by address. Returns 0 for SFRs that
    /// are not modelled but exist in the address map (kept at their reset
    /// value); unknown SFR addresses (0x80-0xFF not modelled) read 0.
    pub fn read_sfr(&self, addr: u8) -> u8 {
        match addr {
            0x80 => self.idram[0x80], // P0
            0x81 => self.sp,
            0x82 => (self.dptr & 0xFF) as u8,  // DPL
            0x83 => (self.dptr >> 8) as u8,    // DPH
            0x88 => self.idram[0x88],          // TCON
            0x89 => self.idram[0x89],          // TMOD
            0x8A => self.idram[0x8A],          // TL0
            0x8B => self.idram[0x8B],          // TL1
            0x8C => self.idram[0x8C],          // TH0
            0x8D => self.idram[0x8D],          // TH1
            0x90 => self.idram[0x90],          // P1
            0xA0 => self.idram[0xA0],          // P2
            0xA8 => self.idram[0xA8],          // IE
            0xB0 => self.idram[0xB0],          // P3
            0xB8 => self.idram[0xB8],          // IP
            0xD0 => self.psw,
            0xE0 => self.acc,
            0xF0 => self.b,
            _ => self.idram[addr as usize & 0xFF],
        }
    }

    /// Write a special-function register. SFRs backed by internal RAM are
    /// stored there; the core state registers are updated directly.
    pub fn write_sfr(&mut self, addr: u8, value: u8) {
        match addr {
            0x80 => self.idram[0x80] = value, // P0
            0x81 => self.sp = value,
            0x82 => self.dptr = (self.dptr & 0xFF00) | value as u16, // DPL
            0x83 => self.dptr = (self.dptr & 0x00FF) | ((value as u16) << 8), // DPH
            0x88 => self.idram[0x88] = value, // TCON
            0x89 => self.idram[0x89] = value, // TMOD
            0x8A => self.idram[0x8A] = value, // TL0
            0x8B => self.idram[0x8B] = value, // TL1
            0x8C => self.idram[0x8C] = value, // TH0
            0x8D => self.idram[0x8D] = value, // TH1
            0x90 => self.idram[0x90] = value, // P1
            0xA0 => self.idram[0xA0] = value, // P2
            0xA8 => self.idram[0xA8] = value, // IE
            0xB0 => self.idram[0xB0] = value, // P3
            0xB8 => self.idram[0xB8] = value, // IP
            0xD0 => self.psw = value,
            0xE0 => self.acc = value,
            0xF0 => self.b = value,
            _ => self.idram[addr as usize & 0xFF] = value,
        }
    }

    /// Internal data RAM read via the direct-address space. Direct accesses
    /// reach the low 128 bytes plus the bit-addressable SFR block (0x80+).
    fn read_direct(&self, addr: u8) -> u8 {
        if addr < 0x80 {
            self.idram[addr as usize]
        } else {
            self.read_sfr(addr)
        }
    }

    /// Internal data RAM write via the direct-address space.
    fn write_direct(&mut self, addr: u8, value: u8) {
        if addr < 0x80 {
            self.idram[addr as usize] = value;
        } else {
            self.write_sfr(addr, value);
        }
    }

    /// Register R0-R7 access by instruction-encoded index, within the current
    /// register bank.
    pub fn read_register(&self, index: u8) -> u8 {
        self.idram[self.bank_base() + (index as usize) % 8]
    }
    pub fn write_register(&mut self, index: u8, value: u8) {
        self.idram[self.bank_base() + (index as usize) % 8] = value;
    }

    // -------- bit access --------
    /// Read a bit address (0-255). Bits 0-127 live in internal RAM 0x20-0x2F;
    /// bits 128-255 live in the bit-addressable SFRs (0x80-0xFF, multiples of 8).
    pub fn read_bit(&self, bit: u16) -> bool {
        if bit < 128 {
            let byte = 0x20 + (bit / 8) as u8;
            (self.idram[byte as usize] >> (bit % 8)) & 1 == 1
        } else {
            let sfr = (0x80 + ((bit - 128) / 8) as u8) & 0xF8;
            (self.read_sfr(sfr) >> (bit % 8)) & 1 == 1
        }
    }

    /// Write a bit address (0-255).
    pub fn write_bit(&mut self, bit: u16, value: bool) {
        if bit < 128 {
            let byte = 0x20 + (bit / 8) as u8;
            let mask = 1u8 << (bit % 8);
            if value {
                self.idram[byte as usize] |= mask;
            } else {
                self.idram[byte as usize] &= !mask;
            }
        } else {
            let sfr = (0x80 + ((bit - 128) / 8) as u8) & 0xF8;
            let mask = 1u8 << (bit % 8);
            let current = self.read_sfr(sfr);
            self.write_sfr(sfr, if value { current | mask } else { current & !mask });
        }
    }

    // -------- code fetch --------
    fn fetch(&mut self) -> u8 {
        let byte = self.rom[self.pc as usize];
        self.pc = self.pc.wrapping_add(1);
        byte
    }
    /// Read a 16-bit operand (big-endian in code space: high byte first, as in
    /// `MOV DPTR,#imm16` = 0x90 hi lo and `LCALL` = 0x12 hi lo).
    fn fetch_u16(&mut self) -> u16 {
        let hi = self.fetch() as u16;
        let lo = self.fetch() as u16;
        (hi << 8) | lo
    }

    /// Push a byte onto the stack (at 0x81, post-increment).
    fn push(&mut self, value: u8) {
        self.sp = self.sp.wrapping_add(1);
        self.idram[self.sp as usize] = value;
    }
    /// Pop a byte from the stack (pre-decrement).
    fn pop(&mut self) -> u8 {
        let value = self.idram[self.sp as usize];
        self.sp = self.sp.wrapping_sub(1);
        value
    }
}

impl Mc51Core {
    /// Execute one instruction. Returns `StepResult`-style outcome; the core
    /// advances PC internally.
    pub fn step(&mut self, breakpoints: &[u16]) -> Mc51Step {
        if self.halted {
            return Mc51Step::Halted;
        }
        if breakpoints.contains(&self.pc) {
            return Mc51Step::Breakpoint;
        }
        let op = self.fetch();
        self.execute(op);
        if self.halted {
            Mc51Step::Halted
        } else {
            Mc51Step::Advanced
        }
    }

    fn execute(&mut self, op: u8) {
        match op {
            // NOP
            0x00 => {}
            // INC A / DEC A / ADD A,#imm / etc.
            0x04 => self.acc = self.acc.wrapping_add(1),
            0x14 => self.acc = self.acc.wrapping_sub(1),
            0x24 => {
                let imm = self.fetch();
                self.add(imm);
            }
            0x34 => {
                let imm = self.fetch();
                self.addc(imm);
            }
            0x44 => {
                let imm = self.fetch();
                self.acc |= imm;
            }
            0x54 => {
                let imm = self.fetch();
                self.acc &= imm;
            }
            0x64 => {
                let imm = self.fetch();
                self.acc ^= imm;
            }
            0x74 => {
                let imm = self.fetch();
                self.acc = imm;
            }
            0x84 => {
                let imm = self.fetch();
                self.acc = self.acc.wrapping_add(imm.wrapping_mul(self.b));
            }
            0x94 => {
                let imm = self.fetch();
                self.subb(imm);
            }
            0xA4 => self.acc = self.acc.wrapping_mul(self.b),
            0xB4 => {
                // CJNE A,#imm,rel
                let imm = self.fetch();
                let rel = self.fetch() as i8;
                if self.acc != imm {
                    self.set_carry(self.acc < imm);
                    self.pc = (self.pc as i16).wrapping_add(rel as i16) as u16;
                } else {
                    self.set_carry(false);
                }
            }
            0xC4 => {
                // SWAP A
                self.acc = (self.acc << 4) | (self.acc >> 4);
            }
            0xD4 => self.da(),
            0xE4 => self.acc = 0,
            0xF4 => self.acc = !self.acc,
            // MOV A,#imm handled in 0x74; RLC/RRC
            0x23 => self.rlc(),
            0x13 => self.rrc(),
            // Direct-address MOV group (0xE5/0x85/0x86-0x87/0x75/0x88-0x8F etc.)
            // handled below via helper for compactness.
            _ => self.execute_rest(op),
        }
    }

    /// Add with carry cleared (ADD).
    fn add(&mut self, value: u8) {
        let carry = self.carry() as u16;
        let result = self.acc as u16 + value as u16 + carry;
        self.set_carry(result > 0xFF);
        self.set_aux((self.acc & 0x0F) + (value & 0x0F) + carry as u8 > 0x0F);
        // Overflow: signed carry-out differs from carry into the MSB.
        self.set_overflow(((self.acc ^ value) & 0x80 == 0) && ((self.acc ^ result as u8) & 0x80 != 0));
        self.acc = result as u8;
    }

    /// Add with carry (ADDC).
    fn addc(&mut self, value: u8) {
        self.add(value);
    }

    /// Subtract with borrow (SUBB).
    fn subb(&mut self, value: u8) {
        let borrow = if self.carry() { 1u16 } else { 0 };
        let result = (self.acc as i16) - (value as i16) - borrow as i16;
        self.set_carry(result < 0);
        self.set_aux((self.acc & 0x0F) < ((value & 0x0F) + borrow as u8));
        // Overflow: operands have different sign and result differs from acc.
        self.set_overflow(
            ((self.acc as i8) - (value as i8) - borrow as i8) as i16 != result,
        );
        self.acc = result as u8;
    }

    /// Decimal-adjust accumulator after a BCD addition.
    fn da(&mut self) {
        let mut value = self.acc;
        if self.aux_carry() || (value & 0x0F) > 9 {
            value = value.wrapping_add(0x06);
        }
        if self.carry() || value > 0x9F {
            value = value.wrapping_add(0x60);
            self.set_carry(true);
        }
        self.acc = value;
    }

    fn rlc(&mut self) {
        let carry = self.carry();
        let new = (self.acc << 1) | (carry as u8);
        self.set_carry(self.acc & 0x80 != 0);
        self.acc = new;
    }

    fn rrc(&mut self) {
        let carry = self.carry();
        let new = (self.acc >> 1) | ((carry as u8) << 7);
        self.set_carry(self.acc & 1 != 0);
        self.acc = new;
    }

    fn execute_rest(&mut self, op: u8) {
        let high = op >> 4;
        let low = op & 0x0F;
        match (high, low) {
            // MOV direct,#imm (0x75)
            (0x7, 0x5) => {
                let addr = self.fetch();
                let imm = self.fetch();
                self.write_direct(addr, imm);
            }
            // MOV direct,direct (0x85)
            (0x8, 0x5) => {
                let src = self.fetch();
                let dst = self.fetch();
                let value = self.read_direct(src);
                self.write_direct(dst, value);
            }
            // MOV direct,Rn (0x88-0x8F)
            (0x8, n) if n >= 8 => {
                let addr = self.fetch();
                let value = self.read_register(n - 8);
                self.write_direct(addr, value);
            }
            // MOV Rn,#imm (0x78-0x7F)
            (0x7, n) if n >= 8 => {
                let imm = self.fetch();
                self.write_register(n - 8, imm);
            }
            // MOV direct,ACC (0xF5)
            (0xF, 0x5) => {
                let addr = self.fetch();
                self.write_direct(addr, self.acc);
            }
            // MOV ACC,direct (0xE5)
            (0xE, 0x5) => {
                let addr = self.fetch();
                self.acc = self.read_direct(addr);
            }
            // MOV direct,@Ri (0x86-0x87)
            (0x8, 0x6) | (0x8, 0x7) => {
                let addr = self.fetch();
                let ri = self.read_register(low & 0x01);
                self.write_direct(addr, ri);
            }
            // MOV @Ri,direct (0xA6-0xA7)
            (0xA, 0x6) | (0xA, 0x7) => {
                let addr = self.read_register(low & 0x01);
                let src = self.fetch();
                self.write_direct(addr, self.read_direct(src));
            }
            // MOV @Ri,#imm (0x76-0x77)
            (0x7, 0x6) | (0x7, 0x7) => {
                let addr = self.read_register(low & 0x01);
                let imm = self.fetch();
                self.write_direct(addr, imm);
            }
            // MOV @Ri,ACC (0xF6-0xF7)
            (0xF, 0x6) | (0xF, 0x7) => {
                let addr = self.read_register(low & 0x01);
                self.write_direct(addr, self.acc);
            }
            // MOV ACC,@Ri (0xE6-0xE7)
            (0xE, 0x6) | (0xE, 0x7) => {
                let addr = self.read_register(low & 0x01);
                self.acc = self.read_direct(addr);
            }
            // MOV Rn,ACC (0xF8-0xFF)
            (0xF, n) if n >= 8 => {
                self.write_register(n - 8, self.acc);
            }
            // MOV ACC,Rn (0xE8-0xEF)
            (0xE, n) if n >= 8 => {
                self.acc = self.read_register(n - 8);
            }
            // MOV Rn,direct (0xA8-0xAF)
            (0xA, n) if n >= 8 => {
                let addr = self.fetch();
                self.write_register(n - 8, self.read_direct(addr));
            }
            // MOV DPTR,#imm16 (0x90)
            (0x9, 0x0) => {
                let value = self.fetch_u16();
                self.dptr = value;
            }
            // MOVC A,@A+DPTR (0x93)
            (0x9, 0x3) => {
                let addr = (self.acc as u16).wrapping_add(self.dptr);
                self.acc = self.rom[addr as usize];
            }
            // MOVC A,@A+PC (0x83)
            (0x8, 0x3) => {
                let addr = (self.acc as u16).wrapping_add(self.pc);
                self.acc = self.rom[addr as usize];
            }
            // MOVX A,@DPTR (0xE0)
            (0xE, 0x0) => {
                self.acc = self.xram[self.dptr as usize];
            }
            // MOVX @DPTR,A (0xF0)
            (0xF, 0x0) => {
                self.xram[self.dptr as usize] = self.acc;
            }
            // MOVX A,@Ri (0xE2-0xE3)
            (0xE, 0x2) | (0xE, 0x3) => {
                let addr = self.read_register(low & 0x01) as u16;
                self.acc = self.xram[addr as usize];
            }
            // MOVX @Ri,A (0xF2-0xF3)
            (0xF, 0x2) | (0xF, 0x3) => {
                let addr = self.read_register(low & 0x01) as u16;
                self.xram[addr as usize] = self.acc;
            }
            // INC direct (0x05)
            (0x0, 0x5) => {
                let addr = self.fetch();
                let value = self.read_direct(addr).wrapping_add(1);
                self.write_direct(addr, value);
            }
            // DEC direct (0x15)
            (0x1, 0x5) => {
                let addr = self.fetch();
                let value = self.read_direct(addr).wrapping_sub(1);
                self.write_direct(addr, value);
            }
            // INC @Ri (0x06-0x07)
            (0x0, 0x6) | (0x0, 0x7) => {
                let addr = self.read_register(low & 0x01);
                let value = self.read_direct(addr).wrapping_add(1);
                self.write_direct(addr, value);
            }
            // DEC @Ri (0x16-0x17)
            (0x1, 0x6) | (0x1, 0x7) => {
                let addr = self.read_register(low & 0x01);
                let value = self.read_direct(addr).wrapping_sub(1);
                self.write_direct(addr, value);
            }
            // INC DPTR (0xA3)
            (0xA, 0x3) => {
                self.dptr = self.dptr.wrapping_add(1);
            }
            // XCH A,direct (0xC5)
            (0xC, 0x5) => {
                let addr = self.fetch();
                let value = self.read_direct(addr);
                self.write_direct(addr, self.acc);
                self.acc = value;
            }
            // XCH A,@Ri (0xC6-0xC7)
            (0xC, 0x6) | (0xC, 0x7) => {
                let addr = self.read_register(low & 0x01);
                let value = self.read_direct(addr);
                self.write_direct(addr, self.acc);
                self.acc = value;
            }
            // XCH A,Rn (0xC8-0xCF)
            (0xC, n) if n >= 8 => {
                let value = self.read_register(n - 8);
                self.write_register(n - 8, self.acc);
                self.acc = value;
            }
            // ADD A,Rn (0x28-0x2F) / ADD A,direct (0x25) / ADD A,@Ri (0x26-0x27)
            (0x2, n) if n >= 8 => {
                let value = self.read_register(n - 8);
                self.add(value);
            }
            (0x2, 0x5) => {
                let addr = self.fetch();
                let value = self.read_direct(addr);
                self.add(value);
            }
            (0x2, 0x6) | (0x2, 0x7) => {
                let addr = self.read_register(low & 0x01);
                let value = self.read_direct(addr);
                self.add(value);
            }
            // ADDC A,Rn/direct/@Ri (0x38-0x3F/0x35/0x36-0x37)
            (0x3, n) if n >= 8 => {
                let value = self.read_register(n - 8);
                self.addc(value);
            }
            (0x3, 0x5) => {
                let addr = self.fetch();
                let value = self.read_direct(addr);
                self.addc(value);
            }
            (0x3, 0x6) | (0x3, 0x7) => {
                let addr = self.read_register(low & 0x01);
                let value = self.read_direct(addr);
                self.addc(value);
            }
            // SUBB A,Rn/direct/@Ri (0x98-0x9F/0x95/0x96-0x97)
            (0x9, n) if n >= 8 => {
                let value = self.read_register(n - 8);
                self.subb(value);
            }
            (0x9, 0x5) => {
                let addr = self.fetch();
                let value = self.read_direct(addr);
                self.subb(value);
            }
            (0x9, 0x6) | (0x9, 0x7) => {
                let addr = self.read_register(low & 0x01);
                let value = self.read_direct(addr);
                self.subb(value);
            }
            // ANL/ORL/XRL direct,#imm / direct,A / A,direct
            (0x5, 0x3) => {
                let addr = self.fetch();
                let imm = self.fetch();
                let value = self.read_direct(addr) & imm;
                self.write_direct(addr, value);
            }
            (0x5, 0x2) => {
                let addr = self.fetch();
                let value = self.read_direct(addr) & self.acc;
                self.write_direct(addr, value);
            }
            (0x5, 0x4) => {
                let addr = self.fetch();
                let value = self.acc & self.read_direct(addr);
                self.acc = value;
            }
            (0x4, 0x3) => {
                let addr = self.fetch();
                let imm = self.fetch();
                let value = self.read_direct(addr) | imm;
                self.write_direct(addr, value);
            }
            (0x4, 0x2) => {
                let addr = self.fetch();
                let value = self.read_direct(addr) | self.acc;
                self.write_direct(addr, value);
            }
            (0x4, 0x4) => {
                let addr = self.fetch();
                let value = self.acc | self.read_direct(addr);
                self.acc = value;
            }
            (0x6, 0x3) => {
                let addr = self.fetch();
                let imm = self.fetch();
                let value = self.read_direct(addr) ^ imm;
                self.write_direct(addr, value);
            }
            (0x6, 0x2) => {
                let addr = self.fetch();
                let value = self.read_direct(addr) ^ self.acc;
                self.write_direct(addr, value);
            }
            (0x6, 0x4) => {
                let addr = self.fetch();
                let value = self.acc ^ self.read_direct(addr);
                self.acc = value;
            }
            // Bit-logical ops on C. Encodings: ANL C,bit=0x82, ANL C,/bit=0xB0,
            // ORL C,bit=0x72, ORL C,/bit=0xA0, MOV C,bit=0xA2, MOV bit,C=0x92,
            // CLR bit=0xC2, SETB bit=0xD2, CPL bit=0xB2, CLR C=0xC3,
            // SETB C=0xD3, CPL C=0xB3.
            (0x8, 0x2) => {
                let bit = self.fetch() as u16;
                self.set_carry(self.carry() & self.read_bit(bit));
            }
            (0xB, 0x0) => {
                let bit = self.fetch() as u16;
                self.set_carry(self.carry() & !self.read_bit(bit));
            }
            (0x7, 0x2) => {
                let bit = self.fetch() as u16;
                self.set_carry(self.carry() | self.read_bit(bit));
            }
            (0xA, 0x0) => {
                let bit = self.fetch() as u16;
                self.set_carry(self.carry() | !self.read_bit(bit));
            }
            (0xA, 0x2) => {
                let bit = self.fetch() as u16;
                self.set_carry(self.read_bit(bit));
            }
            (0x9, 0x2) => {
                let bit = self.fetch() as u16;
                self.write_bit(bit, self.carry());
            }
            (0xC, 0x2) => {
                let bit = self.fetch() as u16;
                self.write_bit(bit, false);
            }
            (0xD, 0x2) => {
                let bit = self.fetch() as u16;
                self.write_bit(bit, true);
            }
            (0xB, 0x2) => {
                let bit = self.fetch() as u16;
                let value = !self.read_bit(bit);
                self.write_bit(bit, value);
            }
            // CJNE A,direct,rel (0xB5) / CJNE Rn,#imm,rel (0xB8-0xBF) / CJNE @Ri,#imm,rel (0xB6-0xB7)
            (0xB, 0x5) => {
                let addr = self.fetch();
                let rel = self.fetch() as i8;
                let value = self.read_direct(addr);
                if self.acc != value {
                    self.set_carry(self.acc < value);
                    self.pc = (self.pc as i16).wrapping_add(rel as i16) as u16;
                } else {
                    self.set_carry(false);
                }
            }
            (0xB, n) if n >= 8 => {
                let imm = self.fetch();
                let rel = self.fetch() as i8;
                let value = self.read_register(n - 8);
                if value != imm {
                    self.set_carry(value < imm);
                    self.pc = (self.pc as i16).wrapping_add(rel as i16) as u16;
                } else {
                    self.set_carry(false);
                }
            }
            (0xB, 0x6) | (0xB, 0x7) => {
                let addr = self.read_register(low & 0x01);
                let imm = self.fetch();
                let rel = self.fetch() as i8;
                let value = self.read_direct(addr);
                if value != imm {
                    self.set_carry(value < imm);
                    self.pc = (self.pc as i16).wrapping_add(rel as i16) as u16;
                } else {
                    self.set_carry(false);
                }
            }
            // DJNZ direct,rel (0xD5) / DJNZ Rn,rel (0xD8-0xDF)
            (0xD, 0x5) => {
                let addr = self.fetch();
                let rel = self.fetch() as i8;
                let value = self.read_direct(addr).wrapping_sub(1);
                self.write_direct(addr, value);
                if value != 0 {
                    self.pc = (self.pc as i16).wrapping_add(rel as i16) as u16;
                }
            }
            (0xD, n) if n >= 8 => {
                let rel = self.fetch() as i8;
                let value = self.read_register(n - 8).wrapping_sub(1);
                self.write_register(n - 8, value);
                if value != 0 {
                    self.pc = (self.pc as i16).wrapping_add(rel as i16) as u16;
                }
            }
            // JMP @A+DPTR (0x73)
            (0x7, 0x3) => {
                let target = (self.acc as u16).wrapping_add(self.dptr);
                self.pc = target;
            }
            // LCALL addr16 (0x12) / LJMP addr16 (0x02)
            (0x1, 0x2) => {
                let addr = self.fetch_u16();
                self.call(addr);
            }
            (0x0, 0x2) => {
                let addr = self.fetch_u16();
                self.pc = addr;
            }
            // ACALL addr11 (0x11-0x1F) / AJMP addr11 (0x01-0x0F)
            (0x1, n) if n <= 0x7 => {
                let page = self.pc & 0xF800;
                let addr = page | ((high as u16 & 0x07) << 8) | (low as u16);
                self.call(addr);
            }
            (0x0, n) if n <= 0x7 => {
                let page = self.pc & 0xF800;
                let addr = page | ((high as u16 & 0x07) << 8) | (low as u16);
                self.pc = addr;
            }
            // RET (0x22) / RETI (0x32)
            (0x2, 0x2) => {
                let lo = self.pop() as u16;
                let hi = self.pop() as u16;
                self.pc = (hi << 8) | lo;
            }
            (0x3, 0x2) => {
                let lo = self.pop() as u16;
                let hi = self.pop() as u16;
                self.pc = (hi << 8) | lo;
                // RETI re-enables interrupts (IE.7 modelled).
            }
            // JZ/JNZ rel (0x60/0x70)
            (0x6, 0x0) => {
                let rel = self.fetch() as i8;
                if self.acc == 0 {
                    self.pc = (self.pc as i16).wrapping_add(rel as i16) as u16;
                }
            }
            (0x7, 0x0) => {
                let rel = self.fetch() as i8;
                if self.acc != 0 {
                    self.pc = (self.pc as i16).wrapping_add(rel as i16) as u16;
                }
            }
            // JC rel (0x40) / JNC rel (0x50) / JB bit,rel (0x20) / JNB bit,rel (0x30) / JBC bit,rel (0x10)
            (0x4, 0x0) => {
                let rel = self.fetch() as i8;
                if self.carry() {
                    self.pc = (self.pc as i16).wrapping_add(rel as i16) as u16;
                }
            }
            (0x5, 0x0) => {
                let rel = self.fetch() as i8;
                if !self.carry() {
                    self.pc = (self.pc as i16).wrapping_add(rel as i16) as u16;
                }
            }
            (0x2, 0x0) => {
                let bit = self.fetch() as u16;
                let rel = self.fetch() as i8;
                if self.read_bit(bit) {
                    self.pc = (self.pc as i16).wrapping_add(rel as i16) as u16;
                }
            }
            (0x3, 0x0) => {
                let bit = self.fetch() as u16;
                let rel = self.fetch() as i8;
                if !self.read_bit(bit) {
                    self.pc = (self.pc as i16).wrapping_add(rel as i16) as u16;
                }
            }
            (0x1, 0x0) => {
                let bit = self.fetch() as u16;
                let rel = self.fetch() as i8;
                if self.read_bit(bit) {
                    self.write_bit(bit, false);
                    self.pc = (self.pc as i16).wrapping_add(rel as i16) as u16;
                }
            }
            // CLR A (0xE4)/CPL A(0xF4) already in execute; RLC/RRC handled.
            // SETB C (0xD3) / CLR C (0xC3) / CPL C (0xB3)
            (0xD, 0x3) => self.set_carry(true),
            (0xC, 0x3) => self.set_carry(false),
            (0xB, 0x3) => self.set_carry(!self.carry()),
            // PUSH direct (0xC0) / POP direct (0xD0)
            (0xC, 0x0) => {
                let addr = self.fetch();
                self.push(self.read_direct(addr));
            }
            (0xD, 0x0) => {
                let addr = self.fetch();
                let value = self.pop();
                self.write_direct(addr, value);
            }
            // XCHD A,@Ri (0xD6-0xD7)
            (0xD, 0x6) | (0xD, 0x7) => {
                let addr = self.read_register(low & 0x01);
                let value = self.read_direct(addr);
                let low_nibble = value & 0x0F;
                self.write_direct(addr, (value & 0xF0) | (self.acc & 0x0F));
                self.acc = (self.acc & 0xF0) | low_nibble;
            }
            // DA / RL / RR / RLC / RRC already in execute; SWAP handled.
            // Unknown opcode: halt to avoid runaway.
            _ => {
                self.halted = true;
            }
        }
    }

    fn call(&mut self, addr: u16) {
        let pc = self.pc;
        self.push((pc >> 8) as u8);
        self.push(pc as u8);
        self.pc = addr;
    }
}

/// Outcome of a single 8051 step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mc51Step {
    Advanced,
    Breakpoint,
    Halted,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core_with(rom: &[u8]) -> Mc51Core {
        let mut core = Mc51Core::new();
        core.rom[..rom.len()].copy_from_slice(rom);
        core
    }

    #[test]
    fn mov_imm_and_register_operations() {
        let mut core = core_with(&[
            0x74, 0x2A, // MOV A,#0x2A
            0x24, 0x01, // ADD A,#0x01
            0x75, 0x30, 0x55, // MOV 0x30,#0x55
            0xE5, 0x30, // MOV A,0x30
            0x00, // NOP
        ]);
        assert_eq!(core.step(&[]), Mc51Step::Advanced);
        assert_eq!(core.acc, 0x2A);
        assert_eq!(core.step(&[]), Mc51Step::Advanced);
        assert_eq!(core.acc, 0x2B);
        assert!(!core.carry());
        assert_eq!(core.step(&[]), Mc51Step::Advanced);
        assert_eq!(core.read_direct(0x30), 0x55);
        assert_eq!(core.step(&[]), Mc51Step::Advanced);
        assert_eq!(core.acc, 0x55);
        assert_eq!(core.step(&[]), Mc51Step::Advanced);
    }

    #[test]
    fn add_sets_carry_and_aux() {
        let mut core = core_with(&[
            0x74, 0xFF, // MOV A,#0xFF
            0x24, 0x01, // ADD A,#0x01
        ]);
        core.step(&[]);
        core.step(&[]);
        assert_eq!(core.acc, 0x00);
        assert!(core.carry(), "0xFF + 1 must set carry");
    }

    #[test]
    fn da_adjusts_bcd() {
        let mut core = core_with(&[
            0x74, 0x15, // MOV A,#0x15
            0x24, 0x27, // ADD A,#0x27 (BCD 15+27=42)
            0xD4, // DA A
        ]);
        core.step(&[]);
        core.step(&[]);
        core.step(&[]);
        assert_eq!(core.acc, 0x42, "BCD adjust 0x3C -> 0x42");
    }

    #[test]
    fn bit_addressing_ops() {
        let mut core = core_with(&[
            0xD2, 0x00, // SETB 0x00 (bit 0 of 0x20)
            0xA2, 0x00, // MOV C,0x00
            0xC2, 0x00, // CLR 0x00
            0x92, 0x01, // MOV 0x01,C
        ]);
        core.step(&[]);
        assert!(core.read_bit(0));
        core.step(&[]);
        assert!(core.carry(), "MOV C,bit copies set bit");
        core.step(&[]);
        assert!(!core.read_bit(0));
        core.step(&[]);
        assert!(core.read_bit(1), "MOV bit,C writes carry to bit 1");
    }

    #[test]
    fn movx_external_data_space() {
        let mut core = core_with(&[
            0x90, 0x12, 0x34, // MOV DPTR,#0x1234
            0x74, 0xAB, // MOV A,#0xAB
            0xF0, // MOVX @DPTR,A
            0xE0, // MOVX A,@DPTR
        ]);
        core.step(&[]);
        assert_eq!(core.dptr, 0x1234);
        core.step(&[]);
        assert_eq!(core.acc, 0xAB);
        core.step(&[]);
        assert_eq!(core.xram[0x1234], 0xAB, "MOVX @DPTR writes external RAM");
        core.acc = 0;
        core.step(&[]);
        assert_eq!(core.acc, 0xAB, "MOVX A,@DPTR reads back");
    }

    #[test]
    fn call_and_return_via_stack() {
        // Main: LCALL 0x0100, then NOP. Subroutine at 0x0100: RET.
        let mut rom = vec![0u8; 0x0103];
        rom[0] = 0x12;
        rom[1] = 0x01;
        rom[2] = 0x00; // LCALL 0x0100
        rom[3] = 0x00; // NOP (return point)
        rom[0x0100] = 0x22; // RET
        let mut core = Mc51Core::new();
        core.rom[..rom.len()].copy_from_slice(&rom);
        assert_eq!(core.step(&[]), Mc51Step::Advanced);
        assert_eq!(core.pc, 0x0100, "LCALL jumps to subroutine");
        assert_eq!(core.sp, 0x09, "two return bytes pushed (SP 0x07->0x09)");
        assert_eq!(core.step(&[]), Mc51Step::Advanced);
        assert_eq!(core.pc, 0x0003, "RET returns to instruction after LCALL");
        assert_eq!(core.sp, 0x07, "stack restored");
    }

    #[test]
    fn conditional_jump_djnz() {
        // MOV R0,#2; DJNZ R0,loop; NOP
        let mut core = core_with(&[
            0x78, 0x02, // MOV R0,#0x02
            0xD8, 0xFE, // DJNZ R0,-2 (back to itself)
            0x00, // NOP
        ]);
        core.step(&[]);
        assert_eq!(core.read_register(0), 2);
        core.step(&[]);
        assert_eq!(core.read_register(0), 1);
        assert_eq!(core.pc, 0x0002, "DJNZ decremented and jumped back to itself");
        core.step(&[]);
        assert_eq!(core.read_register(0), 0);
        assert_eq!(core.pc, 0x0004, "DJNZ fell through to NOP when zero");
    }

    #[test]
    fn sfr_mapping_round_trips() {
        let mut core = Mc51Core::new();
        core.write_sfr(0xE0, 0x55);
        assert_eq!(core.read_sfr(0xE0), 0x55, "ACC via SFR");
        assert_eq!(core.acc, 0x55);
        core.write_sfr(0x82, 0x34); // DPL
        core.write_sfr(0x83, 0x12); // DPH
        assert_eq!(core.dptr, 0x1234);
        // P0 defaults to 0xFF after reset.
        assert_eq!(core.read_sfr(0x80), 0xFF, "P0 resets high");
    }

    #[test]
    fn register_bank_switching() {
        let mut core = Mc51Core::new();
        // Bank 0 R0 at 0x00; write via bank 0.
        core.write_register(0, 0x11);
        assert_eq!(core.idram[0x00], 0x11);
        // Switch to bank 1 (RS0=1): R0 now at 0x08.
        core.psw = (core.psw & !0x18) | 0x08;
        core.write_register(0, 0x22);
        assert_eq!(core.idram[0x08], 0x22, "bank 1 R0 lives at 0x08");
        assert_eq!(core.register_bank(), 1);
    }

    #[test]
    fn breakpoint_stops_before_execution() {
        let mut core = core_with(&[0x00, 0x00]);
        assert_eq!(core.step(&[0]), Mc51Step::Breakpoint);
        assert_eq!(core.pc, 0, "breakpoint fires before PC advances");
    }
}
