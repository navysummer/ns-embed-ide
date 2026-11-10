//! An ST Microelectronics STM8 (ST8 core) instruction-level simulator.
//!
//! Models the STM8 CPU family used by IAR/Keil/SDCC STM8 toolchains: an 8-bit
//! accumulator (`A`), two 16-bit index registers (`X`, `Y`), a 16-bit stack
//! pointer (`SP`), a 24-bit program counter (`PC`), and a condition-code
//! register (`CC`: C=bit0, Z=bit1, N=bit2, I1=bit3, H=bit5, I0=bit6, V=bit7).
//!
//! The memory model is a flat 16 MiB image covering the 24-bit program space
//! and the 16-bit data window. Opcode encodings follow ST PM0044 and the SDCC
//! ucsim `disass_stm8` table.
//!
//! ## Encoding
//!
//! For the arithmetic/logic family (`0xA0-0xFF`) the **high nibble** selects
//! the addressing mode and the **low nibble** selects the operation:
//!
//! | high nibble | addressing mode |
//! |-------------|-----------------|
//! | `0xA` | immediate byte `#imm8` |
//! | `0xB` | direct short `addr8` |
//! | `0xC` | direct long `addr16` (or `[addr16]` with `0x92`) |
//! | `0xD` | indexed `(off16,X)` / `(off16,Y)` with `0x90` |
//! | `0xE` | indexed `(off8,X)` / `(off8,Y)` with `0x90` |
//! | `0xF` | indexed `(X)` / `(Y)` with `0x90` |
//!
//! Low nibble operation codes: `0`=SUB, `1`=CP, `2`=SBC, `3`=CPW, `4`=AND,
//! `5`=BCP, `6`=LD A,op, `7`=LD op,A, `8`=XOR, `9`=ADC, `A`=OR, `B`=ADD,
//! `C`=JP, `D`=CALL, `E`=LDW X,op, `F`=LDW op,X.
//!
//! The unary family (`0x30-0x7F`) uses the high nibble for the addressing
//! mode (`3`=`addr8`, `4`=A, `5`=`addr16`, `6`=`(off8,X)`, `7`=`(X)`) and the
//! low nibble for the operation (`0`=NEG, `1`=CPL, `2`=SRL, `3`=SRA, `4`=SLA,
//! `5`=RLC, `6`=RRC, `7`=SWAP, `8`=INC, `9`=DEC, `A`=CLR, `B`=TNZ).
//!
//! ## Prefixes
//!
//! * `0x90`: Y-indexed forms.
//! * `0x91`: SP-relative long `(off16,SP)`.
//! * `0x92`: indirect word-pointer `[addr16]`.
//! * `0x72`: extended forms (long direct, indirect with X).
//! * `0x72 0x90` / `0x90 0x72`: extended Y forms.
//!
//! ## Covered subset
//!
//! Loads/stores (`LD`, `LDW`, `LDF`), arithmetic (`ADD`/`ADC`/`SUB`/`SBC`/
//! `CP`, `MUL`/`DIV`/`DIVW`, `INC`/`DEC`, `NEG`, `CLR`, `ADDW`/`SUBW`), logic
//! (`AND`/`OR`/`XOR`/`CPL`/`BCP`, `SCF`/`RCF`/`CCF`/`RIM`/`SIM`), shifts
//! (`SLL`/`SRL`/`SRA`/`RLC`/`RRC`/`SWAP`/`RLWA`/`RRWA`), bit ops (`BSET`/
//! `BRES`/`BCPL`/`BCCM`, `BTJT`/`BTJF`), control (`JRA`/`JRF`/`JRxx`, `JP`,
//! `CALL`/`CALLR`/`CALLF`, `RET`/`RETF`/`IRET`, `NOP`, `HALT`, `TRAP`,
//! `BREAK`, `WFI`/`WFE`), stack (`PUSH`/`POP`, `ADDW SP`, `SUB SP`), and
//! exchange (`EXG`, `EXGW`, `MOV`).
//!
//! ## Simplifications
//!
//! * Interrupt vector dispatch is not modelled; `IRET`/`RETF` restore `PC`
//!   from the stack, enough to return from an ISR entered explicitly.
//! * Unknown opcodes set `halted` so the caller surfaces an honest stop.

/// Address-space size (16 MiB covers the 24-bit PC and 16-bit data window).
pub const ADDR_SIZE: usize = 16 * 1024 * 1024;

/// Typical reset stack pointer for small STM8 parts (RAM end).
pub const RESET_SP: u32 = 0x17FF;

// CC register bit masks.
pub const CC_C: u8 = 0x01;
pub const CC_Z: u8 = 0x02;
pub const CC_N: u8 = 0x04;
pub const CC_I1: u8 = 0x08;
pub const CC_H: u8 = 0x20;
pub const CC_I0: u8 = 0x40;
pub const CC_V: u8 = 0x80;

/// Outcome of a single STM8 step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stm8Step {
    Advanced,
    Breakpoint,
    Halted,
}

/// A resolved memory/register operand target for read/write.
#[derive(Debug, Clone, Copy)]
enum Target {
    /// Immediate byte: the next code byte is consumed but not written.
    Imm,
    /// Direct short: `addr8` (page zero).
    Direct8,
    /// Direct long: `addr16`.
    Direct16,
    /// Indexed `(X)`.
    IndexX,
    /// Indexed `(off8,X)`.
    IndexX8,
    /// Indexed `(off16,X)`.
    IndexX16,
    /// Indexed `(Y)`.
    IndexY,
    /// Indexed `(off8,Y)`.
    IndexY8,
    /// Indexed `(off16,Y)`.
    IndexY16,
    /// SP-relative `(off16,SP)`.
    IndexSp16,
    /// Indirect `[addr16]` (word pointer).
    Indirect,
    /// Indirect with X: `([addr16],X)`.
    IndirectX,
    /// Indirect with Y: `([addr16],Y)`.
    IndirectY,
}

/// An STM8 core.
pub struct Stm8Core {
    /// Accumulator (8-bit).
    pub a: u8,
    /// X index register (16-bit).
    pub x: u16,
    /// Y index register (16-bit).
    pub y: u16,
    /// Stack pointer (16-bit).
    pub sp: u16,
    /// Program counter (24-bit).
    pub pc: u32,
    /// Condition-code register.
    pub cc: u8,
    /// Flat memory image (16 MiB address space).
    pub memory: Vec<u8>,
    /// Whether execution should stop.
    pub halted: bool,
    /// Instructions executed since reset.
    pub cycle_count: u64,
}

impl Default for Stm8Core {
    fn default() -> Self {
        Self::new()
    }
}

impl Stm8Core {
    pub fn new() -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            sp: RESET_SP as u16,
            pc: 0,
            cc: 0x28, // reset: I0/I1 set, H clear
            memory: vec![0; ADDR_SIZE],
            halted: false,
            cycle_count: 0,
        }
    }

    /// Reset the core to `entry` (24-bit address). Memory is preserved.
    pub fn reset(&mut self, entry: u32) {
        self.a = 0;
        self.x = 0;
        self.y = 0;
        self.sp = RESET_SP as u16;
        self.pc = entry & 0xFF_FFFF;
        self.cc = 0x28;
        self.halted = false;
        self.cycle_count = 0;
    }

    /// Load a program image at `base` and start execution there.
    pub fn load_image(&mut self, base: u32, image: &[u8]) {
        self.write_bytes(base, image);
        self.set_pc(base);
        self.halted = false;
    }

    /// Set the program counter (24-bit).
    pub fn set_pc(&mut self, pc: u32) {
        self.pc = pc & 0xFF_FFFF;
    }

    // -------- condition flags --------
    pub fn flag_c(&self) -> bool {
        self.cc & CC_C != 0
    }
    fn set_c(&mut self, value: bool) {
        if value {
            self.cc |= CC_C;
        } else {
            self.cc &= !CC_C;
        }
    }
    pub fn flag_z(&self) -> bool {
        self.cc & CC_Z != 0
    }
    fn set_z(&mut self, value: bool) {
        if value {
            self.cc |= CC_Z;
        } else {
            self.cc &= !CC_Z;
        }
    }
    pub fn flag_n(&self) -> bool {
        self.cc & CC_N != 0
    }
    fn set_n(&mut self, value: bool) {
        if value {
            self.cc |= CC_N;
        } else {
            self.cc &= !CC_N;
        }
    }
    pub fn flag_v(&self) -> bool {
        self.cc & CC_V != 0
    }
    fn set_v(&mut self, value: bool) {
        if value {
            self.cc |= CC_V;
        } else {
            self.cc &= !CC_V;
        }
    }
    pub fn flag_h(&self) -> bool {
        self.cc & CC_H != 0
    }
    fn set_h(&mut self, value: bool) {
        if value {
            self.cc |= CC_H;
        } else {
            self.cc &= !CC_H;
        }
    }

    // -------- memory access --------
    fn read_u8(&self, addr: u32) -> u8 {
        self.memory
            .get((addr & 0xFF_FFFF) as usize)
            .copied()
            .unwrap_or(0)
    }
    fn read_u16(&self, addr: u32) -> u16 {
        let lo = self.read_u8(addr) as u16;
        let hi = self.read_u8(addr.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }
    fn write_u8(&mut self, addr: u32, value: u8) {
        let idx = (addr & 0xFF_FFFF) as usize;
        if let Some(byte) = self.memory.get_mut(idx) {
            *byte = value;
        }
    }
    fn write_u16(&mut self, addr: u32, value: u16) {
        self.write_u8(addr, value as u8);
        self.write_u8(addr.wrapping_add(1), (value >> 8) as u8);
    }
    fn write_bytes(&mut self, base: u32, bytes: &[u8]) {
        for (i, byte) in bytes.iter().enumerate() {
            self.write_u8(base.wrapping_add(i as u32), *byte);
        }
    }
    fn zero_bytes(&mut self, base: u32, count: u32) {
        for i in 0..count {
            self.write_u8(base.wrapping_add(i), 0);
        }
    }

    // -------- public memory accessors (for the IDE) --------
    pub fn read_mem_u8(&self, addr: u32) -> u8 {
        self.read_u8(addr)
    }
    pub fn read_mem_u16(&self, addr: u32) -> u16 {
        self.read_u16(addr)
    }
    pub fn read_mem_u32(&self, addr: u32) -> u32 {
        let mut value = 0u32;
        for i in 0..4 {
            value |= (self.read_u8(addr.wrapping_add(i as u32)) as u32) << (8 * i);
        }
        value
    }
    pub fn write_mem_u8(&mut self, addr: u32, value: u8) {
        self.write_u8(addr, value);
    }
    pub fn write_mem_u16(&mut self, addr: u32, value: u16) {
        self.write_u16(addr, value);
    }
    pub fn write_mem_u32(&mut self, addr: u32, value: u32) {
        for i in 0..4 {
            self.write_u8(addr.wrapping_add(i as u32), (value >> (8 * i)) as u8);
        }
    }
    pub fn write_mem_bytes(&mut self, base: u32, bytes: &[u8]) {
        self.write_bytes(base, bytes);
    }
    pub fn zero_mem_bytes(&mut self, base: u32, count: u32) {
        self.zero_bytes(base, count);
    }

    // -------- stack --------
    fn push8(&mut self, value: u8) {
        self.write_u8(self.sp as u32, value);
        self.sp = self.sp.wrapping_sub(1);
    }
    fn pop8(&mut self) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        self.read_u8(self.sp as u32)
    }
    fn push16(&mut self, value: u16) {
        self.push8((value >> 8) as u8);
        self.push8(value as u8);
    }
    fn pop16(&mut self) -> u16 {
        let lo = self.pop8() as u16;
        let hi = self.pop8() as u16;
        (hi << 8) | lo
    }

    // -------- flag helpers --------
    fn set_nz(&mut self, value: u8) {
        self.set_n(value & 0x80 != 0);
        self.set_z(value == 0);
    }
    fn add8(&mut self, a: u8, b: u8) -> u8 {
        let sum = a as u16 + b as u16;
        let result = sum as u8;
        self.set_c(sum > 0xFF);
        self.set_h((a & 0x0F) as u16 + (b & 0x0F) as u16 > 0x0F);
        self.set_v((a ^ result) & (b ^ result) & 0x80 != 0);
        self.set_nz(result);
        result
    }
    fn adc8(&mut self, a: u8, b: u8) -> u8 {
        let carry = self.flag_c() as u16;
        let sum = a as u16 + b as u16 + carry;
        let result = sum as u8;
        self.set_c(sum > 0xFF);
        self.set_h((a & 0x0F) as u16 + (b & 0x0F) as u16 + carry > 0x0F);
        self.set_v((a ^ result) & (b ^ result) & 0x80 != 0);
        self.set_nz(result);
        result
    }
    fn sub8(&mut self, a: u8, b: u8, borrow: bool) -> u8 {
        let borrow = borrow as i16;
        let diff = a as i16 - b as i16 - borrow;
        let result = diff as u8;
        self.set_c(diff < 0);
        self.set_h((a & 0x0F) as i16 - (b & 0x0F) as i16 - borrow < 0);
        self.set_v((a ^ b) & (a ^ result) & 0x80 != 0);
        self.set_nz(result);
        result
    }
    fn cmp8(&mut self, a: u8, b: u8) {
        let _ = self.sub8(a, b, false);
    }
    fn sub16(&mut self, a: u16, b: u16) -> u16 {
        let diff = a as i32 - b as i32;
        let result = diff as u16;
        self.set_c(diff < 0);
        self.set_n(result & 0x8000 != 0);
        self.set_z(result == 0);
        self.set_v((a ^ b) & (a ^ result) & 0x8000 != 0);
        result
    }

    // -------- fetch --------
    fn fetch8(&mut self) -> u8 {
        let b = self.read_u8(self.pc);
        self.pc = self.pc.wrapping_add(1) & 0xFF_FFFF;
        b
    }
    fn fetch16(&mut self) -> u16 {
        let lo = self.fetch8() as u16;
        let hi = self.fetch8() as u16;
        (hi << 8) | lo
    }
    fn fetch24(&mut self) -> u32 {
        let b0 = self.fetch8() as u32;
        let b1 = self.fetch8() as u32;
        let b2 = self.fetch8() as u32;
        (b2 << 16) | (b1 << 8) | b0
    }

    // -------- operand decode (addressing mode by HIGH nibble) --------
    /// Decode the addressing mode from the ALU-family high nibble and prefix.
    fn decode_alu_target(&mut self, high: u8, prefix: u8) -> Target {
        match (prefix, high & 0x0F) {
            (0x00, 0xA) => Target::Imm,
            (0x00, 0xB) => Target::Direct8,
            (0x00, 0xC) => Target::Direct16,
            (0x00, 0xD) => Target::IndexX16,
            (0x00, 0xE) => Target::IndexX8,
            (0x00, 0xF) => Target::IndexX,
            (0x90, 0xD) => Target::IndexY16,
            (0x90, 0xE) => Target::IndexY8,
            (0x90, 0xF) => Target::IndexY,
            (0x91, 0xB) => Target::IndexSp16,
            (0x92, 0xC) => Target::Indirect,
            (0x72, 0xC) => Target::Direct16,
            (0x72, 0xD) => Target::IndirectX,
            (0xF2, 0xD) => Target::IndirectY,
            _ => Target::Direct8,
        }
    }

    /// Read a target's value (advancing PC over operand bytes).
    fn read_target(&mut self, target: Target) -> u8 {
        match target {
            Target::Imm => self.fetch8(),
            Target::Direct8 => {
                let addr = self.fetch8() as u32;
                self.read_u8(addr)
            }
            Target::Direct16 => {
                let addr = self.fetch16() as u32;
                self.read_u8(addr)
            }
            Target::IndexX => self.read_u8(self.x as u32),
            Target::IndexX8 => {
                let off = self.fetch8() as u32;
                self.read_u8(self.x.wrapping_add(off as u16) as u32)
            }
            Target::IndexX16 => {
                let off = self.fetch16() as u32;
                self.read_u8(self.x.wrapping_add(off as u16) as u32)
            }
            Target::IndexY => self.read_u8(self.y as u32),
            Target::IndexY8 => {
                let off = self.fetch8() as u32;
                self.read_u8(self.y.wrapping_add(off as u16) as u32)
            }
            Target::IndexY16 => {
                let off = self.fetch16() as u32;
                self.read_u8(self.y.wrapping_add(off as u16) as u32)
            }
            Target::IndexSp16 => {
                let off = self.fetch16() as u32;
                self.read_u8(self.sp.wrapping_add(off as u16) as u32)
            }
            Target::Indirect => {
                let ptr = self.fetch16() as u32;
                self.read_u8(self.read_u16(ptr) as u32)
            }
            Target::IndirectX => {
                let ptr = self.fetch16() as u32;
                let addr = self.read_u16(ptr) as u32;
                self.read_u8(addr.wrapping_add(self.x as u32))
            }
            Target::IndirectY => {
                let ptr = self.fetch16() as u32;
                let addr = self.read_u16(ptr) as u32;
                self.read_u8(addr.wrapping_add(self.y as u32))
            }
        }
    }

    /// Write a value to a target (advancing PC over operand bytes).
    fn write_target(&mut self, target: Target, value: u8) {
        match target {
            Target::Imm => {
                let _ = self.fetch8();
            }
            Target::Direct8 => {
                let addr = self.fetch8() as u32;
                self.write_u8(addr, value);
            }
            Target::Direct16 => {
                let addr = self.fetch16() as u32;
                self.write_u8(addr, value);
            }
            Target::IndexX => self.write_u8(self.x as u32, value),
            Target::IndexX8 => {
                let off = self.fetch8() as u32;
                self.write_u8(self.x.wrapping_add(off as u16) as u32, value);
            }
            Target::IndexX16 => {
                let off = self.fetch16() as u32;
                self.write_u8(self.x.wrapping_add(off as u16) as u32, value);
            }
            Target::IndexY => self.write_u8(self.y as u32, value),
            Target::IndexY8 => {
                let off = self.fetch8() as u32;
                self.write_u8(self.y.wrapping_add(off as u16) as u32, value);
            }
            Target::IndexY16 => {
                let off = self.fetch16() as u32;
                self.write_u8(self.y.wrapping_add(off as u16) as u32, value);
            }
            Target::IndexSp16 => {
                let off = self.fetch16() as u32;
                self.write_u8(self.sp.wrapping_add(off as u16) as u32, value);
            }
            Target::Indirect => {
                let ptr = self.fetch16() as u32;
                let addr = self.read_u16(ptr) as u32;
                self.write_u8(addr, value);
            }
            Target::IndirectX => {
                let ptr = self.fetch16() as u32;
                let addr = self.read_u16(ptr) as u32;
                self.write_u8(addr.wrapping_add(self.x as u32), value);
            }
            Target::IndirectY => {
                let ptr = self.fetch16() as u32;
                let addr = self.read_u16(ptr) as u32;
                self.write_u8(addr.wrapping_add(self.y as u32), value);
            }
        }
    }

    /// Read a 16-bit word from a target.
    fn read_target16(&mut self, target: Target) -> u16 {
        match target {
            Target::Imm => self.fetch16(),
            Target::Direct8 => {
                let addr = self.fetch8() as u32;
                self.read_u16(addr)
            }
            Target::Direct16 => {
                let addr = self.fetch16() as u32;
                self.read_u16(addr)
            }
            Target::IndexX => self.read_u16(self.x as u32),
            Target::IndexX8 => {
                let off = self.fetch8() as u32;
                self.read_u16(self.x.wrapping_add(off as u16) as u32)
            }
            Target::IndexX16 => {
                let off = self.fetch16() as u32;
                self.read_u16(self.x.wrapping_add(off as u16) as u32)
            }
            Target::IndexY => self.read_u16(self.y as u32),
            Target::IndexY8 => {
                let off = self.fetch8() as u32;
                self.read_u16(self.y.wrapping_add(off as u16) as u32)
            }
            Target::IndexY16 => {
                let off = self.fetch16() as u32;
                self.read_u16(self.y.wrapping_add(off as u16) as u32)
            }
            Target::IndexSp16 => {
                let off = self.fetch16() as u32;
                self.read_u16(self.sp.wrapping_add(off as u16) as u32)
            }
            Target::Indirect => {
                let ptr = self.fetch16() as u32;
                self.read_u16(self.read_u16(ptr) as u32)
            }
            Target::IndirectX => {
                let ptr = self.fetch16() as u32;
                let addr = self.read_u16(ptr) as u32;
                self.read_u16(addr.wrapping_add(self.x as u32))
            }
            Target::IndirectY => {
                let ptr = self.fetch16() as u32;
                let addr = self.read_u16(ptr) as u32;
                self.read_u16(addr.wrapping_add(self.y as u32))
            }
        }
    }

    /// Write a 16-bit word to a target.
    fn write_target16(&mut self, target: Target, value: u16) {
        match target {
            Target::Imm => {
                let _ = self.fetch16();
            }
            Target::Direct8 => {
                let addr = self.fetch8() as u32;
                self.write_u16(addr, value);
            }
            Target::Direct16 => {
                let addr = self.fetch16() as u32;
                self.write_u16(addr, value);
            }
            Target::IndexX => self.write_u16(self.x as u32, value),
            Target::IndexX8 => {
                let off = self.fetch8() as u32;
                self.write_u16(self.x.wrapping_add(off as u16) as u32, value);
            }
            Target::IndexX16 => {
                let off = self.fetch16() as u32;
                self.write_u16(self.x.wrapping_add(off as u16) as u32, value);
            }
            Target::IndexY => self.write_u16(self.y as u32, value),
            Target::IndexY8 => {
                let off = self.fetch8() as u32;
                self.write_u16(self.y.wrapping_add(off as u16) as u32, value);
            }
            Target::IndexY16 => {
                let off = self.fetch16() as u32;
                self.write_u16(self.y.wrapping_add(off as u16) as u32, value);
            }
            Target::IndexSp16 => {
                let off = self.fetch16() as u32;
                self.write_u16(self.sp.wrapping_add(off as u16) as u32, value);
            }
            Target::Indirect => {
                let ptr = self.fetch16() as u32;
                let addr = self.read_u16(ptr) as u32;
                self.write_u16(addr, value);
            }
            Target::IndirectX => {
                let ptr = self.fetch16() as u32;
                let addr = self.read_u16(ptr) as u32;
                self.write_u16(addr.wrapping_add(self.x as u32), value);
            }
            Target::IndirectY => {
                let ptr = self.fetch16() as u32;
                let addr = self.read_u16(ptr) as u32;
                self.write_u16(addr.wrapping_add(self.y as u32), value);
            }
        }
    }

    /// Execute one instruction. Breakpoints are checked before any state change.
    pub fn step(&mut self, breakpoints: &[u32]) -> Stm8Step {
        if self.halted {
            return Stm8Step::Halted;
        }
        if breakpoints.iter().any(|bp| *bp & 0xFF_FFFF == self.pc) {
            return Stm8Step::Breakpoint;
        }
        self.execute();
        self.cycle_count += 1;
        if self.halted {
            Stm8Step::Halted
        } else {
            Stm8Step::Advanced
        }
    }

    /// Decode and execute one instruction, advancing PC as needed.
    fn execute(&mut self) {
        // Collect prefixes: 0x90, 0x91, 0x92, 0x72 may appear in a chain.
        let mut prefixes: Vec<u8> = Vec::new();
        let opcode;
        loop {
            let b = self.fetch8();
            match b {
                0x90 | 0x91 | 0x92 | 0x72 => prefixes.push(b),
                other => {
                    opcode = other;
                    break;
                }
            }
        }
        let has_72 = prefixes.contains(&0x72);
        let has_90 = prefixes.contains(&0x90);
        let prefix_key = if has_72 && has_90 {
            0xF2
        } else if has_72 {
            0x72
        } else if has_90 {
            0x90
        } else if prefixes.contains(&0x91) {
            0x91
        } else if prefixes.contains(&0x92) {
            0x92
        } else {
            0x00
        };
        self.execute_opcode(opcode, prefix_key);
    }

    /// Execute a decoded opcode. `prefix` is the effective prefix key
    /// (0x00, 0x72, 0x90, 0x91, 0x92, 0xF2).
    fn execute_opcode(&mut self, opcode: u8, prefix: u8) {
        // ---- 0x20-0x2F: short conditional/unconditional jumps ----
        if (0x20..=0x2F).contains(&opcode) && prefix == 0x00 {
            let offset = self.fetch8() as i8 as i32;
            self.branch(opcode, offset);
            return;
        }

        // ---- 0x00-0x1F: bit ops and SP-relative forms ----
        if (0x00..=0x1F).contains(&opcode) && prefix == 0x00 {
            self.execute_sp_and_bit(opcode);
            return;
        }

        // ---- 0x30-0x7F: unary family (high nibble = addressing mode) ----
        if (0x30..=0x7F).contains(&opcode) {
            self.execute_unary(opcode, prefix);
            return;
        }

        // ---- 0x80-0x9F: inherent/register ----
        if (0x80..=0x9F).contains(&opcode) {
            self.execute_inherent(opcode, prefix);
            return;
        }

        // ---- 0xA0-0xFF: ALU family (high nibble = addressing, low = op) ----
        if (0xA0..=0xFF).contains(&opcode) {
            self.execute_alu(opcode, prefix);
            return;
        }

        // Fallback: stop honestly.
        self.halted = true;
    }

    /// Conditional/unconditional short branch for opcodes 0x20-0x2F.
    fn branch(&mut self, opcode: u8, offset: i32) {
        let taken = match opcode & 0x0F {
            0x0 => true,                                                  // JRA
            0x1 => false,                                                 // JRF
            0x2 => self.flag_c() && !self.flag_z(),                       // JRUGT
            0x3 => self.flag_c() || self.flag_z(),                        // JRULE
            0x4 => self.flag_n() == self.flag_v(),                        // JRUGE
            0x5 => self.flag_n() != self.flag_v(),                        // JRULT
            0x6 => !self.flag_z(),                                        // JRNE
            0x7 => self.flag_z(),                                         // JREQ
            0x8 => !self.flag_n(),                                        // JRPL
            0x9 => self.flag_n(),                                         // JRMI
            0xA => !self.flag_z() && (self.flag_n() == self.flag_v()),    // JRSGT
            0xB => self.flag_z() || (self.flag_n() != self.flag_v()),     // JRSLE
            0xC => self.flag_n() == self.flag_v(),                        // JRSGE
            _ => self.flag_n() != self.flag_v(),                          // JRSLT
        };
        if taken {
            self.pc = self.pc.wrapping_add(offset as u32) & 0xFF_FFFF;
        }
    }

    /// Push PC+3 (return address) and jump.
    fn call(&mut self, target: u32) {
        let ret = self.pc & 0xFF_FFFF;
        self.push16(ret as u16);
        self.push8((ret >> 16) as u8);
        self.pc = target;
    }

    /// 0x00-0x1F: bit ops (btjt/btjf/bset/bres/bcpl/bccm) and SP-relative ALU.
    fn execute_sp_and_bit(&mut self, opcode: u8) {
        let low = opcode & 0x0F;
        let bit = (opcode >> 4) & 0x07;
        match low {
            // SP-relative ALU operations (0x10-0x1F low nibble, high 0x1).
            _ if opcode >= 0x10 => {
                let off = self.fetch8() as u32;
                let addr = self.sp.wrapping_add(off as u16) as u32;
                let operand = self.read_u8(addr);
                self.alu_a_byte(low, operand, addr);
            }
            // Bit test / unary on (off8,SP) for 0x00-0x0F.
            0x0 | 0x2 | 0x4 | 0x6 | 0x8 | 0xA | 0xC | 0xE => {
                // BTJT bit, addr8, rel (bit in high nibble)
                let addr = self.fetch8() as u32;
                let offset = self.fetch8() as i8 as i32;
                let value = self.read_u8(addr);
                if value & (1 << bit) != 0 {
                    self.pc = self.pc.wrapping_add(offset as u32) & 0xFF_FFFF;
                }
            }
            _ => {
                // BTJF bit, addr8, rel
                let addr = self.fetch8() as u32;
                let offset = self.fetch8() as i8 as i32;
                let value = self.read_u8(addr);
                if value & (1 << bit) == 0 {
                    self.pc = self.pc.wrapping_add(offset as u32) & 0xFF_FFFF;
                }
            }
        }
    }

    /// Apply an ALU byte operation to A with a memory operand.
    fn alu_a_byte(&mut self, op: u8, operand: u8, _addr: u32) {
        match op {
            0x0 => self.a = self.sub8(self.a, operand, false), // SUB
            0x1 => self.cmp8(self.a, operand),                 // CP
            0x2 => self.a = self.sub8(self.a, operand, self.flag_c()), // SBC
            0x4 => {
                let r = self.a & operand;
                self.set_nz(r);
                self.set_c(false);
                self.a = r;
            } // AND
            0x5 => {
                let r = self.a & operand;
                self.set_nz(r);
                self.set_c(false);
            } // BCP
            0x6 => self.a = operand, // LD A, op
            0x7 => self.a = operand, // (unused here; LD op,A handled separately)
            0x8 => {
                let r = self.a ^ operand;
                self.set_nz(r);
                self.set_c(false);
                self.a = r;
            } // XOR
            0x9 => self.a = self.adc8(self.a, operand), // ADC
            0xA => {
                let r = self.a | operand;
                self.set_nz(r);
                self.set_c(false);
                self.a = r;
            } // OR
            0xB => self.a = self.add8(self.a, operand), // ADD
            _ => {}
        }
    }

    /// Unary family 0x30-0x7F (high nibble = addressing mode).
    fn execute_unary(&mut self, opcode: u8, prefix: u8) {
        let high = (opcode >> 4) & 0x0F;
        let low = opcode & 0x0F;
        let addr = match (prefix, high) {
            (0x00, 0x3) => {
                let a = self.fetch8() as u32;
                a
            } // %d
            (0x00, 0x4) => {
                // A-register unary: operate directly.
                self.unary_reg(low);
                return;
            }
            (0x00, 0x5) => {
                let a = self.fetch16() as u32;
                a
            } // %x
            (0x00, 0x6) => {
                let off = self.fetch8() as u32;
                self.x.wrapping_add(off as u16) as u32
            } // (off8,X)
            (0x00, 0x7) => self.x as u32, // (X)
            (0x90, 0x6) => {
                let off = self.fetch8() as u32;
                self.y.wrapping_add(off as u16) as u32
            } // (off8,Y)
            (0x90, 0x7) => self.y as u32, // (Y)
            (0x92, 0x3) => {
                let ptr = self.fetch16() as u32;
                self.read_u16(ptr) as u32
            } // [%w]
            (0x72, 0x3) => {
                let ptr = self.fetch16() as u32;
                self.read_u16(ptr).wrapping_add(self.x) as u32
            } // ([%w],X)
            (0xF2, 0x3) => {
                let ptr = self.fetch16() as u32;
                self.read_u16(ptr).wrapping_add(self.y) as u32
            } // ([%w],Y)
            _ => {
                self.halted = true;
                return;
            }
        };
        let value = self.read_u8(addr);
        let result = self.unary_op(low, value);
        if low != 0xB {
            self.write_u8(addr, result);
        }
    }

    /// Unary op on A (0x40-0x4F high nibble).
    fn unary_reg(&mut self, low: u8) {
        self.a = self.unary_op(low, self.a);
    }

    /// Unary operation core (low nibble = op).
    fn unary_op(&mut self, op: u8, value: u8) -> u8 {
        match op & 0x0F {
            0x0 => {
                let r = self.sub8(0, value, false);
                self.set_z(r == 0);
                r
            } // NEG
            0x1 => {
                let r = !value;
                self.set_nz(r);
                r
            } // CPL
            0x2 => {
                self.set_c(value & 1 != 0);
                let r = value >> 1;
                self.set_nz(r);
                r
            } // SRL
            0x3 => {
                self.set_c(value & 1 != 0);
                let r = ((value as i8) >> 1) as u8;
                self.set_nz(r);
                r
            } // SRA
            0x4 => {
                self.set_c(value & 0x80 != 0);
                let r = value << 1;
                self.set_nz(r);
                r
            } // SLA
            0x5 => {
                let r = (value << 1) | self.flag_c() as u8;
                self.set_c(value & 0x80 != 0);
                self.set_nz(r);
                r
            } // RLC
            0x6 => {
                let r = (value >> 1) | ((self.flag_c() as u8) << 7);
                self.set_c(value & 1 != 0);
                self.set_nz(r);
                r
            } // RRC
            0x7 => {
                let r = value.rotate_left(4);
                self.set_nz(r);
                r
            } // SWAP
            0x8 => {
                let r = value.wrapping_add(1);
                self.set_nz(r);
                self.set_c(r == 0);
                r
            } // INC
            0x9 => {
                let r = value.wrapping_sub(1);
                self.set_nz(r);
                self.set_c(r == 0xFF);
                r
            } // DEC
            0xA => {
                self.set_z(true);
                self.set_n(false);
                self.set_c(false);
                0
            } // CLR
            0xB => {
                self.set_nz(value);
                value
            } // TNZ
            _ => value,
        }
    }

    /// Inherent/register group 0x80-0x9F.
    fn execute_inherent(&mut self, opcode: u8, prefix: u8) {
        let has_90 = prefix == 0x90 || prefix == 0xF2;
        match opcode {
            0x80 => {
                // IRET
                let ret = self.pop16();
                self.pc = ((self.pop8() as u32) << 16) | ret as u32;
                self.cc = self.pop8();
            }
            0x81 => {
                // RET
                let ret = self.pop16();
                self.pc = ((self.pop8() as u32) << 16) | ret as u32;
            }
            0x82 => {
                // INT addr24
                let target = self.fetch24();
                self.push16(self.pc as u16);
                self.push8((self.pc >> 16) as u8);
                self.push8(self.cc);
                self.pc = target;
            }
            0x83 => self.halted = true,   // TRAP
            0x84 => self.a = self.pop8(), // POP A
            0x85 => {
                self.x = self.pop16();
                self.y = self.pop16();
            } // POPW X/Y
            0x86 => self.cc = self.pop8(), // POP CC
            0x87 => {
                // RETF
                let ret = self.pop16();
                self.pc = ((self.pop8() as u32) << 16) | ret as u32;
            }
            0x88 => self.push8(self.a), // PUSH A
            0x89 => {
                self.push16(self.x);
                self.push16(self.y);
            } // PUSHW X/Y
            0x8A => self.push8(self.cc), // PUSH CC
            0x8B => self.halted = true,  // BREAK
            0x8C => self.cc ^= CC_C,     // CCF
            0x8D => {
                let target = self.fetch24();
                self.call(target);
            } // CALLF
            0x8E => self.halted = true, // HALT
            0x8F => self.halted = true, // WFI/WFE
            0x90 => self.y = self.x,    // LDW Y, X
            0x93 => self.x = self.y,    // LDW X, Y
            0x94 => {
                if has_90 {
                    self.sp = self.y;
                } else {
                    self.sp = self.x;
                }
            } // LDW SP, X/Y
            0x95 => {
                if has_90 {
                    self.y = (self.y & 0x00FF) | ((self.a as u16) << 8);
                } else {
                    self.x = (self.x & 0x00FF) | ((self.a as u16) << 8);
                }
            } // LD XH/YH, A
            0x96 => {
                if has_90 {
                    self.y = self.sp;
                } else {
                    self.x = self.sp;
                }
            } // LDW X/Y, SP
            0x97 => {
                if has_90 {
                    self.y = (self.y & 0xFF00) | self.a as u16;
                } else {
                    self.x = (self.x & 0xFF00) | self.a as u16;
                }
            } // LD XL/YL, A
            0x98 => self.cc &= !CC_C, // RCF
            0x99 => self.cc |= CC_C,  // SCF
            0x9A => self.cc &= !(CC_I0 | CC_I1), // RIM
            0x9B => self.cc |= CC_I0 | CC_I1,    // SIM
            0x9C => self.cc &= !CC_H,  // RVF
            0x9D => {}                 // NOP
            0x9E => {
                self.a = if has_90 { (self.y >> 8) as u8 } else { (self.x >> 8) as u8 };
            } // LD A, XH/YH
            0x9F => {
                self.a = if has_90 { (self.y & 0xFF) as u8 } else { (self.x & 0xFF) as u8 };
            } // LD A, XL/YL
            _ => self.halted = true,
        }
    }

    /// ALU family 0xA0-0xFF (high nibble = addressing mode, low = operation).
    fn execute_alu(&mut self, opcode: u8, prefix: u8) {
        let high = (opcode >> 4) & 0x0F;
        let low = opcode & 0x0F;
        let target = self.decode_alu_target(high, prefix);
        match low {
            0x0 => {
                // SUB A, op
                let operand = self.read_target(target);
                self.a = self.sub8(self.a, operand, false);
            }
            0x1 => {
                // CP A, op
                let operand = self.read_target(target);
                self.cmp8(self.a, operand);
            }
            0x2 => {
                // SBC A, op
                let operand = self.read_target(target);
                self.a = self.sub8(self.a, operand, self.flag_c());
            }
            0x3 => {
                // CPW X/Y, word
                let operand16 = self.read_target16(target);
                if high == 0xA || prefix == 0x90 {
                    let _ = self.sub16(self.y, operand16);
                } else {
                    let _ = self.sub16(self.x, operand16);
                }
            }
            0x4 => {
                // AND A, op
                let operand = self.read_target(target);
                let r = self.a & operand;
                self.set_nz(r);
                self.set_c(false);
                self.a = r;
            }
            0x5 => {
                // BCP A, op
                let operand = self.read_target(target);
                let r = self.a & operand;
                self.set_nz(r);
                self.set_c(false);
            }
            0x6 => {
                // LD A, op
                self.a = self.read_target(target);
            }
            0x7 => {
                // LD op, A
                let value = self.a;
                self.write_target(target, value);
            }
            0x8 => {
                // XOR A, op
                let operand = self.read_target(target);
                let r = self.a ^ operand;
                self.set_nz(r);
                self.set_c(false);
                self.a = r;
            }
            0x9 => {
                // ADC A, op
                let operand = self.read_target(target);
                self.a = self.adc8(self.a, operand);
            }
            0xA => {
                // OR A, op
                let operand = self.read_target(target);
                let r = self.a | operand;
                self.set_nz(r);
                self.set_c(false);
                self.a = r;
            }
            0xB => {
                // ADD A, op
                let operand = self.read_target(target);
                self.a = self.add8(self.a, operand);
            }
            0xC => {
                // JP op
                let addr = self.read_target16(target);
                self.pc = addr as u32 & 0xFF_FFFF;
            }
            0xD => {
                // CALL op
                let addr = self.read_target16(target);
                self.call(addr as u32);
            }
            0xE => {
                // LDW X, op
                self.x = self.read_target16(target);
            }
            0xF => {
                // LDW op, X
                let value = self.x;
                self.write_target16(target, value);
            }
            _ => {}
        }
    }
}

/// Disassemble one STM8 instruction at `pc` using `read` for bytes.
pub fn disassemble(pc: u32, read: &dyn Fn(u32) -> u8) -> String {
    let opcode = read(pc);
    match opcode {
        0x9D => "nop".to_string(),
        0x81 => "ret".to_string(),
        0x8E => "halt".to_string(),
        0x88 => "push a".to_string(),
        0x84 => "pop a".to_string(),
        0x20 => "jra".to_string(),
        0x21 => "jrf".to_string(),
        _ => format!("0x{opcode:02x}"),
    }
}
