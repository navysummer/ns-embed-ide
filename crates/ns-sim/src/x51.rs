//! An Intel 80251 (MCS-251) instruction-set simulator core.
//!
//! The 80251 is the 16-bit successor to the 8051: a 16-bit accumulator and
//! data pointer, a 24-bit program/data address space, and 16 general-purpose
//! registers (R0-R15) selectable in banks. This core models the commonly
//! emitted instructions for bare-metal firmware (byte/word moves, arithmetic
//! with carry, bit operations, the DPTR 24-bit pointer, and the control-flow
//! set including the 24-bit `LJMP`/`LCALL`).
//!
//! Memory is a flat 24-bit address space backed by a byte vector sized for
//! the internal RAM plus an extended RAM window; accesses outside the
//! simulated range read 0 and drop writes rather than panicking.

/// Internal RAM size (256 bytes, mirroring the 8051 lower space).
pub const IDRAM_SIZE: usize = 256;
/// Extended RAM window (up to 64 KiB of external data space).
pub const XRAM_SIZE: usize = 64 * 1024;
/// Program-space size (24-bit addressable, capped to a practical image).
pub const ROM_SIZE: usize = 1024 * 1024;

/// Stack pointer register index (in the 16-register window).
pub const SP: usize = 8;
/// Program counter register index.
pub const PC_REG: usize = 15;

/// An 80251 core.
pub struct X51Core {
    /// Accumulator (16-bit).
    pub acc: u16,
    /// B register (8-bit, used by MUL/DIV).
    pub b: u8,
    /// Program status word (bits: C/AC/OV/N/Z).
    pub psw: u8,
    /// Stack pointer.
    pub sp: u8,
    /// 16-bit data pointer (low) + high page for 24-bit addressing.
    pub dptr: u32,
    /// Register bank R0-R15 (16-bit each).
    pub regs: [u16; 16],
    /// Internal data RAM.
    pub idram: [u8; IDRAM_SIZE],
    /// Extended data RAM.
    pub xram: Vec<u8>,
    /// Program image.
    pub rom: Vec<u8>,
    /// Program counter.
    pub pc: u32,
    /// Whether execution stopped.
    pub halted: bool,
    /// Instructions executed since reset.
    pub cycle_count: u64,
}

impl X51Core {
    pub fn new() -> Self {
        Self {
            acc: 0,
            b: 0,
            psw: 0,
            sp: 0x07,
            dptr: 0,
            regs: [0; 16],
            idram: [0; IDRAM_SIZE],
            xram: vec![0; XRAM_SIZE],
            rom: vec![0; ROM_SIZE],
            pc: 0,
            halted: false,
            cycle_count: 0,
        }
    }

    // -------- condition flags --------
    pub fn carry(&self) -> bool {
        self.psw & 0x80 != 0
    }
    pub fn aux_carry(&self) -> bool {
        self.psw & 0x40 != 0
    }
    pub fn overflow(&self) -> bool {
        self.psw & 0x04 != 0
    }
    fn set_carry(&mut self, value: bool) {
        if value {
            self.psw |= 0x80;
        } else {
            self.psw &= !0x80;
        }
    }
    fn set_aux_carry(&mut self, value: bool) {
        if value {
            self.psw |= 0x40;
        } else {
            self.psw &= !0x40;
        }
    }
    fn set_overflow(&mut self, value: bool) {
        if value {
            self.psw |= 0x04;
        } else {
            self.psw &= !0x04;
        }
    }
    fn set_nz(&mut self, value: u16) {
        if value == 0 {
            self.psw |= 0x02;
        } else {
            self.psw &= !0x02;
        }
        if value & 0x8000 != 0 {
            self.psw |= 0x01;
        } else {
            self.psw &= !0x01;
        }
    }

    // -------- memory access --------
    fn read_idram(&self, addr: u8) -> u8 {
        let a = addr as usize;
        if a >= 0x20 && a < 0x30 {
            // Bit-addressable region maps to registers' low bytes.
            return self.idram[a];
        }
        self.idram[a.min(IDRAM_SIZE - 1)]
    }
    fn write_idram(&mut self, addr: u8, value: u8) {
        let a = addr as usize;
        if a < IDRAM_SIZE {
            self.idram[a] = value;
        }
    }
    fn read_xram(&self, addr: u32) -> u8 {
        let a = addr as usize;
        if a < self.xram.len() {
            self.xram[a]
        } else {
            0
        }
    }
    fn write_xram(&mut self, addr: u32, value: u8) {
        let a = addr as usize;
        if a < self.xram.len() {
            self.xram[a] = value;
        }
    }
    fn read_prog(&self, addr: u32) -> u8 {
        let a = addr as usize;
        if a < self.rom.len() {
            self.rom[a]
        } else {
            0
        }
    }

    /// Load a program image at `base` (24-bit address) into code space.
    pub fn load_image(&mut self, base: u32, image: &[u8]) {
        for (i, byte) in image.iter().enumerate() {
            let addr = base as usize + i;
            if addr < self.rom.len() {
                self.rom[addr] = *byte;
            }
        }
        self.pc = base;
        self.halted = false;
    }

    // -------- fetch/decode --------
    fn fetch(&mut self) -> u8 {
        let byte = self.read_prog(self.pc);
        self.pc = self.pc.wrapping_add(1) & 0xFF_FFFF;
        byte
    }
    fn fetch_u16(&mut self) -> u16 {
        // Big-endian: high byte first.
        let hi = self.fetch() as u16;
        let lo = self.fetch() as u16;
        (hi << 8) | lo
    }
    fn fetch_u24(&mut self) -> u32 {
        // 80251 is big-endian: the highest byte comes first.
        let hi = self.fetch() as u32;
        let mid = self.fetch() as u32;
        let lo = self.fetch() as u32;
        (hi << 16) | (mid << 8) | lo
    }
    fn fetch_s8(&mut self) -> i8 {
        self.fetch() as i8
    }

    /// Execute one instruction. Returns what happened.
    pub fn step(&mut self, breakpoints: &[u32]) -> X51Step {
        if self.halted {
            return X51Step::Halted;
        }
        if breakpoints.contains(&self.pc) {
            return X51Step::Breakpoint;
        }
        let op = self.fetch();
        self.execute(op);
        self.cycle_count += 1;
        if self.halted {
            X51Step::Halted
        } else {
            X51Step::Advanced
        }
    }

    fn execute(&mut self, op: u8) {
        // 80251 opcode map (simplified): the most common byte/word ops. Each
        // opcode's upper bits select the addressing mode; we decode the
        // handful a bare-metal toolchain emits.
        match op {
            // NOP
            0x00 => {}
            // MOV Rn, #imm16 (0x74-0x7F: MOV R4-R15, #imm16)
            0x74..=0x7F => {
                let rn = (op & 0x0F) as usize;
                self.regs[rn] = self.fetch_u16();
            }
            // MOV A, Rn (0xE8-0xEF)
            0xE8..=0xEF => {
                let rn = (op & 0x07) as usize;
                self.acc = self.regs[rn];
            }
            // MOV Rn, A (0xF8-0xFF)
            0xF8..=0xFF => {
                let rn = (op & 0x07) as usize;
                self.regs[rn] = self.acc;
            }
            // ADD A, Rn (0x28-0x2F)
            0x28..=0x2F => {
                let rn = (op & 0x07) as usize;
                let value = self.regs[rn];
                let result = self.acc.wrapping_add(value);
                let carry = result < self.acc;
                self.set_carry(carry);
                self.set_aux_carry(((self.acc & 0x0F) + (value & 0x0F)) > 0x0F);
                self.acc = result;
                self.set_nz(result);
            }
            // SUBB A, Rn (0x98-0x9F)
            0x98..=0x9F => {
                let rn = (op & 0x07) as usize;
                let value = self.regs[rn];
                let borrow = if self.carry() { 1 } else { 0 };
                let result = self.acc.wrapping_sub(value.wrapping_add(borrow));
                let carry = self.acc < value.wrapping_add(borrow);
                self.set_carry(carry);
                self.set_aux_carry((self.acc & 0x0F) < (value & 0x0F).wrapping_add(borrow));
                self.acc = result;
                self.set_nz(result);
            }
            // INC A / DEC A
            0x04 => {
                self.acc = self.acc.wrapping_add(1);
                self.set_nz(self.acc);
            }
            0x14 => {
                self.acc = self.acc.wrapping_sub(1);
                self.set_nz(self.acc);
            }
            // INC Rn (0x08-0x0F) / DEC Rn (0x18-0x1F)
            0x08..=0x0F => {
                let rn = (op & 0x07) as usize;
                self.regs[rn] = self.regs[rn].wrapping_add(1);
                self.set_nz(self.regs[rn]);
            }
            0x18..=0x1F => {
                let rn = (op & 0x07) as usize;
                self.regs[rn] = self.regs[rn].wrapping_sub(1);
                self.set_nz(self.regs[rn]);
            }
            // MUL AB (0xA4): acc = acc*b, b = high
            0xA4 => {
                let product = (self.acc as u32).wrapping_mul(self.b as u32);
                self.acc = product as u16;
                self.b = (product >> 16) as u8;
                self.set_carry(false);
                self.set_overflow(false);
            }
            // DIV AB (0x84): acc = acc/b, b = remainder
            0x84 => {
                if self.b == 0 {
                    self.set_carry(false);
                    self.set_overflow(true);
                } else {
                    let quotient = self.acc / self.b as u16;
                    let remainder = self.acc % self.b as u16;
                    self.acc = quotient;
                    self.b = remainder as u8;
                    self.set_carry(false);
                    self.set_overflow(false);
                }
            }
            // LJMP (0x02): 24-bit absolute
            0x02 => {
                let addr = self.fetch_u24();
                self.pc = addr;
            }
            // LCALL (0x12): push return PC, 24-bit target
            0x12 => {
                let addr = self.fetch_u24();
                let ret = self.pc & 0xFF_FFFF;
                self.sp = self.sp.wrapping_add(1);
                self.write_idram(self.sp, (ret >> 16) as u8);
                self.sp = self.sp.wrapping_add(1);
                self.write_idram(self.sp, (ret >> 8) as u8);
                self.sp = self.sp.wrapping_add(1);
                self.write_idram(self.sp, ret as u8);
                self.pc = addr;
            }
            // RET (0x22)
            0x22 => {
                let lo = self.read_idram(self.sp) as u32;
                self.sp = self.sp.wrapping_sub(1);
                let mid = self.read_idram(self.sp) as u32;
                self.sp = self.sp.wrapping_sub(1);
                let hi = self.read_idram(self.sp) as u32;
                self.sp = self.sp.wrapping_sub(1);
                self.pc = (hi << 16) | (mid << 8) | lo;
            }
            // JMP @A+DPTR (0x73): 16-bit relative from DPTR
            0x73 => {
                let addr = (self.dptr.wrapping_add(self.acc as u32)) & 0xFF_FFFF;
                self.pc = addr;
            }
            // MOVX @DPTR, A / MOVX A, @DPTR (0xF0/0xE0 with DPTR select)
            0xF0 => {
                let addr = self.dptr & 0xFF_FFFF;
                self.write_xram(addr, self.acc as u8);
            }
            0xE0 => {
                let addr = self.dptr & 0xFF_FFFF;
                self.acc = self.read_xram(addr) as u16;
            }
            // MOVX @Ri, A / MOVX A, @Ri (8-bit pointer, low byte of DPTR)
            0xF2 => {
                let ri = ((op >> 3) & 1) as usize;
                let addr = (self.regs[ri] & 0xFF) as u32;
                self.write_xram(addr, self.acc as u8);
            }
            0xE2 => {
                let ri = ((op >> 3) & 1) as usize;
                let addr = (self.regs[ri] & 0xFF) as u32;
                self.acc = self.read_xram(addr) as u16;
            }
            // MOV DPTR, #imm24 (0x90)
            0x90 => {
                let addr = self.fetch_u24();
                self.dptr = addr;
            }
            // SJMP (0x80): 8-bit relative
            0x80 => {
                let rel = self.fetch_s8();
                self.pc = ((self.pc as i32).wrapping_add(rel as i32)) as u32 & 0xFF_FFFF;
            }
            // JZ / JNZ
            0x60 => {
                let rel = self.fetch_s8();
                if self.acc == 0 {
                    self.pc = ((self.pc as i32).wrapping_add(rel as i32)) as u32 & 0xFF_FFFF;
                }
            }
            0x70 => {
                let rel = self.fetch_s8();
                if self.acc != 0 {
                    self.pc = ((self.pc as i32).wrapping_add(rel as i32)) as u32 & 0xFF_FFFF;
                }
            }
            // JC / JNC
            0x40 => {
                let rel = self.fetch_s8();
                if self.carry() {
                    self.pc = ((self.pc as i32).wrapping_add(rel as i32)) as u32 & 0xFF_FFFF;
                }
            }
            0x50 => {
                let rel = self.fetch_s8();
                if !self.carry() {
                    self.pc = ((self.pc as i32).wrapping_add(rel as i32)) as u32 & 0xFF_FFFF;
                }
            }
            // CLR A / CPL A
            0xE4 => {
                self.acc = 0;
                self.set_nz(0);
            }
            0xF4 => {
                self.acc = !self.acc;
                self.set_nz(self.acc);
            }
            // RL / RR / RLC / RRC (8-bit rotate on low byte)
            0x23 => {
                let carry = (self.acc >> 15) & 1;
                self.acc = (self.acc << 1) | carry as u16;
                self.set_carry(carry != 0);
            }
            0x03 => {
                let carry = self.acc & 1;
                self.acc = (self.acc >> 1) | ((carry as u16) << 15);
                self.set_carry(carry != 0);
            }
            // MOV A, #imm16 (0x74 variant is regs; use 0xE5 for direct)
            0xE5 => {
                let direct = self.fetch() as usize;
                self.acc = self.read_idram(direct as u8) as u16;
            }
            // Unknown: halt so the caller surfaces it.
            _ => {
                self.halted = true;
            }
        }
    }
}

/// Outcome of a single 80251 step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X51Step {
    Advanced,
    Breakpoint,
    Halted,
}

impl Default for X51Core {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> X51Core {
        X51Core::new()
    }

    fn run(core: &mut X51Core, insns: &[u8], steps: usize) {
        core.load_image(0, insns);
        for _ in 0..steps {
            let r = core.step(&[]);
            if r == X51Step::Halted {
                break;
            }
        }
    }

    #[test]
    fn mov_immediate_to_register() {
        let mut c = core();
        // MOV R4, #0x1234 (0x74, 0x12, 0x34) big-endian immediate
        run(&mut c, &[0x74, 0x12, 0x34], 1);
        assert_eq!(c.regs[4], 0x1234);
    }

    #[test]
    fn mov_acc_from_register() {
        let mut c = core();
        c.regs[0] = 0xABCD;
        // MOV A, R0 (0xE8)
        run(&mut c, &[0xE8], 1);
        assert_eq!(c.acc, 0xABCD);
    }

    #[test]
    fn add_register_sets_carry() {
        let mut c = core();
        c.regs[0] = 0xFFFF;
        c.acc = 1;
        // ADD A, R0 (0x28)
        run(&mut c, &[0x28], 1);
        assert_eq!(c.acc, 0);
        assert!(c.carry(), "0xFFFF + 1 wraps with carry");
        assert!(!c.overflow(), "0xFFFF (-1) + 1 does not sign-overflow");
    }

    #[test]
    fn subb_with_borrow() {
        let mut c = core();
        c.regs[0] = 5;
        c.acc = 10;
        // SUBB A, R0 (0x98)
        run(&mut c, &[0x98], 1);
        assert_eq!(c.acc, 5);
        assert!(!c.carry(), "no borrow");
    }

    #[test]
    fn mul_ab_produces_16bit() {
        let mut c = core();
        c.acc = 0x100;
        c.b = 3;
        // MUL AB (0xA4)
        run(&mut c, &[0xA4], 1);
        assert_eq!(c.acc, 0x300);
        assert_eq!(c.b, 0);
    }

    #[test]
    fn ljmp_24bit_absolute() {
        let mut c = core();
        // LJMP 0x123456 (0x02, 0x12, 0x34, 0x56)
        run(&mut c, &[0x02, 0x12, 0x34, 0x56], 1);
        assert_eq!(c.pc, 0x123456);
    }

    #[test]
    fn lcall_ret_roundtrip() {
        let mut c = core();
        // LCALL 0x000010 (0x12, 0x00, 0x00, 0x10), then RET (0x22)
        run(&mut c, &[0x12, 0x00, 0x00, 0x10, 0x22], 1);
        assert_eq!(c.pc, 0x000010);
        // RET pops the saved PC back.
        let saved = c.pc;
        let _ = saved;
        c.pc = 0x000010;
        c.rom[0x10] = 0x22; // RET at target
        let r = c.step(&[]);
        assert_eq!(r, X51Step::Advanced);
        assert_eq!(c.pc, 4, "RET returns to instruction after LCALL");
    }

    #[test]
    fn mov_dptr_24bit() {
        let mut c = core();
        // MOV DPTR, #0x123456 (0x90, 0x12, 0x34, 0x56)
        run(&mut c, &[0x90, 0x12, 0x34, 0x56], 1);
        assert_eq!(c.dptr, 0x123456);
    }

    #[test]
    fn movx_uses_dptr() {
        let mut c = core();
        c.dptr = 0x1000;
        c.acc = 0xAB;
        // MOVX @DPTR, A (0xF0)
        run(&mut c, &[0xF0], 1);
        assert_eq!(c.read_xram(0x1000), 0xAB);
        // MOVX A, @DPTR (0xE0)
        c.acc = 0;
        run(&mut c, &[0xE0], 1);
        assert_eq!(c.acc, 0xAB);
    }

    #[test]
    fn jz_branch_taken() {
        let mut c = core();
        c.acc = 0;
        // JZ +2 (0x60, 0x02): target = next_pc(2) + 2 = 4
        run(&mut c, &[0x60, 0x02], 1);
        assert_eq!(c.pc, 4);
    }

    #[test]
    fn unknown_opcode_halts() {
        let mut c = core();
        run(&mut c, &[0x05, 0x00], 1);
        assert!(c.halted);
    }
}
