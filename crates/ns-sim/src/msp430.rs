//! A TI MSP430 (classic 16-bit) instruction-level simulator core.
//!
//! Models the classic MSP430 CPU family used by IAR/Keil-style bare-metal
//! MSP430 toolchains: sixteen 16-bit registers (`r0` = PC, `r1` = SP, `r2` =
//! SR, `r3` = constant generator), a 16-bit address space (64 KiB, flat,
//! little-endian), and the two main instruction formats plus the conditional
//! jump group.
//!
//! ## Covered instructions
//!
//! * Format I (two operand), opcodes 4..=F:
//!   `MOV`, `ADD`, `ADDC`, `SUBC`, `SUB`, `CMP`, `DADD`, `BIT`, `BIC`, `BIS`,
//!   `XOR`, `AND`. Both word and byte (`.B`) forms are decoded.
//! * Format II (single operand): `RRC`, `SWPB`, `RRA`, `SXT`, `PUSH`, `CALL`,
//!   `RETI`.
//! * Conditional jumps: `JNE`, `JEQ`, `JNC`, `JC`, `JN`, `JGE`, `JL`, `JMP`.
//! * Addressing modes: register direct, indexed `X(Rn)`, symbolic `X(PC)`,
//!   absolute `&ADDR` (`X(SR)`), indirect `@Rn`, autoincrement `@Rn+`, and
//!   immediate `#N` (`@PC+`). The constant generator is honoured for `R2`
//!   (`#4`, `#8`) and `R3` (`#0`, `#1`, `#2`, `#-1`); writing to `R3` is
//!   discarded.
//!
//! `RET` has no dedicated opcode on the MSP430; the compiler emits
//! `MOV @SP+, PC`, which this core executes through the normal `MOV` path.
//!
//! The `B/W` bit follows silicon: `0` selects a word operation and `1` selects
//! a byte operation (`.B`), so artifacts produced by msp430-gcc/IAR decode
//! correctly.
//!
//! ## Flags
//!
//! `SR` (`r2`) holds `C` (0x0001), `Z` (0x0002), `N` (0x0004), `GIE`
//! (0x0008), and `V` (0x0100). `C`/`Z`/`N`/`V` are maintained for every
//! arithmetic/logical instruction; `MOV` leaves flags untouched. `BIC`, `BIS`,
//! `BIT`, `AND` clear `C` and `V`; `XOR` sets `C` when the result is non-zero.
//!
//! ## Simplifications
//!
//! * `DADD` performs decimal (packed-BCD) addition and reports the decimal
//!   carry in `C`; `V` is left clear because the hardware defines `V` as
//!   undefined for `DADD`.
//! * Interrupt entry/vector dispatch is not modelled. `RETI` restores `SR`
//!   and `PC` from the stack, which is enough to return from an ISR that the
//!   caller entered explicitly.
//! * `SWPB` always operates on a full word (the ISA has no `.B` form).
//! * `PUSH.B` moves `SP` by 2 (as on silicon) and stores only the low byte.
//! * Unbacked memory cannot occur: the 64 KiB address space always exists.
//!   Addresses wrap modulo 64 KiB.
//! * Unknown opcodes set `halted`, so the caller can surface an honest stop
//!   instead of executing garbage.

/// Simulated MSP430 address-space size (64 KiB).
pub const MEMORY_SIZE: usize = 64 * 1024;

/// Typical reset stack pointer for small MSP430 parts (`0x0400`).
///
/// The real value is target-specific; callers that know the device can write
/// `r1`/`SP` after [`Msp430Core::reset`].
pub const RESET_SP: u32 = 0x0400;

const FLAG_C: u16 = 0x0001;
const FLAG_Z: u16 = 0x0002;
const FLAG_N: u16 = 0x0004;
const FLAG_V: u16 = 0x0100;

/// Outcome of a single MSP430 step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Msp430Step {
    Advanced,
    Breakpoint,
    Halted,
}

/// A write destination resolved from the destination addressing mode.
#[derive(Debug, Clone, Copy)]
enum Target {
    Reg(usize),
    Mem(u16),
    Discard,
}

/// A classic MSP430 core.
pub struct Msp430Core {
    /// Register file: `r0` = PC, `r1` = SP, `r2` = SR, `r3` = CG2, `r4..r15`.
    pub r: [u32; 16],
    /// Program counter (kept in sync with `r[0]`).
    pub pc: u32,
    /// 64 KiB flat little-endian memory.
    pub memory: Vec<u8>,
    /// Whether execution should stop.
    pub halted: bool,
    /// Instructions executed since reset.
    pub cycle_count: u64,
}

impl Msp430Core {
    pub fn new() -> Self {
        Self {
            r: [0; 16],
            pc: 0,
            memory: vec![0; MEMORY_SIZE],
            halted: false,
            cycle_count: 0,
        }
    }

    /// Reset the core: zero registers, set `SP = 0x0400`, `SR = 0`, and
    /// `PC = entry`. Memory is preserved.
    pub fn reset(&mut self, entry: u32) {
        self.r = [0; 16];
        self.pc = entry & 0xFFFF;
        self.r[0] = self.pc;
        self.r[1] = RESET_SP;
        self.r[2] = 0;
        self.halted = false;
        self.cycle_count = 0;
    }

    /// Load a program image at `base` and set `PC` to `base`.
    pub fn load_image(&mut self, base: u32, image: &[u8]) {
        self.write_bytes(base, image);
        self.set_pc(base);
        self.halted = false;
    }

    /// Set the program counter (also updates `r[0]`).
    pub fn set_pc(&mut self, pc: u32) {
        self.pc = pc & 0xFFFF;
        self.r[0] = self.pc;
    }

    // -------- memory access --------
    pub fn read_u8(&self, addr: u32) -> u8 {
        self.memory[(addr as usize) & 0xFFFF]
    }

    pub fn read_u16(&self, addr: u32) -> u16 {
        let a = (addr as usize) & 0xFFFF;
        u16::from_le_bytes([self.memory[a], self.memory[(a + 1) & 0xFFFF]])
    }

    pub fn read_u32(&self, addr: u32) -> u32 {
        let mut value = 0u32;
        for i in 0..4 {
            value |= (self.memory[((addr as usize) + i) & 0xFFFF] as u32) << (8 * i);
        }
        value
    }

    pub fn write_u8(&mut self, addr: u32, value: u8) {
        self.memory[(addr as usize) & 0xFFFF] = value;
    }

    pub fn write_u16(&mut self, addr: u32, value: u16) {
        let a = (addr as usize) & 0xFFFF;
        self.memory[a] = value as u8;
        self.memory[(a + 1) & 0xFFFF] = (value >> 8) as u8;
    }

    pub fn write_u32(&mut self, addr: u32, value: u32) {
        for i in 0..4 {
            self.memory[((addr as usize) + i) & 0xFFFF] = (value >> (8 * i)) as u8;
        }
    }

    /// Write a block of bytes (e.g. an ELF segment).
    pub fn write_bytes(&mut self, base: u32, bytes: &[u8]) {
        for (i, byte) in bytes.iter().enumerate() {
            self.memory[((base as usize) + i) & 0xFFFF] = *byte;
        }
    }

    /// Zero-fill `count` bytes starting at `base`.
    pub fn zero_bytes(&mut self, base: u32, count: u32) {
        for i in 0..count as usize {
            self.memory[((base as usize) + i) & 0xFFFF] = 0;
        }
    }

    // -------- registers --------
    pub fn read_reg(&self, index: usize) -> u32 {
        if index == 0 {
            self.pc
        } else {
            self.r[index.min(15)] & 0xFFFF
        }
    }

    pub fn write_reg(&mut self, index: usize, value: u32) {
        if index == 0 {
            self.set_pc(value);
        } else {
            self.r[index.min(15)] = value & 0xFFFF;
        }
    }

    pub fn register_count() -> usize {
        16
    }

    pub fn register_label(index: usize) -> String {
        reg_name(index)
    }

    // -------- internal register helpers --------
    fn reg_read(&self, index: usize) -> u16 {
        if index == 0 {
            self.pc as u16
        } else {
            (self.r[index.min(15)] & 0xFFFF) as u16
        }
    }

    fn reg_write(&mut self, index: usize, value: u16) {
        if index == 0 {
            self.set_pc(value as u32);
        } else {
            self.r[index.min(15)] = value as u32;
        }
    }

    fn flag(&self, mask: u16) -> bool {
        ((self.r[2] & 0xFFFF) as u16) & mask != 0
    }

    fn set_flag(&mut self, mask: u16, value: bool) {
        let mut sr = (self.r[2] & 0xFFFF) as u16;
        if value {
            sr |= mask;
        } else {
            sr &= !mask;
        }
        self.r[2] = sr as u32;
    }

    fn fetch_word(&mut self) -> u16 {
        let word = self.read_u16(self.pc);
        self.pc = (self.pc + 2) & 0xFFFF;
        word
    }

    fn read_mem(&self, addr: u16, byte: bool) -> u16 {
        if byte {
            self.read_u8(addr as u32) as u16
        } else {
            self.read_u16(addr as u32)
        }
    }

    fn write_target(&mut self, target: Target, value: u16, byte: bool) {
        match target {
            Target::Reg(index) => {
                if byte {
                    let current = self.reg_read(index);
                    self.reg_write(index, (current & 0xFF00) | (value & 0x00FF));
                } else {
                    self.reg_write(index, value);
                }
            }
            Target::Mem(addr) => {
                if byte {
                    self.write_u8(addr as u32, value as u8);
                } else {
                    self.write_u16(addr as u32, value);
                }
            }
            Target::Discard => {}
        }
    }

    fn read_target(&self, target: Target, byte: bool) -> u16 {
        match target {
            Target::Reg(index) => self.reg_read(index),
            Target::Mem(addr) => self.read_mem(addr, byte),
            Target::Discard => 0,
        }
    }

    /// Resolve a source operand, applying autoincrement side effects.
    fn read_source(&mut self, reg: usize, as_: u8, byte: bool) -> u16 {
        if reg == 3 {
            return match as_ {
                0 => 0,
                1 => 1,
                2 => 2,
                _ => 0xFFFF,
            };
        }
        if reg == 2 {
            return match as_ {
                0 => self.reg_read(2),
                1 => {
                    let addr = self.fetch_word();
                    self.read_mem(addr, byte)
                }
                2 => 4,
                _ => 8,
            };
        }
        match as_ {
            0 => self.reg_read(reg),
            1 => {
                let base = if reg == 0 {
                    self.pc as u16
                } else {
                    self.reg_read(reg)
                };
                let offset = self.fetch_word();
                let addr = base.wrapping_add(offset);
                self.read_mem(addr, byte)
            }
            2 => {
                let addr = if reg == 0 {
                    self.pc as u16
                } else {
                    self.reg_read(reg)
                };
                self.read_mem(addr, byte)
            }
            _ => {
                if reg == 0 {
                    // Immediate: the extension word is the operand and fetching
                    // it already advanced PC.
                    self.fetch_word()
                } else if reg == 1 {
                    let addr = self.reg_read(1);
                    let value = self.read_mem(addr, byte);
                    self.r[1] = (addr as u32).wrapping_add(2) & 0xFFFF;
                    value
                } else {
                    let addr = self.reg_read(reg);
                    let value = self.read_mem(addr, byte);
                    let increment = if byte { 1 } else { 2 };
                    self.r[reg] = (addr as u32).wrapping_add(increment) & 0xFFFF;
                    value
                }
            }
        }
    }

    /// Resolve a destination operand (consuming its extension word when
    /// indexed/absolute).
    fn resolve_destination(&mut self, reg: usize, ad: u8) -> Target {
        if reg == 3 {
            if ad == 1 {
                let _ = self.fetch_word();
            }
            return Target::Discard;
        }
        if ad == 0 {
            return Target::Reg(reg);
        }
        match reg {
            // Absolute: `&ADDR` is encoded as R2 indexed and ignores SR.
            2 => Target::Mem(self.fetch_word()),
            // Symbolic: PC-relative, base is the extension word address.
            0 => {
                let base = self.pc as u16;
                let offset = self.fetch_word();
                Target::Mem(base.wrapping_add(offset))
            }
            _ => {
                let base = self.reg_read(reg);
                let offset = self.fetch_word();
                Target::Mem(base.wrapping_add(offset))
            }
        }
    }

    /// Resolve a single-operand instruction's operand and write-back target.
    fn resolve_unary(&mut self, reg: usize, as_: u8, byte: bool) -> (u16, Target) {
        if reg == 3 {
            let value = match as_ {
                0 => 0,
                1 => 1,
                2 => 2,
                _ => 0xFFFF,
            };
            return (value, Target::Discard);
        }
        if reg == 2 {
            return match as_ {
                0 => (self.reg_read(2), Target::Reg(2)),
                1 => {
                    let addr = self.fetch_word();
                    (self.read_mem(addr, byte), Target::Mem(addr))
                }
                2 => (4, Target::Discard),
                _ => (8, Target::Discard),
            };
        }
        if reg == 0 {
            return match as_ {
                0 => (self.pc as u16, Target::Reg(0)),
                1 => {
                    let base = self.pc as u16;
                    let offset = self.fetch_word();
                    let addr = base.wrapping_add(offset);
                    (self.read_mem(addr, byte), Target::Mem(addr))
                }
                2 => {
                    let addr = self.pc as u16;
                    (self.read_mem(addr, byte), Target::Mem(addr))
                }
                _ => (self.fetch_word(), Target::Discard),
            };
        }
        let base = self.reg_read(reg);
        match as_ {
            0 => (base, Target::Reg(reg)),
            1 => {
                let offset = self.fetch_word();
                let addr = base.wrapping_add(offset);
                (self.read_mem(addr, byte), Target::Mem(addr))
            }
            2 => (self.read_mem(base, byte), Target::Mem(base)),
            _ => {
                let value = self.read_mem(base, byte);
                // The stack pointer always moves by a full word.
                let increment = if reg == 1 {
                    2
                } else if byte {
                    1
                } else {
                    2
                };
                self.r[reg] = (base as u32).wrapping_add(increment) & 0xFFFF;
                (value, Target::Mem(base))
            }
        }
    }

    fn set_nzcv(&mut self, result: u16, carry: bool, overflow: bool, byte: bool) {
        let sign = if byte { 0x0080 } else { 0x8000 };
        let mask = if byte { 0x00FF } else { 0xFFFF };
        self.set_flag(FLAG_N, result & sign != 0);
        self.set_flag(FLAG_Z, (result & mask) == 0);
        self.set_flag(FLAG_C, carry);
        self.set_flag(FLAG_V, overflow);
    }

    fn set_logic_flags(&mut self, result: u16, byte: bool) {
        let sign = if byte { 0x0080 } else { 0x8000 };
        let mask = if byte { 0x00FF } else { 0xFFFF };
        self.set_flag(FLAG_N, result & sign != 0);
        self.set_flag(FLAG_Z, (result & mask) == 0);
        self.set_flag(FLAG_C, false);
        self.set_flag(FLAG_V, false);
    }

    // -------- execution --------
    /// Execute one instruction. Returns what happened.
    pub fn step(&mut self, breakpoints: &[u32]) -> Msp430Step {
        if self.halted {
            return Msp430Step::Halted;
        }
        if breakpoints.contains(&self.pc) {
            return Msp430Step::Breakpoint;
        }
        let insn = self.fetch_word();
        if (insn & 0xE000) == 0x2000 {
            self.execute_jump(insn);
        } else {
            match insn & 0xF000 {
                0x1000 => self.execute_single(insn),
                0x4000..=0xF000 => self.execute_format1(insn),
                _ => self.halted = true,
            }
        }
        self.r[0] = self.pc;
        self.cycle_count = self.cycle_count.wrapping_add(1);
        if self.halted {
            Msp430Step::Halted
        } else {
            Msp430Step::Advanced
        }
    }

    fn execute_format1(&mut self, insn: u16) {
        let op = (insn >> 12) & 0xF;
        let src_reg = ((insn >> 8) & 0xF) as usize;
        let ad = ((insn >> 7) & 1) as u8;
        let byte = (insn >> 6) & 1 == 1;
        let as_ = ((insn >> 4) & 3) as u8;
        let dst_reg = (insn & 0xF) as usize;

        let src = self.read_source(src_reg, as_, byte);
        let target = self.resolve_destination(dst_reg, ad);
        let dst = self.read_target(target, byte);
        let carry_in: u16 = if self.flag(FLAG_C) { 1 } else { 0 };

        match op {
            // MOV: affects no flags.
            0x4 => self.write_target(target, src, byte),
            // ADD
            0x5 => {
                let (result, carry, overflow) = add_values(dst, src, 0, byte);
                self.set_nzcv(result, carry, overflow, byte);
                self.write_target(target, result, byte);
            }
            // ADDC
            0x6 => {
                let (result, carry, overflow) = add_values(dst, src, carry_in, byte);
                self.set_nzcv(result, carry, overflow, byte);
                self.write_target(target, result, byte);
            }
            // SUBC: dst - src - (1 - C)
            0x7 => {
                let borrow_in = 1 - carry_in;
                let (result, carry, overflow) = sub_values(dst, src, borrow_in, byte);
                self.set_nzcv(result, carry, overflow, byte);
                self.write_target(target, result, byte);
            }
            // SUB
            0x8 => {
                let (result, carry, overflow) = sub_values(dst, src, 0, byte);
                self.set_nzcv(result, carry, overflow, byte);
                self.write_target(target, result, byte);
            }
            // CMP: flags only, destination unchanged.
            0x9 => {
                let (result, carry, overflow) = sub_values(dst, src, 0, byte);
                self.set_nzcv(result, carry, overflow, byte);
            }
            // DADD: packed-BCD addition.
            0xA => {
                let (result, carry, _) = dadd_values(dst, src, carry_in, byte);
                self.set_nzcv(result, carry, false, byte);
                self.write_target(target, result, byte);
            }
            // BIT: flags from dst & src, no write.
            0xB => {
                let mask = if byte { 0x00FF } else { 0xFFFF };
                let result = (dst & src) & mask;
                self.set_nzcv(result, false, false, byte);
            }
            // BIC
            0xC => {
                let mask = if byte { 0x00FF } else { 0xFFFF };
                let result = (dst & !src) & mask;
                self.set_logic_flags(result, byte);
                self.write_target(target, result, byte);
            }
            // BIS
            0xD => {
                let mask = if byte { 0x00FF } else { 0xFFFF };
                let result = (dst | src) & mask;
                self.set_logic_flags(result, byte);
                self.write_target(target, result, byte);
            }
            // XOR
            0xE => {
                let mask = if byte { 0x00FF } else { 0xFFFF };
                let result = (dst ^ src) & mask;
                let sign = if byte { 0x0080 } else { 0x8000 };
                self.set_flag(FLAG_N, result & sign != 0);
                self.set_flag(FLAG_Z, result == 0);
                self.set_flag(FLAG_C, result != 0);
                self.set_flag(FLAG_V, false);
                self.write_target(target, result, byte);
            }
            // AND
            0xF => {
                let mask = if byte { 0x00FF } else { 0xFFFF };
                let result = (dst & src) & mask;
                self.set_logic_flags(result, byte);
                self.write_target(target, result, byte);
            }
            _ => self.halted = true,
        }
    }

    fn execute_single(&mut self, insn: u16) {
        let op = insn & 0xFF80;
        let byte = (insn >> 6) & 1 == 1;
        let as_ = ((insn >> 4) & 3) as u8;
        let reg = (insn & 0xF) as usize;

        match op {
            // RRC: carry into MSB, LSB into carry.
            0x1000 => {
                let (value, target) = self.resolve_unary(reg, as_, byte);
                let carry = self.flag(FLAG_C);
                let result = if byte {
                    let low = value & 0x00FF;
                    ((low >> 1) | ((carry as u16) << 7)) & 0x00FF
                } else {
                    (value >> 1) | ((carry as u16) << 15)
                };
                let new_carry = value & 1 != 0;
                let sign = if byte { 0x0080 } else { 0x8000 };
                let n = result & sign != 0;
                let z = if byte {
                    result & 0x00FF == 0
                } else {
                    result == 0
                };
                self.set_flag(FLAG_C, new_carry);
                self.set_flag(FLAG_N, n);
                self.set_flag(FLAG_Z, z);
                self.set_flag(FLAG_V, n ^ new_carry);
                self.write_target(target, result, byte);
            }
            // SWPB: swap the two bytes, no flag changes.
            0x1080 => {
                let (value, target) = self.resolve_unary(reg, as_, false);
                self.write_target(target, value.rotate_left(8), false);
            }
            // RRA: arithmetic right shift, MSB preserved.
            0x1100 => {
                let (value, target) = self.resolve_unary(reg, as_, byte);
                let result = if byte {
                    let low = value & 0x00FF;
                    ((low >> 1) | (low & 0x0080)) & 0x00FF
                } else {
                    (value >> 1) | (value & 0x8000)
                };
                let new_carry = value & 1 != 0;
                let sign = if byte { 0x0080 } else { 0x8000 };
                let n = result & sign != 0;
                let z = if byte {
                    result & 0x00FF == 0
                } else {
                    result == 0
                };
                self.set_flag(FLAG_C, new_carry);
                self.set_flag(FLAG_N, n);
                self.set_flag(FLAG_Z, z);
                self.set_flag(FLAG_V, n ^ new_carry);
                self.write_target(target, result, byte);
            }
            // SXT: sign-extend the low byte to a word.
            0x1180 => {
                let (value, target) = self.resolve_unary(reg, as_, false);
                let result = (value as u8 as i8 as i16) as u16;
                self.set_flag(FLAG_C, result != 0);
                self.set_flag(FLAG_N, result & 0x8000 != 0);
                self.set_flag(FLAG_Z, result == 0);
                self.set_flag(FLAG_V, false);
                self.write_target(target, result, false);
            }
            // PUSH: SP -= 2; [SP] = operand.
            0x1200 => {
                let (value, _) = self.resolve_unary(reg, as_, byte);
                let sp = self.reg_read(1).wrapping_sub(2);
                self.r[1] = sp as u32;
                if byte {
                    self.write_u8(sp as u32, value as u8);
                } else {
                    self.write_u16(sp as u32, value);
                }
            }
            // CALL: push the return address and jump.
            0x1280 => {
                let (value, _) = self.resolve_unary(reg, as_, false);
                let return_pc = self.pc as u16;
                let sp = self.reg_read(1).wrapping_sub(2);
                self.r[1] = sp as u32;
                self.write_u16(sp as u32, return_pc);
                self.set_pc(value as u32);
            }
            // RETI: pop SR then PC.
            0x1300 => {
                let sp = self.reg_read(1);
                let sr = self.read_u16(sp as u32);
                let pc = self.read_u16(sp.wrapping_add(2) as u32);
                self.r[1] = sp.wrapping_add(4) as u32;
                self.r[2] = sr as u32;
                self.set_pc(pc as u32);
            }
            _ => self.halted = true,
        }
    }

    fn execute_jump(&mut self, insn: u16) {
        let condition = (insn >> 10) & 7;
        let offset = insn & 0x3FF;
        let take = match condition {
            0 => !self.flag(FLAG_Z),                       // JNE
            1 => self.flag(FLAG_Z),                        // JEQ
            2 => !self.flag(FLAG_C),                       // JNC
            3 => self.flag(FLAG_C),                        // JC
            4 => self.flag(FLAG_N),                        // JN
            5 => self.flag(FLAG_N) == self.flag(FLAG_V),   // JGE
            6 => self.flag(FLAG_N) != self.flag(FLAG_V),   // JL
            _ => true,                                     // JMP
        };
        if take {
            let delta = (((offset << 6) as i16) >> 6) as i32 * 2;
            let target = (self.pc as i32).wrapping_add(delta) as u16;
            self.set_pc(target as u32);
        }
    }
}

fn add_values(a: u16, b: u16, carry_in: u16, byte: bool) -> (u16, bool, bool) {
    if byte {
        let a8 = (a & 0x00FF) as u32;
        let b8 = (b & 0x00FF) as u32;
        let sum = a8 + b8 + carry_in as u32;
        let result = (sum & 0x00FF) as u16;
        let carry = sum > 0x00FF;
        let overflow = ((a8 as u8 ^ result as u8) & (b8 as u8 ^ result as u8) & 0x80) != 0;
        (result, carry, overflow)
    } else {
        let sum = a as u32 + b as u32 + carry_in as u32;
        let result = (sum & 0xFFFF) as u16;
        let carry = sum > 0xFFFF;
        let overflow = ((a ^ result) & (b ^ result) & 0x8000) != 0;
        (result, carry, overflow)
    }
}

fn sub_values(a: u16, b: u16, borrow_in: u16, byte: bool) -> (u16, bool, bool) {
    if byte {
        let a8 = (a & 0x00FF) as i32;
        let b8 = (b & 0x00FF) as i32;
        let diff = a8 - b8 - borrow_in as i32;
        let result = (diff & 0x00FF) as u16;
        let carry = diff >= 0;
        let overflow =
            ((a8 as u8 ^ b8 as u8) & (a8 as u8 ^ result as u8) & 0x80) != 0;
        (result, carry, overflow)
    } else {
        let diff = a as i32 - b as i32 - borrow_in as i32;
        let result = (diff & 0xFFFF) as u16;
        let carry = diff >= 0;
        let overflow = ((a ^ b) & (a ^ result) & 0x8000) != 0;
        (result, carry, overflow)
    }
}

fn dadd_values(a: u16, b: u16, carry_in: u16, byte: bool) -> (u16, bool, bool) {
    let nibbles: u32 = if byte { 2 } else { 4 };
    let mask: u16 = if byte { 0x00FF } else { 0xFFFF };
    let mut result = 0u16;
    let mut carry = carry_in != 0;
    for i in 0..nibbles {
        let da = (a >> (4 * i)) & 0xF;
        let db = (b >> (4 * i)) & 0xF;
        let mut sum = da + db + carry as u16;
        carry = false;
        if sum > 9 {
            sum += 6;
        }
        if sum > 0xF {
            carry = true;
            sum &= 0xF;
        }
        result |= sum << (4 * i);
    }
    (result & mask, carry, false)
}

impl Default for Msp430Core {
    fn default() -> Self {
        Self::new()
    }
}

fn reg_name(index: usize) -> String {
    match index {
        0 => "PC".to_string(),
        1 => "SP".to_string(),
        2 => "SR".to_string(),
        3 => "CG2".to_string(),
        _ => format!("R{index}"),
    }
}

/// Disassemble the instruction at `pc` using `read` for little-endian bytes.
///
/// Extension words are consumed as required by the addressing modes, so the
/// output is a readable one-line rendering such as `MOV #0x1234, R5`.
pub fn disassemble(pc: u32, read: &dyn Fn(u32) -> u8) -> String {
    let mut decoder = Decoder {
        read,
        next: pc & 0xFFFF,
    };
    let insn = decoder.fetch();
    let top = insn & 0xF000;

    if top == 0x1000 {
        let op = insn & 0xFF80;
        if op == 0x1300 {
            return "RETI".to_string();
        }
        let byte = (insn >> 6) & 1 == 1;
        let as_ = ((insn >> 4) & 3) as u8;
        let reg = (insn & 0xF) as usize;
        let (name, uses_byte) = match op {
            0x1000 => ("RRC", byte),
            0x1080 => ("SWPB", false),
            0x1100 => ("RRA", byte),
            0x1180 => ("SXT", false),
            0x1200 => ("PUSH", byte),
            0x1280 => ("CALL", false),
            _ => return format!("DW 0x{insn:04X}"),
        };
        let suffix = if uses_byte { ".B" } else { "" };
        let operand = decoder.source(reg, as_);
        return format!("{name}{suffix} {operand}");
    }

    if (insn & 0xE000) == 0x2000 {
        let condition = ((insn >> 10) & 7) as usize;
        let offset = insn & 0x3FF;
        let delta = (((offset << 6) as i16) >> 6) as i32 * 2;
        let target = (decoder.next as i32).wrapping_add(delta) as u16;
        let names = ["JNE", "JEQ", "JNC", "JC", "JN", "JGE", "JL", "JMP"];
        return format!("{} 0x{:04X}", names[condition], target);
    }

    if top >= 0x4000 {
        let names = [
            "?", "?", "?", "?", "MOV", "ADD", "ADDC", "SUBC", "SUB", "CMP", "DADD",
            "BIT", "BIC", "BIS", "XOR", "AND",
        ];
        let op = ((insn >> 12) & 0xF) as usize;
        let byte = (insn >> 6) & 1 == 1;
        let as_ = ((insn >> 4) & 3) as u8;
        let src_reg = ((insn >> 8) & 0xF) as usize;
        let ad = ((insn >> 7) & 1) as u8;
        let dst_reg = (insn & 0xF) as usize;
        let suffix = if byte { ".B" } else { "" };
        let src = decoder.source(src_reg, as_);
        let dst = decoder.destination(dst_reg, ad);
        return format!("{}{} {}, {}", names[op], suffix, src, dst);
    }

    format!("DW 0x{insn:04X}")
}

struct Decoder<'a> {
    read: &'a dyn Fn(u32) -> u8,
    next: u32,
}

impl Decoder<'_> {
    fn word_at(&self, addr: u32) -> u16 {
        let a = addr & 0xFFFF;
        (self.read)(a) as u16 | (((self.read)((a + 1) & 0xFFFF) as u16) << 8)
    }

    fn fetch(&mut self) -> u16 {
        let word = self.word_at(self.next);
        self.next = (self.next + 2) & 0xFFFF;
        word
    }

    fn source(&mut self, reg: usize, as_: u8) -> String {
        match reg {
            3 => match as_ {
                0 => "#0".to_string(),
                1 => "#1".to_string(),
                2 => "#2".to_string(),
                _ => "#0xFFFF".to_string(),
            },
            2 => match as_ {
                0 => reg_name(2),
                1 => {
                    let addr = self.fetch();
                    format!("&0x{addr:04X}")
                }
                2 => "#4".to_string(),
                _ => "#8".to_string(),
            },
            _ => {
                let name = reg_name(reg);
                match as_ {
                    0 => name,
                    1 => {
                        let base = if reg == 0 { self.next as u16 } else { 0 };
                        let offset = self.fetch();
                        if reg == 0 {
                            format!("&0x{:04X}", base.wrapping_add(offset))
                        } else {
                            format!("0x{offset:04X}({name})")
                        }
                    }
                    2 => format!("@{name}"),
                    _ => {
                        if reg == 0 {
                            let value = self.fetch();
                            format!("#0x{value:04X}")
                        } else {
                            format!("@{name}+")
                        }
                    }
                }
            }
        }
    }

    fn destination(&mut self, reg: usize, ad: u8) -> String {
        if ad == 0 {
            return reg_name(reg);
        }
        if reg == 3 {
            let _ = self.fetch();
            return reg_name(3);
        }
        match reg {
            2 => {
                let addr = self.fetch();
                format!("&0x{addr:04X}")
            }
            0 => {
                let base = self.next as u16;
                let offset = self.fetch();
                format!("&0x{:04X}", base.wrapping_add(offset))
            }
            _ => {
                let offset = self.fetch();
                format!("0x{offset:04X}({})", reg_name(reg))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f1(op: u16, src: usize, ad: u16, byte: bool, as_: u16, dst: usize) -> u16 {
        (op << 12)
            | ((src as u16) << 8)
            | (ad << 7)
            | ((byte as u16) << 6)
            | (as_ << 4)
            | (dst as u16)
    }

    fn f2(base: u16, byte: bool, as_: u16, reg: usize) -> u16 {
        base | ((byte as u16) << 6) | (as_ << 4) | (reg as u16)
    }

    fn program(words: &[u16]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(words.len() * 2);
        for word in words {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    fn core_with(words: &[u16]) -> Msp430Core {
        let mut core = Msp430Core::new();
        core.reset(0x0200);
        core.write_bytes(0x0200, &program(words));
        core
    }

    fn run(core: &mut Msp430Core, steps: usize) {
        for _ in 0..steps {
            if core.step(&[]) != Msp430Step::Advanced {
                break;
            }
        }
    }

    fn sr_flag(core: &Msp430Core, mask: u16) -> bool {
        (core.r[2] as u16) & mask != 0
    }

    #[test]
    fn reset_sets_sp_sr_pc_and_register_metadata() {
        assert_eq!(Msp430Core::register_count(), 16);
        assert_eq!(Msp430Core::register_label(0), "PC");
        assert_eq!(Msp430Core::register_label(1), "SP");
        assert_eq!(Msp430Core::register_label(2), "SR");
        assert_eq!(Msp430Core::register_label(3), "CG2");
        assert_eq!(Msp430Core::register_label(4), "R4");
        assert_eq!(Msp430Core::register_label(15), "R15");

        let mut core = Msp430Core::new();
        core.reset(0x0200);
        assert_eq!(core.pc, 0x0200);
        assert_eq!(core.r[1], RESET_SP);
        assert_eq!(core.r[2], 0);
        assert_eq!(core.read_reg(0), 0x0200);
        core.write_reg(0, 0x0300);
        assert_eq!(core.pc, 0x0300);
        assert_eq!(core.r[0], 0x0300);
        core.write_reg(5, 0xBEEF);
        assert_eq!(core.read_reg(5), 0xBEEF);
    }

    #[test]
    fn mov_immediate_to_register() {
        let mut core = core_with(&[f1(4, 0, 0, false, 3, 5), 0x1234]);
        assert_eq!(core.step(&[]), Msp430Step::Advanced);
        assert_eq!(core.read_reg(5), 0x1234);
        assert_eq!(core.pc, 0x0204);
    }

    #[test]
    fn mov_indexed_and_absolute() {
        let mut core = Msp430Core::new();
        core.reset(0x0200);
        core.r[4] = 0x02F0;
        core.write_u16(0x0300, 0xBEEF);
        core.write_u16(0x0302, 0xCAFE);
        core.write_u16(0x0200, f1(4, 4, 0, false, 1, 5));
        core.write_u16(0x0202, 0x0010);
        core.write_u16(0x0204, f1(4, 2, 0, false, 1, 6));
        core.write_u16(0x0206, 0x0302);
        core.write_u16(0x0208, f1(4, 5, 1, false, 0, 2));
        core.write_u16(0x020A, 0x0304);
        run(&mut core, 3);
        assert_eq!(core.read_reg(5), 0xBEEF, "indexed X(R4)");
        assert_eq!(core.read_reg(6), 0xCAFE, "absolute &0x0302");
        assert_eq!(core.read_u16(0x0304), 0xBEEF, "absolute destination");
    }

    #[test]
    fn addc_chains_carry() {
        let mut core = core_with(&[
            f1(4, 0, 0, false, 3, 4),
            0xFFFF,
            f1(4, 0, 0, false, 3, 5),
            0x0001,
            f1(5, 5, 0, false, 0, 4),
            f1(6, 5, 0, false, 0, 6),
        ]);
        run(&mut core, 4);
        assert_eq!(core.read_reg(4), 0, "0xFFFF + 1 wraps to 0");
        assert_eq!(core.read_reg(6), 2, "ADDC adds src plus the previous carry");
        assert!(!sr_flag(&core, FLAG_C), "carry consumed by ADDC");
    }

    #[test]
    fn sub_and_cmp_flags() {
        let mut core = core_with(&[f1(8, 0, 0, false, 3, 4), 0x0005]);
        core.r[4] = 3;
        run(&mut core, 1);
        assert_eq!(core.read_reg(4), 0xFFFE);
        assert!(!sr_flag(&core, FLAG_C), "3 - 5 borrows");
        assert!(sr_flag(&core, FLAG_N));
        assert!(!sr_flag(&core, FLAG_Z));
        assert!(!sr_flag(&core, FLAG_V));

        let mut overflow = core_with(&[f1(8, 0, 0, false, 3, 4), 0xFFFF]);
        overflow.r[4] = 0x7FFF;
        run(&mut overflow, 1);
        assert_eq!(overflow.read_reg(4), 0x8000);
        assert!(sr_flag(&overflow, FLAG_V), "positive minus negative overflows");
        assert!(sr_flag(&overflow, FLAG_N));
        assert!(!sr_flag(&overflow, FLAG_C));

        let mut zero = core_with(&[f1(9, 4, 0, false, 0, 4)]);
        zero.r[4] = 0x1234;
        run(&mut zero, 1);
        assert_eq!(zero.read_reg(4), 0x1234, "CMP does not write");
        assert!(sr_flag(&zero, FLAG_Z));
        assert!(sr_flag(&zero, FLAG_C), "equal compare does not borrow");
        assert!(!sr_flag(&zero, FLAG_N));
    }

    #[test]
    fn byte_operations_preserve_high_byte() {
        let mut core = core_with(&[
            f1(4, 0, 0, true, 3, 4),
            0x0034,
            f1(5, 0, 0, true, 3, 4),
            0x0001,
            f1(5, 0, 0, true, 3, 4),
            0x00CB,
        ]);
        core.r[4] = 0xABCD;
        run(&mut core, 3);
        assert_eq!(core.read_reg(4), 0xAB00, "low byte wraps, high byte kept");
        assert!(sr_flag(&core, FLAG_C));
        assert!(sr_flag(&core, FLAG_Z));
        assert!(!sr_flag(&core, FLAG_N));
    }

    #[test]
    fn push_and_pop_through_stack() {
        let mut core = core_with(&[f2(0x1200, false, 0, 4), f1(4, 1, 0, false, 3, 5)]);
        core.r[4] = 0x1234;
        run(&mut core, 1);
        assert_eq!(core.r[1], 0x03FE);
        assert_eq!(core.read_u16(0x03FE), 0x1234);
        run(&mut core, 1);
        assert_eq!(core.read_reg(5), 0x1234);
        assert_eq!(core.r[1], 0x0400, "MOV @SP+ restores SP");
    }

    #[test]
    fn call_and_return_via_mov() {
        let mut core = Msp430Core::new();
        core.reset(0x0200);
        core.write_u16(0x0200, f2(0x1280, false, 3, 0));
        core.write_u16(0x0202, 0x0210);
        core.write_u16(0x0204, f1(4, 0, 0, false, 3, 5));
        core.write_u16(0x0206, 0x00AA);
        core.write_u16(0x0210, f1(4, 1, 0, false, 3, 0));
        assert_eq!(core.step(&[]), Msp430Step::Advanced);
        assert_eq!(core.pc, 0x0210);
        assert_eq!(core.r[1], 0x03FE);
        assert_eq!(core.read_u16(0x03FE), 0x0204);
        assert_eq!(core.step(&[]), Msp430Step::Advanced);
        assert_eq!(core.pc, 0x0204, "MOV @SP+, PC returns");
        assert_eq!(core.r[1], 0x0400);
        assert_eq!(core.step(&[]), Msp430Step::Advanced);
        assert_eq!(core.read_reg(5), 0xAA);
    }

    #[test]
    fn conditional_jump_taken_and_not_taken() {
        let words = [
            f1(9, 5, 0, false, 0, 4),
            0x2402,
            f1(4, 0, 0, false, 3, 6),
            0xDEAD,
            f1(4, 0, 0, false, 3, 6),
            0xBEEF,
        ];
        let mut taken = core_with(&words);
        taken.r[4] = 5;
        taken.r[5] = 5;
        run(&mut taken, 3);
        assert_eq!(taken.read_reg(6), 0xBEEF, "JEQ skips the first MOV");

        let mut not_taken = core_with(&words);
        not_taken.r[4] = 5;
        not_taken.r[5] = 6;
        run(&mut not_taken, 3);
        assert_eq!(not_taken.read_reg(6), 0xDEAD, "JEQ falls through");
    }

    #[test]
    fn autoincrement_word_and_byte() {
        let mut core = core_with(&[
            f1(4, 4, 0, false, 3, 5),
            f1(4, 4, 0, false, 3, 6),
            f1(4, 4, 0, true, 3, 7),
        ]);
        core.r[4] = 0x0300;
        core.write_u16(0x0300, 0x1111);
        core.write_u16(0x0302, 0x2222);
        core.write_u8(0x0304, 0x33);
        run(&mut core, 3);
        assert_eq!(core.read_reg(5), 0x1111);
        assert_eq!(core.read_reg(6), 0x2222);
        assert_eq!(core.read_reg(7), 0x33);
        assert_eq!(core.r[4], 0x0305, "word +2 twice, byte +1");
    }

    #[test]
    fn constant_generator_values_and_discard() {
        let mut core = core_with(&[
            f1(4, 3, 0, false, 0, 4),
            f1(4, 3, 0, false, 1, 5),
            f1(4, 3, 0, false, 2, 6),
            f1(4, 3, 0, false, 3, 7),
            f1(4, 2, 0, false, 2, 8),
            f1(4, 2, 0, false, 3, 9),
            f1(5, 0, 0, false, 3, 3),
            0x0001,
        ]);
        run(&mut core, 7);
        assert_eq!(core.read_reg(4), 0);
        assert_eq!(core.read_reg(5), 1);
        assert_eq!(core.read_reg(6), 2);
        assert_eq!(core.read_reg(7), 0xFFFF);
        assert_eq!(core.read_reg(8), 4);
        assert_eq!(core.read_reg(9), 8);
        assert_eq!(core.r[3], 0, "writes to R3 are discarded");
        assert_eq!(core.pc, 0x0210);
    }

    #[test]
    fn breakpoint_stops_before_execute() {
        let mut core = core_with(&[f1(4, 0, 0, false, 3, 4), 0x0001]);
        assert_eq!(core.step(&[0x0200]), Msp430Step::Breakpoint);
        assert_eq!(core.pc, 0x0200);
        assert_eq!(core.read_reg(4), 0);
        assert_eq!(core.step(&[]), Msp430Step::Advanced);
        assert_eq!(core.read_reg(4), 1);
    }

    #[test]
    fn unknown_opcode_halts() {
        let mut core = core_with(&[0x0000]);
        assert_eq!(core.step(&[]), Msp430Step::Halted);
        assert!(core.halted);
    }

    #[test]
    fn disassemble_formats_common_instructions() {
        let mut memory = vec![0u8; MEMORY_SIZE];
        let mut put = |addr: usize, word: u16| {
            memory[addr] = word as u8;
            memory[addr + 1] = (word >> 8) as u8;
        };
        put(0x0200, f1(4, 0, 0, false, 3, 5));
        put(0x0202, 0x1234);
        put(0x0204, f1(5, 4, 0, false, 0, 5));
        put(0x0206, 0x3C01);
        put(0x0208, f2(0x1200, false, 0, 4));
        put(0x020A, 0x1300);
        put(0x020C, f1(4, 0, 0, true, 3, 4));
        put(0x020E, 0x0034);
        let read = |addr: u32| memory[(addr as usize) & 0xFFFF];
        assert_eq!(disassemble(0x0200, &read), "MOV #0x1234, R5");
        assert_eq!(disassemble(0x0204, &read), "ADD R4, R5");
        assert_eq!(disassemble(0x0206, &read), "JMP 0x020A");
        assert_eq!(disassemble(0x0208, &read), "PUSH R4");
        assert_eq!(disassemble(0x020A, &read), "RETI");
        assert_eq!(disassemble(0x020C, &read), "MOV.B #0x0034, R4");
    }

    #[test]
    fn dadd_adds_packed_bcd() {
        let mut core = core_with(&[
            f1(4, 0, 0, false, 3, 4),
            0x0019,
            f1(4, 0, 0, false, 3, 5),
            0x0001,
            f1(0xA, 5, 0, false, 0, 4),
        ]);
        run(&mut core, 3);
        assert_eq!(core.read_reg(4), 0x0020, "BCD 19 + 01 = 20");
        assert!(!sr_flag(&core, FLAG_C));

        let mut overflow = core_with(&[
            f1(4, 0, 0, false, 3, 4),
            0x9999,
            f1(4, 0, 0, false, 3, 5),
            0x0001,
            f1(0xA, 5, 0, false, 0, 4),
        ]);
        run(&mut overflow, 3);
        assert_eq!(overflow.read_reg(4), 0x0000, "BCD 9999 + 1 = 0000");
        assert!(sr_flag(&overflow, FLAG_C), "decimal carry out");
        assert!(sr_flag(&overflow, FLAG_Z));
    }

    #[test]
    fn loop_sums_one_to_ten() {
        let words = [
            f1(4, 0, 0, false, 3, 4), // MOV #0, R4
            0x0000,
            f1(4, 0, 0, false, 3, 5), // MOV #1, R5
            0x0001,
            f1(5, 5, 0, false, 0, 4), // loop: ADD R5, R4
            f1(5, 0, 0, false, 3, 5), // ADD #1, R5
            0x0001,
            f1(9, 0, 0, false, 3, 5), // CMP #11, R5
            0x000B,
            0x23FA,                   // JNE loop
            f1(4, 4, 0, false, 0, 7), // MOV R4, R7
            0x3FFF,                   // JMP $ (park)
        ];
        let mut core = core_with(&words);
        for _ in 0..300 {
            assert_eq!(core.step(&[]), Msp430Step::Advanced);
        }
        assert_eq!(core.read_reg(4), 55);
        assert_eq!(core.read_reg(7), 55);
        assert_eq!(core.pc, 0x0216, "parked on the self jump");
    }
}
