//! A Renesas V850 (V850E family, IAR V850) instruction-level simulator core.
//!
//! Encodings are derived from the Ghidra V850 SLEIGH specification
//! (`V850.sinc` plus `V850_instructions.sinc`) and cross-checked against the
//! GNU binutils `v850-opc.c` opcode table. V850 instructions are little-endian
//! and 16-bit aligned; most are 16 or 32 bits wide, a few are 48 or 64 bits.
//!
//! # Implemented subset
//!
//! * Moves: `mov` reg-reg / imm5 / imm32, `movea`, `movhi`.
//! * Arithmetic: `add`, `add imm5`, `addi`, `sub`, `subr`, `cmp`, `cmp imm5`,
//!   `mul` (64-bit result), `mulu`, `mulh`, `mulh imm5`, `mulhi`, `div`,
//!   `divu`.
//! * Logic: `and`, `andi`, `or`, `ori`, `xor`, `xori`, `not`, `tst`, `setf`.
//! * Shifts: `shl`/`shr`/`sar` with imm5, register, and three-operand forms.
//! * Memory: `ld.b`, `ld.bu`, `ld.h`, `ld.hu`, `ld.w`, `st.b`, `st.h`, `st.w`
//!   with a 16-bit displacement. V850 has no separate register-indirect
//!   load/store encoding; a zero displacement is the register-indirect form.
//! * Control: `jr` (22/32-bit displacement), `jarl` (22/32-bit displacement,
//!   link register), `jmp [reg]`, `jmp disp32[reg]`, conditional branches
//!   (`bcond`, 9-bit displacement), `ret` (the canonical `jmp [lp]` encoding
//!   `0x007f`), `prepare`/`dispose` (all documented forms), `halt`, `nop`,
//!   `ei`/`di`.
//!
//! # Documented simplifications
//!
//! * `r0` is hard-wired to zero; writes to it are ignored.
//! * `psw` models Z (bit 0), S (bit 1), OV (bit 2) and CY (bit 3) exactly as
//!   the SLEIGH flag macros do. SAT (bit 4) is only read by `setf sa`; no
//!   saturating instruction is implemented, so it is never set by this core.
//! * Memory is a flat 1 MiB little-endian buffer addressed identically to the
//!   program counter. Reads outside it return zero and writes are dropped
//!   (mirroring the other simulator cores in this crate).
//! * `halt` stops the core (`halted = true`). The SLEIGH spec instead loops on
//!   the instruction forever; stopping is the useful behaviour for an IDE.
//! * An unknown opcode stops the core without advancing `pc`, so the caller
//!   can inspect the offending address.
//! * `ei`/`di` are accepted and ignored: no interrupt controller or system
//!   register file is modelled.
//! * Division by zero and signed-overflow division are deterministic: the
//!   quotient is 0 / `0x8000_0000` respectively, the remainder is the dividend
//!   / 0, and OV is set. Real hardware leaves the results undefined.
//! * There is no dedicated `mov imm16`, `neg`, `clr`, `inc`, `dec`, `push` or
//!   `pop` instruction in the V850 encoding. The assembler idioms are used
//!   instead: `mov imm32`/`movea`/`movhi`, `subr r0, rn`, `mov r0, rn`,
//!   `addi 1, rn, rn`, `addi -1, rn, rn`, and `prepare`/`dispose`.
//! * Not implemented (omitted rather than guessed): FPU, saturation,
//!   bit-manipulation (`set1`/`clr1`/`not1`/`tst1`), byte-swap, sign
//!   extensions, `callt`/`switch`/`trap`/`syscall`, system-register access,
//!   interrupt return, the 23-bit displacement load/store forms, the V850E3
//!   7-bit conditional branch form, and V850E3V5 encodings. There is also no
//!   32-bit conditional `jr`: conditional branches only exist in the 9-bit
//!   `bcond` form above.

/// Flat address-space size (1 MiB).
const MEMORY_SIZE: usize = 1024 * 1024;

/// PSW zero flag.
const FLAG_Z: u32 = 1 << 0;
/// PSW sign flag.
const FLAG_S: u32 = 1 << 1;
/// PSW overflow flag.
const FLAG_OV: u32 = 1 << 2;
/// PSW carry flag.
const FLAG_CY: u32 = 1 << 3;
/// PSW saturation flag (only observed by `setf sa`).
const FLAG_SAT: u32 = 1 << 4;

/// List bit for each register r20..r31 in a `prepare`/`dispose` register list.
/// Derived from the SLEIGH `prep`/`DispList` tables (identical to the GNU
/// binutils `type1_regs` table).
const REGISTER_LIST_BIT: [u8; 12] = [
    7,  // r20
    6,  // r21
    5,  // r22
    4,  // r23
    11, // r24
    10, // r25
    9,  // r26
    8,  // r27
    3,  // r28
    2,  // r29
    0,  // r30 / ep
    1,  // r31 / lp
];

/// Outcome of a single V850 step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V850Step {
    Advanced,
    Breakpoint,
    Halted,
}

/// A V850E core with 32 general-purpose registers and a flat memory image.
pub struct V850Core {
    /// r0..r31; r3 = sp, r4 = gp, r5 = tp, r30 = ep, r31 = lp.
    pub r: [u32; 32],
    /// Program counter.
    pub pc: u32,
    /// Program status word: Z=0, S=1, OV=2, CY=3, SAT=4.
    pub psw: u32,
    /// Flat little-endian memory image.
    pub memory: Vec<u8>,
    /// Whether execution should stop.
    pub halted: bool,
    /// Instructions executed since reset.
    pub cycle_count: u64,
}

impl V850Core {
    pub fn new() -> Self {
        Self {
            r: [0; 32],
            pc: 0,
            psw: 0,
            memory: vec![0; MEMORY_SIZE],
            halted: false,
            cycle_count: 0,
        }
    }

    /// Reset the register file, PSW and cycle counter, then jump to `entry`.
    /// Memory contents are preserved.
    pub fn reset(&mut self, entry: u32) {
        self.r = [0; 32];
        self.pc = entry;
        self.psw = 0;
        self.halted = false;
        self.cycle_count = 0;
    }

    /// Write an image at `base` and start execution there.
    pub fn load_image(&mut self, base: u32, image: &[u8]) {
        self.write_bytes(base, image);
        self.pc = base;
        self.halted = false;
    }

    /// Set the program counter directly.
    pub fn set_pc(&mut self, pc: u32) {
        self.pc = pc;
    }

    // -------- memory --------

    pub fn read_u8(&self, addr: u32) -> u8 {
        self.memory.get(addr as usize).copied().unwrap_or(0)
    }

    pub fn read_u16(&self, addr: u32) -> u16 {
        u16::from_le_bytes([
            self.read_u8(addr),
            self.read_u8(addr.wrapping_add(1)),
        ])
    }

    pub fn read_u32(&self, addr: u32) -> u32 {
        u32::from_le_bytes([
            self.read_u8(addr),
            self.read_u8(addr.wrapping_add(1)),
            self.read_u8(addr.wrapping_add(2)),
            self.read_u8(addr.wrapping_add(3)),
        ])
    }

    pub fn write_u8(&mut self, addr: u32, value: u8) {
        if let Some(byte) = self.memory.get_mut(addr as usize) {
            *byte = value;
        }
    }

    pub fn write_u16(&mut self, addr: u32, value: u16) {
        let bytes = value.to_le_bytes();
        self.write_u8(addr, bytes[0]);
        self.write_u8(addr.wrapping_add(1), bytes[1]);
    }

    pub fn write_u32(&mut self, addr: u32, value: u32) {
        let bytes = value.to_le_bytes();
        for (i, byte) in bytes.iter().enumerate() {
            self.write_u8(addr.wrapping_add(i as u32), *byte);
        }
    }

    /// Write a block of bytes (for example an ELF segment).
    pub fn write_bytes(&mut self, base: u32, bytes: &[u8]) {
        for (i, byte) in bytes.iter().enumerate() {
            self.write_u8(base.wrapping_add(i as u32), *byte);
        }
    }

    /// Zero-fill `count` bytes starting at `base`.
    pub fn zero_bytes(&mut self, base: u32, count: u32) {
        for i in 0..count {
            self.write_u8(base.wrapping_add(i), 0);
        }
    }

    // -------- registers --------

    /// Read an architectural register. r0 always reads as zero.
    pub fn read_reg(&self, index: usize) -> u32 {
        if index & 31 == 0 {
            0
        } else {
            self.r[index & 31]
        }
    }

    /// Write an architectural register. Writes to r0 are ignored.
    pub fn write_reg(&mut self, index: usize, value: u32) {
        let index = index & 31;
        if index != 0 {
            self.r[index] = value;
        }
    }

    /// Number of general-purpose registers.
    pub fn register_count() -> usize {
        32
    }

    /// ABI-aware register name.
    pub fn register_label(index: usize) -> String {
        match index & 31 {
            3 => "sp".to_string(),
            4 => "gp".to_string(),
            5 => "tp".to_string(),
            30 => "ep".to_string(),
            31 => "lp".to_string(),
            other => format!("r{other}"),
        }
    }

    // -------- flags --------

    fn flag(&self, mask: u32) -> bool {
        self.psw & mask != 0
    }

    fn set_flag(&mut self, mask: u32, value: bool) {
        if value {
            self.psw |= mask;
        } else {
            self.psw &= !mask;
        }
    }

    /// Set S and Z from a result; leave OV and CY untouched.
    fn set_sz(&mut self, result: u32) {
        self.set_flag(FLAG_S, result & 0x8000_0000 != 0);
        self.set_flag(FLAG_Z, result == 0);
    }

    /// OV = 0, then S/Z from the result.
    fn set_ov0_sz(&mut self, result: u32) {
        self.set_flag(FLAG_OV, false);
        self.set_sz(result);
    }

    /// Add and update CY/OV/S/Z (SLEIGH `set_general_flags_pos`).
    fn add_flags(&mut self, a: u32, b: u32) -> u32 {
        let result = a.wrapping_add(b);
        self.set_flag(FLAG_CY, result < a);
        self.set_flag(FLAG_OV, (a ^ result) & (b ^ result) & 0x8000_0000 != 0);
        self.set_sz(result);
        result
    }

    /// Subtract and update CY/OV/S/Z (SLEIGH `set_general_flags_neg`).
    fn sub_flags(&mut self, a: u32, b: u32) -> u32 {
        let result = a.wrapping_sub(b);
        self.set_flag(FLAG_CY, a < b);
        self.set_flag(FLAG_OV, (a ^ b) & (a ^ result) & 0x8000_0000 != 0);
        self.set_sz(result);
        result
    }

    /// Evaluate a 4-bit V850 condition code.
    fn condition(&self, cond: u8) -> bool {
        let z = self.flag(FLAG_Z);
        let s = self.flag(FLAG_S);
        let ov = self.flag(FLAG_OV);
        let cy = self.flag(FLAG_CY);
        match cond & 0xF {
            0x0 => ov,                        // v
            0x1 => cy,                        // c / l
            0x2 => z,                         // e / z
            0x3 => cy || z,                   // nh
            0x4 => s,                         // n
            0x5 => true,                      // t / br
            0x6 => s ^ ov,                    // lt
            0x7 => (s ^ ov) || z,             // le
            0x8 => !ov,                       // nv
            0x9 => !cy,                       // nc / nl
            0xA => !z,                        // ne / nz
            0xB => !(cy || z),                // h
            0xC => !s,                        // p
            0xD => self.flag(FLAG_SAT),       // sa
            0xE => !(s ^ ov),                 // ge
            _ => !((s ^ ov) || z),            // gt
        }
    }

    // -------- execution --------

    /// Decode the instruction at `pc` using the core's own memory.
    fn decode(&self) -> (Insn, u32) {
        let read = |addr: u32| self.read_u16(addr);
        decode_at(self.pc, &read)
    }

    /// Execute one instruction. Breakpoints are checked before any state change.
    pub fn step(&mut self, breakpoints: &[u32]) -> V850Step {
        if self.halted {
            return V850Step::Halted;
        }
        if breakpoints.contains(&self.pc) {
            return V850Step::Breakpoint;
        }
        let start = self.pc;
        let (insn, len) = self.decode();
        if matches!(insn, Insn::Unknown(_)) {
            self.halted = true;
            return V850Step::Halted;
        }
        self.pc = start.wrapping_add(len);
        self.cycle_count += 1;
        self.execute(insn, start, len);
        if self.halted {
            V850Step::Halted
        } else {
            V850Step::Advanced
        }
    }
}

impl Default for V850Core {
    fn default() -> Self {
        Self::new()
    }
}

/// Access width for load/store instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemSize {
    Byte,
    Half,
    Word,
}

/// Third operand of a `prepare` instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrepareMode {
    Plain,
    StackPointer,
    Imm16Low(i32),
    Imm16High(i32),
    Imm32(u32),
}

/// A decoded V850 instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Insn {
    Nop,
    Halt,
    InterruptEnable,
    InterruptDisable,
    Mov { src: usize, dst: usize },
    MovI5 { imm: i32, dst: usize },
    MovI32 { imm: u32, dst: usize },
    Movea { imm: i32, src: usize, dst: usize },
    Movhi { imm: i32, src: usize, dst: usize },
    Add { a: usize, b: usize },
    AddI5 { imm: i32, b: usize },
    Addi { imm: i32, src: usize, dst: usize },
    Sub { a: usize, b: usize },
    Subr { a: usize, b: usize },
    Cmp { a: usize, b: usize },
    CmpI5 { imm: i32, b: usize },
    And { a: usize, b: usize },
    Andi { imm: u32, src: usize, dst: usize },
    Or { a: usize, b: usize },
    Ori { imm: u32, src: usize, dst: usize },
    Xor { a: usize, b: usize },
    Xori { imm: u32, src: usize, dst: usize },
    Not { src: usize, dst: usize },
    Tst { a: usize, b: usize },
    ShlI5 { count: u32, dst: usize },
    ShrI5 { count: u32, dst: usize },
    SarI5 { count: u32, dst: usize },
    ShlReg { count: usize, dst: usize },
    ShrReg { count: usize, dst: usize },
    SarReg { count: usize, dst: usize },
    Shl3 { count: usize, src: usize, dst: usize },
    Shr3 { count: usize, src: usize, dst: usize },
    Sar3 { count: usize, src: usize, dst: usize },
    Mul { a: usize, b: usize, low: usize, high: usize },
    Mulu { a: usize, b: usize, low: usize, high: usize },
    Mulh { a: usize, b: usize },
    MulhI5 { imm: i32, b: usize },
    Mulhi { imm: i32, src: usize, dst: usize },
    Div { a: usize, b: usize, quot: usize, rem: usize },
    Divu { a: usize, b: usize, quot: usize, rem: usize },
    Setf { cond: u8, dst: usize },
    Ld { disp: i32, base: usize, dst: usize, size: MemSize, unsigned: bool },
    St { disp: i32, base: usize, src: usize, size: MemSize },
    Bcond { cond: u8, target: u32 },
    Jr { target: u32 },
    Jarl { target: u32, reg: usize },
    JmpReg { reg: usize },
    JmpDisp { reg: usize, disp: u32 },
    Ret,
    Prepare { list: u32, imm5: u32, mode: PrepareMode },
    Dispose { list: u32, imm5: u32, ret: Option<usize> },
    Unknown(u16),
}

/// Sign-extend the low `bits` bits of `value`.
fn sext(value: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((value << shift) as i32) >> shift
}

/// Sign-extend the low 16 bits.
fn sext16(value: u32) -> i32 {
    value as u16 as i16 as i32
}

/// Build an `Insn::Unknown` together with its consumed length.
fn unknown(w0: u16) -> (Insn, u32) {
    (Insn::Unknown(w0), 2)
}

/// Decode the instruction at `pc`, reading 16-bit halfwords through `read16`.
fn decode_at(pc: u32, read16: &dyn Fn(u32) -> u16) -> (Insn, u32) {
    let w0 = read16(pc);
    let r1 = (w0 & 0x1F) as usize;
    let r2 = (w0 >> 11) as usize;
    // Single-opcode system instructions (16-bit). These share the SETF opcode
    // space but are one word wide: `halt`, `ei`, `di`.
    match w0 {
        0x7E7F => return (Insn::Halt, 2),
        0x7E80 => return (Insn::InterruptEnable, 2),
        0x7E81 => return (Insn::InterruptDisable, 2),
        _ => {}
    }

    // 0x0000 is the canonical NOP (`mov r0, r0` shares the encoding).
    if w0 == 0 {
        return (Insn::Nop, 2);
    }

    // Format I: opcode in bits 5..10, two register/immediate operands.
    let op = (w0 >> 5) & 0x3F;
    match op {
        0x00 => return (Insn::Mov { src: r1, dst: r2 }, 2),
        0x01 => return (Insn::Not { src: r1, dst: r2 }, 2),
        0x07 => return (Insn::Mulh { a: r1, b: r2 }, 2),
        0x08 => return (Insn::Or { a: r1, b: r2 }, 2),
        0x09 => return (Insn::Xor { a: r1, b: r2 }, 2),
        0x0A => return (Insn::And { a: r1, b: r2 }, 2),
        0x0B => return (Insn::Tst { a: r1, b: r2 }, 2),
        0x0C => return (Insn::Subr { a: r1, b: r2 }, 2),
        0x0D => return (Insn::Sub { a: r1, b: r2 }, 2),
        0x0E => return (Insn::Add { a: r1, b: r2 }, 2),
        0x0F => return (Insn::Cmp { a: r1, b: r2 }, 2),
        0x10 => {
            return (
                Insn::MovI5 {
                    imm: sext(r1 as u32, 5),
                    dst: r2,
                },
                2,
            )
        }
        0x12 => {
            return (
                Insn::AddI5 {
                    imm: sext(r1 as u32, 5),
                    b: r2,
                },
                2,
            )
        }
        0x13 => {
            return (
                Insn::CmpI5 {
                    imm: sext(r1 as u32, 5),
                    b: r2,
                },
                2,
            )
        }
        0x14 => {
            return (
                Insn::ShrI5 {
                    count: r1 as u32,
                    dst: r2,
                },
                2,
            )
        }
        0x15 => {
            return (
                Insn::SarI5 {
                    count: r1 as u32,
                    dst: r2,
                },
                2,
            )
        }
        0x16 => {
            return (
                Insn::ShlI5 {
                    count: r1 as u32,
                    dst: r2,
                },
                2,
            )
        }
        0x17 => {
            return (
                Insn::MulhI5 {
                    imm: sext(r1 as u32, 5),
                    b: r2,
                },
                2,
            )
        }
        _ => {}
    }

    // Format III: 9-bit conditional branch, bits 7..10 = 0xB.
    if (w0 & 0x0780) == 0x0580 {
        let cond = (w0 & 0xF) as u8;
        let high = sext((w0 >> 11) as u32, 5);
        let low = (((w0 >> 4) & 0x7) as i32) << 1;
        let disp = (high << 4) | low;
        return (
            Insn::Bcond {
                cond,
                target: pc.wrapping_add(disp as u32),
            },
            2,
        );
    }

    // JMP [reg1] (0x0060 | reg). The r31 form is the `ret` idiom.
    if (w0 & 0xFFE0) == 0x0060 {
        if r1 == 31 {
            return (Insn::Ret, 2);
        }
        return (Insn::JmpReg { reg: r1 }, 2);
    }

    // JR / JARL with a 32-bit displacement (0x02E0 | reg).
    if (w0 & 0xFFE0) == 0x02E0 {
        let w1 = read16(pc.wrapping_add(2));
        let w2 = read16(pc.wrapping_add(4));
        if w1 & 1 != 0 {
            return unknown(w0);
        }
        let disp = ((w2 as u32) << 16) | (w1 as u32);
        let target = pc.wrapping_add(disp);
        if r1 == 0 {
            return (Insn::Jr { target }, 6);
        }
        return (Insn::Jarl { target, reg: r1 }, 6);
    }

    let top = w0 >> 6;

    // DISPOSE imm5, list12[, [reg1]] -- first word bits 6..15 = 0x19.
    if top == 0x19 {
        let w1 = read16(pc.wrapping_add(2));
        let imm5 = ((w0 >> 1) & 0x1F) as u32;
        let list = (((w1 >> 5) as u32) << 1) | (w0 & 1) as u32;
        let ret = match w1 & 0x1F {
            0 => None,
            reg => Some(reg as usize),
        };
        return (Insn::Dispose { list, imm5, ret }, 4);
    }

    // JR/JARL (22-bit displacement), LD.BU, and PREPARE all use 0x1E in
    // bits 6..10; they are distinguished by bits 11..15 and by word 1.
    if top == 0x1E {
        let w1 = read16(pc.wrapping_add(2));
        if w1 & 1 == 0 {
            let disp =
                (sext((w0 & 0x3F) as u32, 6) << 16) | ((w1 & 0xFFFE) as i32);
            let target = pc.wrapping_add(disp as u32);
            if r2 == 0 {
                return (Insn::Jr { target }, 4);
            }
            return (Insn::Jarl { target, reg: r2 }, 4);
        }
        if r2 != 0 {
            // LD.BU disp16[reg1], reg2.
            let disp = (sext((w1 >> 1) as u32, 15) << 1)
                | ((w0 >> 5) & 1) as i32;
            return (
                Insn::Ld {
                    disp,
                    base: r1,
                    dst: r2,
                    size: MemSize::Byte,
                    unsigned: true,
                },
                4,
            );
        }
        // PREPARE list12, imm5[, third] -- low 5 bits of word 1 select the form.
        let imm5 = ((w0 >> 1) & 0x1F) as u32;
        let list = (((w1 >> 5) as u32) << 1) | (w0 & 1) as u32;
        let mode = match w1 & 0x1F {
            0x1 => PrepareMode::Plain,
            0x3 => PrepareMode::StackPointer,
            0xB => PrepareMode::Imm16Low(sext16(read16(pc.wrapping_add(4)) as u32)),
            0x13 => PrepareMode::Imm16High(sext16(read16(pc.wrapping_add(4)) as u32)),
            0x1B => {
                let low = read16(pc.wrapping_add(4)) as u32;
                let high = read16(pc.wrapping_add(6)) as u32;
                PrepareMode::Imm32((high << 16) | low)
            }
            _ => return unknown(w0),
        };
        let len = match mode {
            PrepareMode::Plain | PrepareMode::StackPointer => 4,
            PrepareMode::Imm16Low(_) | PrepareMode::Imm16High(_) => 6,
            PrepareMode::Imm32(_) => 8,
        };
        return (Insn::Prepare { list, imm5, mode }, len);
    }

    match op {
        // ADDI imm16, reg1, reg2
        0x30 => {
            let imm = sext16(read16(pc.wrapping_add(2)) as u32);
            (
                Insn::Addi {
                    imm,
                    src: r1,
                    dst: r2,
                },
                4,
            )
        }
        // MOVEA imm16, reg1, reg2 / MOV imm32, reg1 (reg2 field == 0)
        0x31 => {
            if (w0 & 0xFFE0) == 0x0620 {
                let low = read16(pc.wrapping_add(2)) as u32;
                let high = read16(pc.wrapping_add(4)) as u32;
                (Insn::MovI32 { imm: (high << 16) | low, dst: r1 }, 6)
            } else {
                let imm = sext16(read16(pc.wrapping_add(2)) as u32);
                (
                    Insn::Movea {
                        imm,
                        src: r1,
                        dst: r2,
                    },
                    4,
                )
            }
        }
        // MOVHI imm16, reg1, reg2
        0x32 => {
            let imm = sext16(read16(pc.wrapping_add(2)) as u32);
            (
                Insn::Movhi {
                    imm,
                    src: r1,
                    dst: r2,
                },
                4,
            )
        }
        // ORI / XORI / ANDI imm16, reg1, reg2
        0x34 => {
            let imm = read16(pc.wrapping_add(2)) as u32;
            (
                Insn::Ori {
                    imm,
                    src: r1,
                    dst: r2,
                },
                4,
            )
        }
        0x35 => {
            let imm = read16(pc.wrapping_add(2)) as u32;
            (
                Insn::Xori {
                    imm,
                    src: r1,
                    dst: r2,
                },
                4,
            )
        }
        0x36 => {
            let imm = read16(pc.wrapping_add(2)) as u32;
            (
                Insn::Andi {
                    imm,
                    src: r1,
                    dst: r2,
                },
                4,
            )
        }
        // MULHI imm16, reg1, reg2 / JMP disp32[reg1] (reg2 field == 0)
        0x37 => {
            if (w0 & 0xFFE0) == 0x06E0 {
                let low = read16(pc.wrapping_add(2)) as u32;
                let high = read16(pc.wrapping_add(4)) as u32;
                (
                    Insn::JmpDisp {
                        reg: r1,
                        disp: (high << 16) | low,
                    },
                    6,
                )
            } else {
                let imm = sext16(read16(pc.wrapping_add(2)) as u32);
                (
                    Insn::Mulhi {
                        imm,
                        src: r1,
                        dst: r2,
                    },
                    4,
                )
            }
        }
        // LD.B disp16[reg1], reg2
        0x38 => {
            let disp = sext16(read16(pc.wrapping_add(2)) as u32);
            (
                Insn::Ld {
                    disp,
                    base: r1,
                    dst: r2,
                    size: MemSize::Byte,
                    unsigned: false,
                },
                4,
            )
        }
        // LD.H / LD.W disp16[reg1], reg2 -- word 1 bit 0 selects the width.
        0x39 => {
            let w1 = read16(pc.wrapping_add(2));
            if w1 & 1 == 0 {
                let disp = sext16(w1 as u32);
                (
                    Insn::Ld {
                        disp,
                        base: r1,
                        dst: r2,
                        size: MemSize::Half,
                        unsigned: false,
                    },
                    4,
                )
            } else {
                let disp = sext16(w1 as u32) & !1;
                (
                    Insn::Ld {
                        disp,
                        base: r1,
                        dst: r2,
                        size: MemSize::Word,
                        unsigned: false,
                    },
                    4,
                )
            }
        }
        // ST.B disp16[reg1], reg2
        0x3A => {
            let disp = sext16(read16(pc.wrapping_add(2)) as u32);
            (
                Insn::St {
                    disp,
                    base: r1,
                    src: r2,
                    size: MemSize::Byte,
                },
                4,
            )
        }
        // ST.H / ST.W disp16[reg1], reg2
        0x3B => {
            let w1 = read16(pc.wrapping_add(2));
            if w1 & 1 == 0 {
                let disp = sext16(w1 as u32);
                (
                    Insn::St {
                        disp,
                        base: r1,
                        src: r2,
                        size: MemSize::Half,
                    },
                    4,
                )
            } else {
                let disp = sext16(w1 as u32) & !1;
                (
                    Insn::St {
                        disp,
                        base: r1,
                        src: r2,
                        size: MemSize::Word,
                    },
                    4,
                )
            }
        }
        // 0x3F covers SETF, the register shifts, the three-operand ALU
        // group, and LD.HU.
        0x3F => decode_group_3f(pc, w0, r1, r2, read16),
        _ => unknown(w0),
    }
}

/// Decode the 32-bit encodings whose first-word opcode field is 0x3F.
fn decode_group_3f(
    pc: u32,
    w0: u16,
    r1: usize,
    r2: usize,
    read16: &dyn Fn(u32) -> u16,
) -> (Insn, u32) {
    let w1 = read16(pc.wrapping_add(2));
    let r3 = (w1 >> 11) as usize;

    // SETF cond, reg2 -- bits 4..10 = 0x7E and an all-zero second word.
    if w1 == 0 && (w0 & 0x07F0) == 0x07E0 {
        return (
            Insn::Setf {
                cond: (w0 & 0xF) as u8,
                dst: r2,
            },
            4,
        );
    }

    // Two-operand register shifts.
    match w1 {
        0x0080 => return (Insn::ShrReg { count: r1, dst: r2 }, 4),
        0x00A0 => return (Insn::SarReg { count: r1, dst: r2 }, 4),
        0x00C0 => return (Insn::ShlReg { count: r1, dst: r2 }, 4),
        _ => {}
    }

    // Three-operand ALU group: bits 0..10 select the operation.
    match w1 & 0x07FF {
        0x0220 => {
            return (
                Insn::Mul {
                    a: r1,
                    b: r2,
                    low: r2,
                    high: r3,
                },
                4,
            )
        }
        0x0222 => {
            return (
                Insn::Mulu {
                    a: r1,
                    b: r2,
                    low: r2,
                    high: r3,
                },
                4,
            )
        }
        0x02C0 => {
            return (
                Insn::Div {
                    a: r1,
                    b: r2,
                    quot: r2,
                    rem: r3,
                },
                4,
            )
        }
        0x02C2 => {
            return (
                Insn::Divu {
                    a: r1,
                    b: r2,
                    quot: r2,
                    rem: r3,
                },
                4,
            )
        }
        0x0082 => {
            return (
                Insn::Shr3 {
                    count: r1,
                    src: r2,
                    dst: r3,
                },
                4,
            )
        }
        0x00A2 => {
            return (
                Insn::Sar3 {
                    count: r1,
                    src: r2,
                    dst: r3,
                },
                4,
            )
        }
        0x00C2 => {
            return (
                Insn::Shl3 {
                    count: r1,
                    src: r2,
                    dst: r3,
                },
                4,
            )
        }
        _ => {}
    }

    // LD.HU disp16[reg1], reg2 -- an odd second word that is not an opcode.
    if w1 & 1 != 0 {
        let disp = sext((w1 >> 1) as u32, 15) << 1;
        return (
            Insn::Ld {
                disp,
                base: r1,
                dst: r2,
                size: MemSize::Half,
                unsigned: true,
            },
            4,
        );
    }

    unknown(w0)
}

impl V850Core {
    /// Logical left shift with the SLEIGH `shift_left_logic` flags.
    fn shift_left(&mut self, value: u32, count: u32) -> u32 {
        let shift = count & 0x1F;
        let result = value << shift;
        let carry = shift != 0 && ((value >> (32 - shift)) & 1) != 0;
        self.set_flag(FLAG_OV, false);
        self.set_sz(result);
        self.set_flag(FLAG_CY, carry);
        result
    }

    /// Logical right shift with the SLEIGH `shift_right_logic` flags.
    fn shift_right(&mut self, value: u32, count: u32) -> u32 {
        let shift = count & 0x1F;
        let result = value >> shift;
        let carry = shift != 0 && ((value >> (shift - 1)) & 1) != 0;
        self.set_flag(FLAG_OV, false);
        self.set_sz(result);
        self.set_flag(FLAG_CY, carry);
        result
    }

    /// Arithmetic right shift with the SLEIGH `shift_right_arith` flags.
    fn shift_right_arith(&mut self, value: u32, count: u32) -> u32 {
        let shift = count & 0x1F;
        let result = ((value as i32) >> shift) as u32;
        let carry = shift != 0 && ((value >> (shift - 1)) & 1) != 0;
        self.set_flag(FLAG_OV, false);
        self.set_sz(result);
        self.set_flag(FLAG_CY, carry);
        result
    }

    fn execute(&mut self, insn: Insn, start: u32, len: u32) {
        match insn {
            Insn::Nop | Insn::InterruptEnable | Insn::InterruptDisable => {}
            Insn::Halt => self.halted = true,
            Insn::Mov { src, dst } => {
                let value = self.read_reg(src);
                self.write_reg(dst, value);
            }
            Insn::MovI5 { imm, dst } => self.write_reg(dst, imm as u32),
            Insn::MovI32 { imm, dst } => self.write_reg(dst, imm),
            Insn::Movea { imm, src, dst } => {
                let value = self.read_reg(src).wrapping_add(imm as u32);
                self.write_reg(dst, value);
            }
            Insn::Movhi { imm, src, dst } => {
                let value = self
                    .read_reg(src)
                    .wrapping_add((imm as u32) << 16);
                self.write_reg(dst, value);
            }
            Insn::Add { a, b } => {
                let lhs = self.read_reg(b);
                let rhs = self.read_reg(a);
                let result = self.add_flags(lhs, rhs);
                self.write_reg(b, result);
            }
            Insn::AddI5 { imm, b } => {
                let result = self.add_flags(self.read_reg(b), imm as u32);
                self.write_reg(b, result);
            }
            Insn::Addi { imm, src, dst } => {
                let result = self.add_flags(self.read_reg(src), imm as u32);
                self.write_reg(dst, result);
            }
            Insn::Sub { a, b } => {
                let lhs = self.read_reg(b);
                let rhs = self.read_reg(a);
                let result = self.sub_flags(lhs, rhs);
                self.write_reg(b, result);
            }
            Insn::Subr { a, b } => {
                let lhs = self.read_reg(a);
                let rhs = self.read_reg(b);
                let result = self.sub_flags(lhs, rhs);
                self.write_reg(b, result);
            }
            Insn::Cmp { a, b } => {
                let lhs = self.read_reg(b);
                let rhs = self.read_reg(a);
                self.sub_flags(lhs, rhs);
            }
            Insn::CmpI5 { imm, b } => {
                let lhs = self.read_reg(b);
                self.sub_flags(lhs, imm as u32);
            }
            Insn::And { a, b } => {
                let result = self.read_reg(a) & self.read_reg(b);
                self.write_reg(b, result);
                self.set_ov0_sz(result);
            }
            Insn::Andi { imm, src, dst } => {
                let result = self.read_reg(src) & imm;
                self.write_reg(dst, result);
                self.set_ov0_sz(result);
            }
            Insn::Or { a, b } => {
                let result = self.read_reg(a) | self.read_reg(b);
                self.write_reg(b, result);
                self.set_ov0_sz(result);
            }
            Insn::Ori { imm, src, dst } => {
                let result = self.read_reg(src) | imm;
                self.write_reg(dst, result);
                self.set_ov0_sz(result);
            }
            Insn::Xor { a, b } => {
                let result = self.read_reg(a) ^ self.read_reg(b);
                self.write_reg(b, result);
                self.set_ov0_sz(result);
            }
            Insn::Xori { imm, src, dst } => {
                let result = self.read_reg(src) ^ imm;
                self.write_reg(dst, result);
                self.set_ov0_sz(result);
            }
            Insn::Not { src, dst } => {
                let result = !self.read_reg(src);
                self.write_reg(dst, result);
                self.set_ov0_sz(result);
            }
            Insn::Tst { a, b } => {
                let result = self.read_reg(a) & self.read_reg(b);
                self.set_ov0_sz(result);
            }
            Insn::ShlI5 { count, dst } => {
                let result = self.shift_left(self.read_reg(dst), count);
                self.write_reg(dst, result);
            }
            Insn::ShrI5 { count, dst } => {
                let result = self.shift_right(self.read_reg(dst), count);
                self.write_reg(dst, result);
            }
            Insn::SarI5 { count, dst } => {
                let result = self.shift_right_arith(self.read_reg(dst), count);
                self.write_reg(dst, result);
            }
            Insn::ShlReg { count, dst } => {
                let shift = self.read_reg(count);
                let result = self.shift_left(self.read_reg(dst), shift);
                self.write_reg(dst, result);
            }
            Insn::ShrReg { count, dst } => {
                let shift = self.read_reg(count);
                let result = self.shift_right(self.read_reg(dst), shift);
                self.write_reg(dst, result);
            }
            Insn::SarReg { count, dst } => {
                let shift = self.read_reg(count);
                let result = self.shift_right_arith(self.read_reg(dst), shift);
                self.write_reg(dst, result);
            }
            Insn::Shl3 { count, src, dst } => {
                let shift = self.read_reg(count);
                let result = self.shift_left(self.read_reg(src), shift);
                self.write_reg(dst, result);
            }
            Insn::Shr3 { count, src, dst } => {
                let shift = self.read_reg(count);
                let result = self.shift_right(self.read_reg(src), shift);
                self.write_reg(dst, result);
            }
            Insn::Sar3 { count, src, dst } => {
                let shift = self.read_reg(count);
                let result = self.shift_right_arith(self.read_reg(src), shift);
                self.write_reg(dst, result);
            }
            Insn::Mul { a, b, low, high } => {
                let product = (self.read_reg(a) as i32 as i64)
                    .wrapping_mul(self.read_reg(b) as i32 as i64);
                self.write_reg(low, product as u32);
                self.write_reg(high, (product >> 32) as u32);
            }
            Insn::Mulu { a, b, low, high } => {
                let product =
                    (self.read_reg(a) as u64) * (self.read_reg(b) as u64);
                self.write_reg(low, product as u32);
                self.write_reg(high, (product >> 32) as u32);
            }
            Insn::Mulh { a, b } => {
                let product = sext16(self.read_reg(a))
                    .wrapping_mul(sext16(self.read_reg(b)));
                self.write_reg(b, product as u32);
            }
            Insn::MulhI5 { imm, b } => {
                let product = sext16(self.read_reg(b)).wrapping_mul(imm);
                self.write_reg(b, product as u32);
            }
            Insn::Mulhi { imm, src, dst } => {
                let result = self.read_reg(src).wrapping_mul(imm as u32);
                self.write_reg(dst, result);
            }
            Insn::Div { a, b, quot, rem } => {
                let divisor = self.read_reg(a);
                let dividend = self.read_reg(b);
                let overflow = divisor == 0
                    || (dividend == 0x8000_0000 && divisor == 0xFFFF_FFFF);
                let (q, r) = if divisor == 0 {
                    (0, dividend)
                } else {
                    (
                        (dividend as i32).wrapping_div(divisor as i32) as u32,
                        (dividend as i32).wrapping_rem(divisor as i32) as u32,
                    )
                };
                self.set_flag(FLAG_OV, overflow);
                self.write_reg(quot, q);
                self.write_reg(rem, r);
                self.set_sz(q);
            }
            Insn::Divu { a, b, quot, rem } => {
                let divisor = self.read_reg(a);
                let dividend = self.read_reg(b);
                let (q, r) = if divisor == 0 {
                    (0, dividend)
                } else {
                    (dividend / divisor, dividend % divisor)
                };
                self.set_flag(FLAG_OV, divisor == 0);
                self.write_reg(quot, q);
                self.write_reg(rem, r);
                self.set_sz(q);
            }
            Insn::Setf { cond, dst } => {
                let value = self.condition(cond) as u32;
                self.write_reg(dst, value);
            }
            Insn::Ld {
                disp,
                base,
                dst,
                size,
                unsigned,
            } => {
                let address = self.read_reg(base).wrapping_add(disp as u32);
                let value = match (size, unsigned) {
                    (MemSize::Byte, true) => self.read_u8(address) as u32,
                    (MemSize::Byte, false) => {
                        self.read_u8(address) as i8 as i32 as u32
                    }
                    (MemSize::Half, true) => self.read_u16(address) as u32,
                    (MemSize::Half, false) => {
                        self.read_u16(address) as i16 as i32 as u32
                    }
                    (MemSize::Word, _) => self.read_u32(address),
                };
                self.write_reg(dst, value);
            }
            Insn::St {
                disp,
                base,
                src,
                size,
            } => {
                let address = self.read_reg(base).wrapping_add(disp as u32);
                let value = self.read_reg(src);
                match size {
                    MemSize::Byte => self.write_u8(address, value as u8),
                    MemSize::Half => self.write_u16(address, value as u16),
                    MemSize::Word => self.write_u32(address, value),
                }
            }
            Insn::Bcond { cond, target } => {
                if self.condition(cond) {
                    self.pc = target;
                }
            }
            Insn::Jr { target } => self.pc = target,
            Insn::Jarl { target, reg } => {
                let return_address = start.wrapping_add(len);
                self.write_reg(reg, return_address);
                self.pc = target;
            }
            Insn::JmpReg { reg } => self.pc = self.read_reg(reg),
            Insn::JmpDisp { reg, disp } => {
                self.pc = self.read_reg(reg).wrapping_add(disp);
            }
            Insn::Ret => self.pc = self.read_reg(31),
            Insn::Prepare { list, imm5, mode } => {
                // Push r20..r31 in ascending order (r20 ends up highest).
                for reg in 20..=31 {
                    let bit = REGISTER_LIST_BIT[reg - 20];
                    if list & (1 << bit) != 0 {
                        let sp = self.read_reg(3).wrapping_sub(4);
                        self.write_reg(3, sp);
                        let value = self.read_reg(reg);
                        self.write_u32(sp, value);
                    }
                }
                let sp = self
                    .read_reg(3)
                    .wrapping_sub(imm5.wrapping_mul(4));
                self.write_reg(3, sp);
                match mode {
                    PrepareMode::Plain => {}
                    PrepareMode::StackPointer => self.write_reg(30, sp),
                    PrepareMode::Imm16Low(value) => {
                        self.write_reg(30, value as u32)
                    }
                    PrepareMode::Imm16High(value) => {
                        self.write_reg(30, (value as u32) << 16)
                    }
                    PrepareMode::Imm32(value) => self.write_reg(30, value),
                }
            }
            Insn::Dispose { list, imm5, ret } => {
                let sp = self
                    .read_reg(3)
                    .wrapping_add(imm5.wrapping_mul(4));
                self.write_reg(3, sp);
                // Pop in reverse push order: lp, ep, r29, ..., r20.
                for reg in (20..=31).rev() {
                    let bit = REGISTER_LIST_BIT[reg - 20];
                    if list & (1 << bit) != 0 {
                        let sp = self.read_reg(3);
                        let value = self.read_u32(sp);
                        self.write_reg(reg, value);
                        self.write_reg(3, sp.wrapping_add(4));
                    }
                }
                if let Some(reg) = ret {
                    self.pc = self.read_reg(reg);
                }
            }
            Insn::Unknown(_) => self.halted = true,
        }
    }
}
/// Disassemble the instruction at `pc` using `read` for little-endian bytes.
///
/// V850 instructions are 16-bit aligned and little-endian. Reads two bytes at
/// a time through `read`; extension words are consumed by `decode_at`. Unknown
/// opcodes render as `.word 0xXXXX`.
pub fn disassemble(pc: u32, read: &dyn Fn(u32) -> u8) -> String {
    let read16 = |addr: u32| -> u16 {
        read(addr) as u16 | ((read(addr.wrapping_add(1)) as u16) << 8)
    };
    let (insn, _len) = decode_at(pc, &read16);
    let rname = |index: usize| V850Core::register_label(index);
    match insn {
        Insn::Nop => "nop".to_string(),
        Insn::Halt => "halt".to_string(),
        Insn::InterruptEnable => "ei".to_string(),
        Insn::InterruptDisable => "di".to_string(),
        Insn::Mov { src, dst } => format!("mov {}, {}", rname(src), rname(dst)),
        Insn::MovI5 { imm, dst } => format!("mov {}, {}", imm, rname(dst)),
        Insn::MovI32 { imm, dst } => format!("mov 0x{imm:08x}, {}", rname(dst)),
        Insn::Movea { imm, src, dst } => {
            format!("movea {}, {}, {}", imm, rname(src), rname(dst))
        }
        Insn::Movhi { imm, src, dst } => {
            format!("movhi 0x{:04x}, {}, {}", imm as u16, rname(src), rname(dst))
        }
        Insn::Add { a, b } => format!("add {}, {}", rname(a), rname(b)),
        Insn::AddI5 { imm, b } => format!("add {}, {}", imm, rname(b)),
        Insn::Addi { imm, src, dst } => {
            format!("addi {}, {}, {}", imm, rname(src), rname(dst))
        }
        Insn::Sub { a, b } => format!("sub {}, {}", rname(a), rname(b)),
        Insn::Subr { a, b } => format!("subr {}, {}", rname(a), rname(b)),
        Insn::Cmp { a, b } => format!("cmp {}, {}", rname(a), rname(b)),
        Insn::CmpI5 { imm, b } => format!("cmp {}, {}", imm, rname(b)),
        Insn::And { a, b } => format!("and {}, {}", rname(a), rname(b)),
        Insn::Andi { imm, src, dst } => {
            format!("andi 0x{imm:08x}, {}, {}", rname(src), rname(dst))
        }
        Insn::Or { a, b } => format!("or {}, {}", rname(a), rname(b)),
        Insn::Ori { imm, src, dst } => {
            format!("ori 0x{imm:08x}, {}, {}", rname(src), rname(dst))
        }
        Insn::Xor { a, b } => format!("xor {}, {}", rname(a), rname(b)),
        Insn::Xori { imm, src, dst } => {
            format!("xori 0x{imm:08x}, {}, {}", rname(src), rname(dst))
        }
        Insn::Not { src, dst } => format!("not {}, {}", rname(src), rname(dst)),
        Insn::Tst { a, b } => format!("tst {}, {}", rname(a), rname(b)),
        Insn::ShlI5 { count, dst } => format!("shl {}, {}", count, rname(dst)),
        Insn::ShrI5 { count, dst } => format!("shr {}, {}", count, rname(dst)),
        Insn::SarI5 { count, dst } => format!("sar {}, {}", count, rname(dst)),
        Insn::ShlReg { count, dst } => format!("shl {}, {}", rname(count), rname(dst)),
        Insn::ShrReg { count, dst } => format!("shr {}, {}", rname(count), rname(dst)),
        Insn::SarReg { count, dst } => format!("sar {}, {}", rname(count), rname(dst)),
        Insn::Shl3 { count, src, dst } => {
            format!("shl {}, {}, {}", rname(count), rname(src), rname(dst))
        }
        Insn::Shr3 { count, src, dst } => {
            format!("shr {}, {}, {}", rname(count), rname(src), rname(dst))
        }
        Insn::Sar3 { count, src, dst } => {
            format!("sar {}, {}, {}", rname(count), rname(src), rname(dst))
        }
        Insn::Mul { a, b, low, high: _ } => {
            format!("mul {}, {}, {}", rname(a), rname(b), rname(low))
        }
        Insn::Mulu { a, b, low, high: _ } => {
            format!("mulu {}, {}, {}", rname(a), rname(b), rname(low))
        }
        Insn::Mulh { a, b } => format!("mulh {}, {}", rname(a), rname(b)),
        Insn::MulhI5 { imm, b } => format!("mulh {}, {}", imm, rname(b)),
        Insn::Mulhi { imm, src, dst } => {
            format!("mulhi {}, {}, {}", imm, rname(src), rname(dst))
        }
        Insn::Div { a, b, quot, rem: _ } => {
            format!("div {}, {}, {}", rname(a), rname(b), rname(quot))
        }
        Insn::Divu { a, b, quot, rem: _ } => {
            format!("divu {}, {}, {}", rname(a), rname(b), rname(quot))
        }
        Insn::Setf { cond, dst } => format!("setf {}, {}", cond, rname(dst)),
        Insn::Ld {
            disp,
            base,
            dst,
            size,
            unsigned,
        } => {
            let m = match size {
                MemSize::Byte if unsigned => "ld.bu",
                MemSize::Byte => "ld.b",
                MemSize::Half if unsigned => "ld.hu",
                MemSize::Half => "ld.h",
                MemSize::Word => "ld.w",
            };
            let off = if disp == 0 {
                format!("[{}]", rname(base))
            } else {
                format!("{disp}[{}]", rname(base))
            };
            format!("{m} {off}, {}", rname(dst))
        }
        Insn::St {
            disp,
            base,
            src,
            size,
        } => {
            let m = match size {
                MemSize::Byte => "st.b",
                MemSize::Half => "st.h",
                MemSize::Word => "st.w",
            };
            let off = if disp == 0 {
                format!("[{}]", rname(base))
            } else {
                format!("{disp}[{}]", rname(base))
            };
            format!("{m} {}, {off}", rname(src))
        }
        Insn::Bcond { cond, target } => format!("b{} 0x{target:08x}", cond_suffix(cond)),
        Insn::Jr { target } => format!("jr 0x{target:08x}"),
        Insn::Jarl { target, reg } => format!("jarl 0x{target:08x}, {}", rname(reg)),
        Insn::JmpReg { reg } => format!("jmp [{}]", rname(reg)),
        Insn::JmpDisp { reg, disp } => format!("jmp {disp}[{}]", rname(reg)),
        Insn::Ret => "ret".to_string(),
        Insn::Prepare { list, imm5, mode } => {
            let list_str = register_list_string(list);
            let mode_str = match mode {
                PrepareMode::Plain => String::new(),
                PrepareMode::StackPointer => ", sp".to_string(),
                PrepareMode::Imm16Low(value) => format!(", 0x{value:04x}"),
                PrepareMode::Imm16High(value) => format!(", 0x{value:04x}<<16"),
                PrepareMode::Imm32(value) => format!(", 0x{value:08x}"),
            };
            format!("prepare {list_str}, {imm5}{mode_str}")
        }
        Insn::Dispose { list, imm5, ret } => {
            let list_str = register_list_string(list);
            match ret {
                Some(r) => format!("dispose {list_str}, {imm5}, {}", rname(r)),
                None => format!("dispose {list_str}, {imm5}"),
            }
        }
        Insn::Unknown(word) => format!(".word 0x{word:04x}"),
    }
}

fn cond_suffix(cond: u8) -> &'static str {
    match cond & 0xF {
        0x0 => "v",
        0x1 => "c",
        0x2 => "e",
        0x3 => "nh",
        0x4 => "n",
        0x5 => "t",
        0x6 => "lt",
        0x7 => "le",
        0x8 => "nv",
        0x9 => "nc",
        0xA => "ne",
        0xB => "h",
        0xC => "p",
        0xD => "sa",
        0xE => "ge",
        _ => "gt",
    }
}

fn register_list_string(list: u32) -> String {
    let mut names = Vec::new();
    for reg in 20..=31 {
        let bit = REGISTER_LIST_BIT[reg - 20];
        if list & (1 << bit) != 0 {
            names.push(V850Core::register_label(reg));
        }
    }
    if names.is_empty() {
        String::new()
    } else {
        format!("[{}]", names.join(", "))
    }
}
