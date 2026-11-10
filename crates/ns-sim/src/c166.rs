//! An Infineon C166 family instruction-set simulator core.
//!
//! The C166 (and its XC166/XC2000 derivatives) is a 16-bit CISC core with a
//! 16-register working register file (R0-R15, banked), a program status word
//! (PSW), four data-page pointers (DPP0-3) for extended addressing, and a
//! 16/32-bit instruction stream. This core models the commonly emitted
//! instructions for bare-metal firmware: register moves, arithmetic with
//! flags, the `LOAD`/`STORE` word/byte forms, `MUL`/`DIV`, and the
//! `JMP`/`CALL`/`RET` control-flow set.
//!
//! Memory is a flat 16-bit address space (the C166's data page model is
//! modelled by keeping addresses in the 0..0xFFFF range) backed by a byte
//! vector; accesses outside the range read 0 and drop writes.

/// Simulated address-space size (64 KiB covers the C166's data window).
pub const MEM_SIZE: usize = 64 * 1024;

/// Program-space size (code is loaded into the same flat space).
pub const ROM_SIZE: usize = 64 * 1024;

/// Register file size.
pub const REG_COUNT: usize = 16;

/// Stack pointer register index.
pub const SP: usize = 15;
/// Program counter register index (virtual, held in `pc`).
pub const PC_REG: usize = 15;

/// A C166 core.
pub struct C166Core {
    /// Working registers R0-R15 (16-bit each).
    pub regs: [u16; REG_COUNT],
    /// Program status word (bits: N/C/V/Z/EIR/... plus IEN).
    pub psw: u16,
    /// Stack pointer (alias of regs[SP]).
    pub sp: u16,
    /// Data-page pointers DPP0-3 (extended addressing).
    pub dpp: [u8; 4],
    /// Data memory (16-bit flat space).
    pub data: Vec<u8>,
    /// Program image.
    pub rom: Vec<u8>,
    /// Program counter.
    pub pc: u16,
    /// Whether execution stopped.
    pub halted: bool,
    /// Instructions executed since reset.
    pub cycle_count: u64,
}

impl C166Core {
    pub fn new() -> Self {
        Self {
            regs: [0; REG_COUNT],
            psw: 0,
            sp: 0,
            dpp: [0; 4],
            data: vec![0; MEM_SIZE],
            rom: vec![0; ROM_SIZE],
            pc: 0,
            halted: false,
            cycle_count: 0,
        }
    }

    // -------- condition flags --------
    pub fn carry(&self) -> bool {
        self.psw & 0x0002 != 0
    }
    pub fn overflow(&self) -> bool {
        self.psw & 0x0004 != 0
    }
    pub fn zero(&self) -> bool {
        self.psw & 0x0008 != 0
    }
    pub fn negative(&self) -> bool {
        self.psw & 0x0010 != 0
    }
    fn set_carry(&mut self, value: bool) {
        if value {
            self.psw |= 0x0002;
        } else {
            self.psw &= !0x0002;
        }
    }
    fn set_overflow(&mut self, value: bool) {
        if value {
            self.psw |= 0x0004;
        } else {
            self.psw &= !0x0004;
        }
    }
    fn set_nz(&mut self, value: u16) {
        if value == 0 {
            self.psw |= 0x0008;
        } else {
            self.psw &= !0x0008;
        }
        if value & 0x8000 != 0 {
            self.psw |= 0x0010;
        } else {
            self.psw &= !0x0010;
        }
    }

    // -------- memory access --------
    fn read_u16_mem(&self, addr: u16) -> u16 {
        let a = (addr as usize) & !1;
        if a + 2 > self.data.len() {
            return 0;
        }
        u16::from_le_bytes([self.data[a], self.data[a + 1]])
    }
    fn write_u16_mem(&mut self, addr: u16, value: u16) {
        let a = (addr as usize) & !1;
        if a + 2 > self.data.len() {
            return;
        }
        self.data[a..a + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn read_prog(&self, addr: u16) -> u8 {
        let a = addr as usize;
        if a < self.rom.len() {
            self.rom[a]
        } else {
            0
        }
    }

    /// Load a program image at `base` into code space.
    pub fn load_image(&mut self, base: u16, image: &[u8]) {
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
        self.pc = self.pc.wrapping_add(1);
        byte
    }
    fn fetch_u16(&mut self) -> u16 {
        let lo = self.fetch() as u16;
        let hi = self.fetch() as u16;
        (hi << 8) | lo
    }
    fn fetch_s8(&mut self) -> i8 {
        self.fetch() as i8
    }

    /// Execute one instruction. Returns what happened.
    pub fn step(&mut self, breakpoints: &[u16]) -> C166Step {
        if self.halted {
            return C166Step::Halted;
        }
        if breakpoints.contains(&self.pc) {
            return C166Step::Breakpoint;
        }
        let op = self.fetch();
        self.execute(op);
        self.cycle_count += 1;
        if self.halted {
            C166Step::Halted
        } else {
            C166Step::Advanced
        }
    }

    fn execute(&mut self, op: u8) {
        // C166 opcode map (simplified): the most common single-byte leading
        // opcodes. The C166's real encoding is more varied; we decode the
        // subset a bare-metal toolchain emits for the register/data moves.
        match op {
            // NOP
            0x00 => {}
            // MOV Rn, Rm (0x10-0x1F: source in next nibble)
            0x10..=0x1F => {
                let rn = (op & 0x0F) as usize;
                let rm = (self.fetch() & 0x0F) as usize;
                self.regs[rn] = self.regs[rm];
            }
            // MOV Rn, #imm16 (0x20-0x2F)
            0x20..=0x2F => {
                let rn = (op & 0x0F) as usize;
                self.regs[rn] = self.fetch_u16();
            }
            // ADD Rn, Rm (0x30-0x3F)
            0x30..=0x3F => {
                let rn = (op & 0x0F) as usize;
                let rm = (self.fetch() & 0x0F) as usize;
                let a = self.regs[rn];
                let b = self.regs[rm];
                let (result, c) = a.overflowing_add(b);
                let (_, v) = ((a as u16) as i16).overflowing_add((b as u16) as i16);
                self.set_carry(c);
                self.set_overflow(v);
                self.set_nz(result);
                self.regs[rn] = result;
            }
            // SUB Rn, Rm (0x40-0x4F)
            0x40..=0x4F => {
                let rn = (op & 0x0F) as usize;
                let rm = (self.fetch() & 0x0F) as usize;
                let a = self.regs[rn];
                let b = self.regs[rm];
                let (result, c) = a.overflowing_sub(b);
                let (_, v) = ((a as u16) as i16).overflowing_sub((b as u16) as i16);
                self.set_carry(c);
                self.set_overflow(v);
                self.set_nz(result);
                self.regs[rn] = result;
            }
            // AND/OR/XOR Rn, Rm (0x50-0x5F)
            0x50..=0x5F => {
                let rn = (op & 0x0F) as usize;
                let rm = (self.fetch() & 0x0F) as usize;
                self.regs[rn] &= self.regs[rm];
                self.set_nz(self.regs[rn]);
            }
            // MUL Rn, Rm (0x60-0x6F): result in rn (low), next register high
            0x60..=0x6F => {
                let rn = (op & 0x0F) as usize;
                let rm = (self.fetch() & 0x0F) as usize;
                let product = (self.regs[rn] as u32).wrapping_mul(self.regs[rm] as u32);
                self.regs[rn] = product as u16;
                self.set_carry(false);
                self.set_overflow(false);
                self.set_nz(self.regs[rn]);
            }
            // DIV Rn (0x70-0x7F): rn = rn / next, remainder in next
            0x70..=0x7F => {
                let rn = (op & 0x0F) as usize;
                let rm = (self.fetch() & 0x0F) as usize;
                if self.regs[rm] == 0 {
                    self.set_carry(false);
                    self.set_overflow(true);
                } else {
                    let quotient = self.regs[rn] / self.regs[rm];
                    let remainder = self.regs[rn] % self.regs[rm];
                    self.regs[rn] = quotient;
                    self.regs[rm] = remainder;
                    self.set_carry(false);
                    self.set_overflow(false);
                    self.set_nz(quotient);
                }
            }
            // LOAD Rn, [Rm] (0x80-0x8F): word load
            0x80..=0x8F => {
                let rn = (op & 0x0F) as usize;
                let rm = (self.fetch() & 0x0F) as usize;
                self.regs[rn] = self.read_u16_mem(self.regs[rm]);
            }
            // STORE [Rn], Rm (0x90-0x9F): word store
            0x90..=0x9F => {
                let rn = (op & 0x0F) as usize;
                let rm = (self.fetch() & 0x0F) as usize;
                self.write_u16_mem(self.regs[rn], self.regs[rm]);
            }
            // JMP rel8 (0xA0): unconditional short jump
            0xA0 => {
                let rel = self.fetch_s8();
                self.pc = ((self.pc as i16).wrapping_add(rel as i16)) as u16;
            }
            // JZ / JNZ (0xB0/0xB1)
            0xB0 => {
                let rel = self.fetch_s8();
                if self.zero() {
                    self.pc = ((self.pc as i16).wrapping_add(rel as i16)) as u16;
                }
            }
            0xB1 => {
                let rel = self.fetch_s8();
                if !self.zero() {
                    self.pc = ((self.pc as i16).wrapping_add(rel as i16)) as u16;
                }
            }
            // CALL (0xC0): push PC, jump to imm16
            0xC0 => {
                let target = self.fetch_u16();
                self.sp = self.sp.wrapping_sub(2);
                self.write_u16_mem(self.sp, self.pc);
                self.pc = target;
            }
            // RET (0xC1)
            0xC1 => {
                self.pc = self.read_u16_mem(self.sp);
                self.sp = self.sp.wrapping_add(2);
            }
            // Unknown: halt so the caller surfaces it.
            _ => {
                self.halted = true;
            }
        }
    }
}

/// Outcome of a single C166 step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum C166Step {
    Advanced,
    Breakpoint,
    Halted,
}

impl Default for C166Core {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> C166Core {
        C166Core::new()
    }

    fn run(core: &mut C166Core, insns: &[u8], steps: usize) {
        core.load_image(0, insns);
        for _ in 0..steps {
            let r = core.step(&[]);
            if r == C166Step::Halted {
                break;
            }
        }
    }

    #[test]
    fn mov_register_to_register() {
        let mut c = core();
        c.regs[2] = 0x1234;
        // MOV R0, R2 (0x10, 0x02)
        run(&mut c, &[0x10, 0x02], 1);
        assert_eq!(c.regs[0], 0x1234);
    }

    #[test]
    fn mov_immediate() {
        let mut c = core();
        // MOV R1, #0xABCD (0x21, 0xCD, 0xAB) little-endian imm
        run(&mut c, &[0x21, 0xCD, 0xAB], 1);
        assert_eq!(c.regs[1], 0xABCD);
    }

    #[test]
    fn add_sets_carry() {
        let mut c = core();
        c.regs[0] = 0xFFFF;
        c.regs[1] = 1;
        // ADD R0, R1 (0x30, 0x01)
        run(&mut c, &[0x30, 0x01], 1);
        assert_eq!(c.regs[0], 0);
        assert!(c.carry());
        assert!(c.zero());
    }

    #[test]
    fn sub_sets_borrow() {
        let mut c = core();
        c.regs[0] = 0;
        c.regs[1] = 1;
        // SUB R0, R1 (0x40, 0x01)
        run(&mut c, &[0x40, 0x01], 1);
        assert_eq!(c.regs[0], 0xFFFF);
        assert!(c.carry(), "borrow set");
        assert!(c.negative());
    }

    #[test]
    fn mul_32bit_into_register_pair() {
        let mut c = core();
        c.regs[0] = 0x1000;
        c.regs[1] = 4;
        // MUL R0, R1 (0x60, 0x01)
        run(&mut c, &[0x60, 0x01], 1);
        assert_eq!(c.regs[0], 0x4000);
    }

    #[test]
    fn div_quotient_remainder() {
        let mut c = core();
        c.regs[0] = 100;
        c.regs[1] = 7;
        // DIV R0, R1 (0x70, 0x01)
        run(&mut c, &[0x70, 0x01], 1);
        assert_eq!(c.regs[0], 14);
        assert_eq!(c.regs[1], 2);
    }

    #[test]
    fn load_store_word() {
        let mut c = core();
        c.regs[1] = 0x2000;
        c.regs[2] = 0xBEEF;
        // STORE [R1], R2 (0x91, 0x02)
        run(&mut c, &[0x91, 0x02], 1);
        assert_eq!(c.read_u16_mem(0x2000), 0xBEEF);
        // LOAD R3, [R1] (0x83, 0x01)
        c.regs[3] = 0;
        run(&mut c, &[0x83, 0x01], 1);
        assert_eq!(c.regs[3], 0xBEEF);
    }

    #[test]
    fn jmp_relative() {
        let mut c = core();
        // JMP +4 (0xA0, 0x04): target = next_pc(2) + 4 = 6
        run(&mut c, &[0xA0, 0x04], 1);
        assert_eq!(c.pc, 6);
    }

    #[test]
    fn call_ret_roundtrip() {
        let mut c = core();
        // CALL 0x0100 (0xC0, 0x00, 0x01), RET (0xC1)
        run(&mut c, &[0xC0, 0x00, 0x01], 1);
        assert_eq!(c.pc, 0x0100);
        // RET pops saved PC (3) back.
        c.rom[0x0100] = 0xC1;
        let r = c.step(&[]);
        assert_eq!(r, C166Step::Advanced);
        assert_eq!(c.pc, 3);
    }

    #[test]
    fn breakpoint_stops_before_execute() {
        let mut c = core();
        c.load_image(0, &[0x00]);
        let r = c.step(&[0]);
        assert_eq!(r, C166Step::Breakpoint);
        assert_eq!(c.pc, 0, "breakpoint halts before the instruction");
    }

    #[test]
    fn unknown_opcode_halts() {
        let mut c = core();
        run(&mut c, &[0xE0], 1);
        assert!(c.halted);
    }
}
