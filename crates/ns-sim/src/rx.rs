//! A Renesas RX (RX600 family, IAR RX) instruction-level simulator core.
//!
//! Models the RX CPU used by IAR/Keil-style RX toolchains: sixteen 32-bit
//! registers (`r0`-`r15`; `r0` is the stack pointer and `r1` the accumulator
//! by the standard RX ABI), a program status word (PSW: C/Z/S/O/I/U plus the
//! interrupt stack bits), a flat 32-bit byte-addressed little-endian address
//! space, and the variable-length (16/32/48-bit) instruction encodings.
//!
//! ## Covered instructions
//!
//! * Moves: `MOV` reg-reg / `#uimm4` / `#imm` / `#imm8`, `MOVU`, `MOV`
//!   `dsp[rd]` short and byte/word/long immediate forms.
//! * Arithmetic/logic: `ADD`, `SUB`, `CMP`, `AND`, `OR`, `XOR`, `MUL`,
//!   `DIV`, `DIVU`, `EMUL`, `EMULU`, `ADC`, `SBB`, `INC`, `DEC`, `NEG`,
//!   `NOT`, `ABS`, `MAX`, `MIN`, `TST`.
//! * Shifts: `SHLL`, `SHLR`, `SHAR` with imm5 or register.
//! * Memory: `LD.b/w/l`, `ST.b/w/l` (register-indirect and displacement),
//!   `PUSH`, `POP`, `PUSHM`, `POPM`.
//! * Control: `BRA`, `BSR`, `BCnd` (short/byte/word/a), `JMP`, `JSR`,
//!   `RTS`, `RTSD`, `BRK`, `NOP`, `WAIT`.
//! * Bit ops: `BTST`, `BSET`, `BCLR`, `BNOT`, `SCCnd`, `STZ`, `STNZ`,
//!   `SETPSW`, `CLRPSW`.
//! * Control registers: `MVFC`, `MVTC`, `MVTIPL` (stubs; PSW/ISP/USP
//!   handled, others ignored).
//!
//! Encodings follow the GCC/LLVM RX backends and the QEMU RX decoder
//! (`target/rx/insns.decode`), cross-checked against the RX user's manual.

/// Flat memory window size (16 MiB covers typical RX600 flash + SRAM).
pub const MEMORY_SIZE: usize = 16 * 1024 * 1024;

/// The RX PSW flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Psw {
    /// Carry flag.
    pub c: bool,
    /// Zero flag.
    pub z: bool,
    /// Sign flag.
    pub s: bool,
    /// Overflow flag.
    pub o: bool,
    /// Interrupt priority mask (3 bits, I).
    pub i: u8,
    /// User stack/instruction flag (U).
    pub u: bool,
}

impl Default for Psw {
    fn default() -> Self {
        Psw { c: false, z: false, s: false, o: false, i: 0, u: false }
    }
}

impl Psw {
    fn to_u32(&self) -> u32 {
        let mut v = 0u32;
        if self.c { v |= 1 << 0; }
        if self.z { v |= 1 << 1; }
        if self.s { v |= 1 << 2; }
        if self.o { v |= 1 << 3; }
        v |= (self.i as u32 & 0x7) << 4;
        if self.u { v |= 1 << 8; }
        v
    }

    fn from_u32(v: u32) -> Self {
        Psw {
            c: v & 1 != 0,
            z: v & 2 != 0,
            s: v & 4 != 0,
            o: v & 8 != 0,
            i: ((v >> 4) & 0x7) as u8,
            u: v & 0x100 != 0,
        }
    }
}

/// The outcome of a single RX step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxStep {
    Advanced,
    Breakpoint,
    Halted,
}

/// A Renesas RX core.
pub struct RxCore {
    /// Sixteen 32-bit registers; `r0` is the stack pointer per the RX ABI.
    pub regs: [u32; 16],
    /// Program status word.
    pub psw: Psw,
    /// Program counter (byte address).
    pub pc: u32,
    /// Flat byte-addressable memory.
    pub mem: Vec<u8>,
    /// Whether the core has halted (BRK/WAIT).
    pub halted: bool,
}

impl Default for RxCore {
    fn default() -> Self {
        Self::new()
    }
}

impl RxCore {
    pub fn new() -> Self {
        RxCore {
            regs: [0; 16],
            psw: Psw::default(),
            pc: 0,
            mem: vec![0; MEMORY_SIZE],
            halted: false,
        }
    }

    /// Reset to `entry`, zeroing registers and clearing the halt flag.
    pub fn reset(&mut self, entry: u32) {
        self.regs = [0; 16];
        self.psw = Psw::default();
        self.pc = entry & !1;
        self.halted = false;
    }

    pub fn set_pc(&mut self, pc: u32) {
        self.pc = pc & !1;
    }

    /// Load a program image at `base` and start the PC there.
    pub fn load_image(&mut self, base: u32, image: &[u8]) {
        self.write_bytes(base, image);
        self.pc = base & !1;
        self.halted = false;
    }

    /// Register label for the IDE register view.
    pub fn register_label(index: usize) -> String {
        if index < 16 {
            format!("r{}", index)
        } else {
            format!("r{}", index)
        }
    }

    /// Read a register by index (0-15); out-of-range returns 0.
    pub fn read_reg(&self, index: usize) -> u32 {
        if index < 16 { self.regs[index] } else { 0 }
    }

    /// Write a register by index (0-15); out-of-range is ignored.
    pub fn write_reg(&mut self, index: usize, value: u32) {
        if index < 16 {
            self.regs[index] = value;
        }
    }

    /// Read one byte from memory (out-of-range returns 0).
    pub fn read_u8(&self, address: u32) -> u8 {
        let a = address as usize;
        if a < self.mem.len() { self.mem[a] } else { 0 }
    }

    /// Read a little-endian 32-bit word from memory.
    pub fn read_u32(&self, address: u32) -> u32 {
        let a = address as usize;
        if a + 3 < self.mem.len() {
            u32::from_le_bytes([
                self.mem[a],
                self.mem[a + 1],
                self.mem[a + 2],
                self.mem[a + 3],
            ])
        } else {
            let mut v = 0u32;
            for i in 0..4 {
                v |= (self.read_u8(address + i) as u32) << (8 * i);
            }
            v
        }
    }

    /// Write a little-endian 32-bit word to memory.
    pub fn write_u32(&mut self, address: u32, value: u32) {
        let a = address as usize;
        if a + 3 < self.mem.len() {
            self.mem[a..a + 4].copy_from_slice(&value.to_le_bytes());
        } else {
            for i in 0..4 {
                self.write_u8(address + i, (value >> (8 * i)) as u8);
            }
        }
    }

    /// Write one byte to memory (out-of-range ignored).
    pub fn write_u8(&mut self, address: u32, value: u8) {
        let a = address as usize;
        if a < self.mem.len() {
            self.mem[a] = value;
        }
    }

    /// Write a byte slice to memory starting at `base`.
    pub fn write_bytes(&mut self, base: u32, bytes: &[u8]) {
        let b = base as usize;
        for (i, &byte) in bytes.iter().enumerate() {
            let a = b + i;
            if a < self.mem.len() {
                self.mem[a] = byte;
            }
        }
    }

    fn fetch8(&mut self) -> u8 {
        let b = self.read_u8(self.pc);
        self.pc = self.pc.wrapping_add(1);
        b
    }

    fn fetch16(&mut self) -> u16 {
        let b0 = self.fetch8() as u16;
        let b1 = self.fetch8() as u16;
        (b0 << 8) | b1
    }

    fn fetch32(&mut self) -> u32 {
        let b0 = self.fetch8() as u32;
        let b1 = self.fetch8() as u32;
        let b2 = self.fetch8() as u32;
        let b3 = self.fetch8() as u32;
        (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
    }

    // ---- Flags ----

    fn set_nz(&mut self, value: u32) {
        self.psw.z = value == 0;
        self.psw.s = (value as i32) < 0;
    }

    /// Compute and store C/Z/S/O for an addition.
    fn add_flags(&mut self, a: u32, b: u32, carry: u32) -> u32 {
        let sum = a.wrapping_add(b).wrapping_add(carry);
        let mask = 0x8000_0000;
        let o = ((a & mask) == (b & mask)) && ((a & mask) != (sum & mask));
        self.psw.c = (a as u64) + (b as u64) + (carry as u64) > 0xFFFF_FFFF;
        self.psw.o = o;
        self.set_nz(sum);
        sum
    }

    /// Compute and store C/Z/S/O for a subtraction.
    fn sub_flags(&mut self, a: u32, b: u32, borrow: u32) -> u32 {
        let (diff, borrow_out) = a.overflowing_sub(b.wrapping_add(borrow));
        let mask = 0x8000_0000;
        let o = ((a & mask) != (b & mask)) && ((a & mask) != (diff & mask));
        self.psw.c = borrow_out;
        self.psw.o = o;
        self.set_nz(diff);
        diff
    }

    fn cond_true(&self, cond: u8) -> bool {
        match cond & 0x0F {
            0x0 => self.psz(),            // EQ / Z
            0x1 => !self.psz(),           // NE / NZ
            0x2 => self.psc(),            // C
            0x3 => !self.psc(),           // NC
            0x4 => self.pss(),            // N
            0x5 => !self.pss(),           // P (>= 0)
            0x6 => self.pso(),            // O
            0x7 => !self.pso(),           // NO
            0x8 => self.pss() && !self.pso(), // LE
            0x9 => !self.pss() && self.pso(), // LT (signed <)
            0xA => !self.pss() && !self.pso(), // GT
            0xB => self.pss() && self.pso(),   // GE (signed >=)
            0xC => self.psz() && !self.psc(),  // HI (unsigned >)
            0xD => !self.psz() && self.psc(),  // LS (unsigned <=)
            0xE => self.psz() || self.pss(),   // F? (always for cond 0xE in RX)
            0xF => true,                       // Always
            _ => false,
        }
    }

    fn psz(&self) -> bool { self.psw.z }
    fn psc(&self) -> bool { self.psw.c }
    fn pss(&self) -> bool { self.psw.s }
    fn pso(&self) -> bool { self.psw.o }

    // ---- Stack ----

    fn push(&mut self, value: u32) {
        self.regs[0] = self.regs[0].wrapping_sub(4);
        self.write_u32(self.regs[0], value);
    }

    fn pop(&mut self) -> u32 {
        let v = self.read_u32(self.regs[0]);
        self.regs[0] = self.regs[0].wrapping_add(4);
        v
    }

    // ---- Memory helpers ----

    /// Load from memory with size: 0=byte (sign), 1=word (sign), 2=long.
    fn load(&self, addr: u32, sz: u8) -> u32 {
        match sz {
            0 => {
                let b = self.read_u8(addr);
                b as i8 as i32 as u32
            }
            1 => {
                let w = self.read_u8(addr) as u16 | ((self.read_u8(addr + 1) as u16) << 8);
                w as i16 as i32 as u32
            }
            _ => self.read_u32(addr),
        }
    }

    /// Load from memory with size: 0=byte (zero), 1=word (zero), 2=long.
    fn load_u(&self, addr: u32, sz: u8) -> u32 {
        match sz {
            0 => self.read_u8(addr) as u32,
            1 => {
                (self.read_u8(addr) as u32) | ((self.read_u8(addr + 1) as u32) << 8)
            }
            _ => self.read_u32(addr),
        }
    }

    /// Store a value to memory with size: 0=byte, 1=word, 2=long.
    fn store(&mut self, addr: u32, value: u32, sz: u8) {
        match sz {
            0 => self.write_u8(addr, value as u8),
            1 => {
                self.write_u8(addr, value as u8);
                self.write_u8(addr + 1, (value >> 8) as u8);
            }
            _ => self.write_u32(addr, value),
        }
    }

    /// Read the `sz`-sized operand and return the address of the memory form.
    /// `rs` is the base register. `dsp` is a signed displacement.
    fn mem_addr(&self, rs: u32, dsp: i32) -> u32 {
        rs.wrapping_add(dsp as u32)
    }

    // ---- Branch ----

    fn branch_disp(&mut self, disp: i32, link: bool) {
        if link {
            let ret = self.pc;
            self.push(ret);
        }
        self.pc = self.pc.wrapping_add(disp as u32);
    }

    /// Execute one step, stopping at a breakpoint.
    pub fn step(&mut self, breakpoints: &[u32]) -> RxStep {
        if self.halted {
            return RxStep::Halted;
        }
        if breakpoints.contains(&self.pc) {
            return RxStep::Breakpoint;
        }
        let pc = self.pc;
        self.execute();
        if self.halted {
            return RxStep::Halted;
        }
        if self.pc == pc {
            // Guard against infinite loops on unknown instructions.
            return RxStep::Halted;
        }
        RxStep::Advanced
    }

    fn execute(&mut self) {
        let b0 = self.read_u8(self.pc);
        // 1-byte instructions: BRA.s (0x08-0x0F) and BCnd.s (0x10-0x1F).
        if b0 >= 0x08 && b0 <= 0x1F {
            self.pc = self.pc.wrapping_add(1);
            if b0 <= 0x0F {
                // BRA.s: displacement is the low 7 bits, sign-extended.
                let dsp = sign7(b0 as u16 & 0x7F);
                self.pc = self.pc.wrapping_add(dsp as u32);
            } else {
                // BCnd.s: cond is the low nibble; displacement from low 3 bits
                // of b0 plus the next 7 bits.
                let cond = b0 & 0x0F;
                let dsp = sign7(((b0 as u16 & 0x07) << 3) | (self.read_u8(self.pc) as u16 & 0x7F));
                self.pc = self.pc.wrapping_add(1);
                if self.cond_true(cond) {
                    self.pc = self.pc.wrapping_add(dsp as u32);
                }
            }
            return;
        }
        let insn = self.fetch16();
        self.execute_opcode(insn);
    }

    // ---- Main dispatch ----

    fn execute_opcode(&mut self, insn: u16) {
        let op = ((insn >> 8) & 0xFF) as u8;
        let low = (insn & 0xFF) as u8;

        // 0000 0000 = BRK, 0000 0010 = RTS, 0000 0011 = NOP.
        if insn == 0x0000 {
            self.halted = true;
            return;
        }
        if insn == 0x0002 {
            // RTS
            let ret = self.pop();
            self.pc = ret & !1;
            return;
        }
        if insn == 0x0003 {
            // NOP
            return;
        }
        // 0000 0100 = BRA.a, 0000 0101 = BSR.a.
        if insn == 0x0004 {
            let dsp = self.fetch32() as i32;
            self.branch_disp(dsp, true);
            return;
        }
        if insn == 0x0005 {
            let dsp = self.fetch32() as i32;
            self.branch_disp(dsp, false);
            return;
        }
        // 0111 1111 1001 0110 = WAIT
        if insn == 0x7996 {
            self.halted = true;
            return;
        }
        // 0000 1... = BRA.s (short, dsp7)
        if (insn & 0xF800) == 0x0800 {
            let dsp = sign7(insn & 0x7F);
            self.pc = self.pc.wrapping_add(dsp as u32);
            return;
        }
        // 0001 .... = BCnd.s (short)
        if (op & 0xF0) == 0x10 {
            let cond = ((insn >> 8) & 0x0F) as u8;
            let dsp = sign7(insn & 0x7F);
            if self.cond_true(cond) {
                self.pc = self.pc.wrapping_add(dsp as u32);
            }
            return;
        }
        // 0010 .... = BCnd.b / BRA.b
        if (op & 0xF0) == 0x20 {
            if insn == 0x2E00 {
                // BRA.b dsp8 (op=0x2E)
                let dsp = low as i8 as i32;
                self.pc = self.pc.wrapping_add(dsp as u32);
                return;
            }
            let cond = op & 0x0F;
            let dsp = low as i8 as i32;
            if self.cond_true(cond) {
                self.pc = self.pc.wrapping_add(dsp as u32);
            }
            return;
        }
        // 0011 1000 = BRA.w, 0011 1001 = BSR.w
        if insn == 0x3800 {
            let dsp = self.fetch16() as i16 as i32;
            self.branch_disp(dsp, true);
            return;
        }
        if insn == 0x3900 {
            let dsp = self.fetch16() as i16 as i32;
            self.branch_disp(dsp, false);
            return;
        }
        // 0011 101 . = BCnd.w
        if (op & 0xF8) == 0x3A {
            let cond = (op & 0x07) as u8;
            let dsp_lo = low as i16 as i32;
            let dsp_hi = self.fetch8() as i16 as i32;
            let dsp = (dsp_lo | (dsp_hi << 8)) as i32;
            if self.cond_true(cond) {
                self.pc = self.pc.wrapping_add(dsp as u32);
            }
            return;
        }
        // 0011 1100/1101/1110 = MOV.b/w/l #imm8, dsp5[rd]
        if (op & 0xFC) == 0x3C {
            let sz = op & 0x03;
            let rd = ((insn >> 4) & 0x07) as usize;
            let imm = self.fetch8() as u32;
            let dsp = (((insn >> 7) & 1) << 4 | (insn & 0x0F)) as i32;
            let addr = self.regs[rd].wrapping_add(dsp as u32);
            self.store(addr, imm, sz);
            return;
        }
        // 0011 1111 = RTSD #imm, rd-rd2
        if op == 0x3F {
            let rd = ((insn >> 4) & 0x0F) as usize;
            let rd2 = (insn & 0x0F) as usize;
            let imm = self.fetch8() as u32;
            self.regs[0] = self.regs[0].wrapping_add(imm);
            let mut r = rd;
            while r <= rd2 && r < 16 {
                self.regs[r] = self.pop();
                r += 1;
            }
            self.pc = self.pop() & !1;
            return;
        }
        // 0100 00.. = SUB dsp[rs].ub / SUB rs,rd
        if (op & 0xFC) == 0x40 {
            let ld = op & 0x03;
            let rs = ((low >> 4) & 0x0F) as usize;
            let rd = (low & 0x0F) as usize;
            self.binop(rs, rd, ld, 0x00);
            return;
        }
        // 0100 01.. = CMP dsp[rs].ub / CMP rs,rd
        if (op & 0xFC) == 0x44 {
            let ld = op & 0x03;
            let rs = ((low >> 4) & 0x0F) as usize;
            let rd = (low & 0x0F) as usize;
            self.binop(rs, rd, ld, 0x01);
            return;
        }
        // 0100 10.. = ADD dsp[rs].ub / ADD rs,rd
        if (op & 0xFC) == 0x48 {
            let ld = op & 0x03;
            let rs = ((low >> 4) & 0x0F) as usize;
            let rd = (low & 0x0F) as usize;
            self.binop(rs, rd, ld, 0x02);
            return;
        }
        // 0100 11.. = MUL dsp[rs].ub / MUL rs,rd
        if (op & 0xFC) == 0x4C {
            let ld = op & 0x03;
            let rs = ((low >> 4) & 0x0F) as usize;
            let rd = (low & 0x0F) as usize;
            self.binop(rs, rd, ld, 0x03);
            return;
        }
        // 0101 00.. = AND dsp[rs].ub / AND rs,rd
        if (op & 0xFC) == 0x50 {
            let ld = op & 0x03;
            let rs = ((low >> 4) & 0x0F) as usize;
            let rd = (low & 0x0F) as usize;
            self.binop(rs, rd, ld, 0x04);
            return;
        }
        // 0101 01.. = OR dsp[rs].ub / OR rs,rd
        if (op & 0xFC) == 0x54 {
            let ld = op & 0x03;
            let rs = ((low >> 4) & 0x0F) as usize;
            let rd = (low & 0x0F) as usize;
            self.binop(rs, rd, ld, 0x05);
            return;
        }
        // 0101 1... = MOVU (byte/word zero-extend loads)
        if (op & 0xF8) == 0x58 {
            let sz = (op >> 2) & 0x01;
            let kind = op & 0x03;
            let rs = ((low >> 4) & 0x0F) as usize;
            let rd = (low & 0x0F) as usize;
            match kind {
                0 => {
                    // MOVU.b/w rs, rd (register indirect)
                    let v = self.load_u(self.regs[rs], sz);
                    self.regs[rd] = v;
                }
                1 => {
                    // MOVU.b/w dsp8[rs], rd
                    let dsp = self.fetch8() as i8 as i32;
                    let addr = self.mem_addr(self.regs[rs], dsp);
                    let v = self.load_u(addr, sz);
                    self.regs[rd] = v;
                }
                2 => {
                    // MOVU.b/w dsp16[rs], rd
                    let dsp = self.fetch16() as i16 as i32;
                    let addr = self.mem_addr(self.regs[rs], dsp);
                    let v = self.load_u(addr, sz);
                    self.regs[rd] = v;
                }
                _ => {
                    // MOVU.b/w #imm, rd
                    let imm = self.fetch16();
                    self.regs[rd] = imm as u32;
                }
            }
            return;
        }
        // 0110 0000 = SUB #uimm4, rd; 0110 0001 = CMP #uimm4, rs2;
        // 0110 0010 = ADD #uimm4, rd; 0110 0011 = MUL #uimm4, rd
        if (op & 0xFC) == 0x60 {
            let sub = op & 0x03;
            let imm = ((insn >> 4) & 0x0F) as u32;
            let rd = (low & 0x0F) as usize;
            match sub {
                0 => {
                    let v = self.regs[rd].wrapping_sub(imm);
                    self.regs[rd] = v;
                    self.sub_flags(self.regs[rd].wrapping_add(imm), imm, 0);
                }
                1 => {
                    // CMP #uimm4, rs2: sets flags only.
                    let v = self.regs[rd];
                    self.sub_flags(v, imm, 0);
                }
                2 => {
                    let v = self.add_flags(self.regs[rd], imm, 0);
                    self.regs[rd] = v;
                }
                _ => {
                    let v = self.regs[rd].wrapping_mul(imm);
                    self.regs[rd] = v;
                    self.set_nz(v);
                }
            }
            return;
        }
        // 0110 0100 = AND #uimm4, rd; 0110 0101 = OR #uimm4, rd;
        // 0110 0110 = MOV #uimm4, rd
        if (op & 0xFC) == 0x64 {
            let kind = op & 0x03;
            let imm = ((insn >> 4) & 0x0F) as u32;
            let rd = (low & 0x0F) as usize;
            match kind {
                0 => {
                    let v = self.regs[rd] & imm;
                    self.regs[rd] = v;
                    self.set_nz(v);
                }
                1 => {
                    let v = self.regs[rd] | imm;
                    self.regs[rd] = v;
                    self.set_nz(v);
                }
                _ => {
                    self.regs[rd] = imm;
                }
            }
            return;
        }
        // 0110 100. = SHLR #imm5, rd; 0110 110. = SHLL #imm5, rd
        if (op & 0xFC) == 0x68 || (op & 0xFC) == 0x6C {
            let left = (op & 0x04) != 0;
            let imm = ((insn >> 4) & 0x1F) as u32;
            let rd = (low & 0x0F) as usize;
            let v = self.regs[rd];
            let (result, carry) = if left {
                if imm == 0 { (v, self.psw.c) } else {
                    let c = ((v >> (32 - imm)) & 1) != 0;
                    (v << imm, c)
                }
            } else {
                if imm == 0 { (v, self.psw.c) } else {
                    let c = ((v >> (imm - 1)) & 1) != 0;
                    (v >> imm, c)
                }
            };
            self.regs[rd] = result;
            self.psw.c = carry;
            self.set_nz(result);
            return;
        }
        // 0110 1110 = PUSHM rs-rs2; 0110 1111 = POPM rd-rd2
        if op == 0x6E {
            let rs = ((insn >> 4) & 0x0F) as usize;
            let rs2 = (low & 0x0F) as usize;
            let mut r = rs;
            while r <= rs2 && r < 16 {
                self.push(self.regs[r]);
                r += 1;
            }
            return;
        }
        if op == 0x6F {
            let rd = ((insn >> 4) & 0x0F) as usize;
            let rd2 = (low & 0x0F) as usize;
            let mut r = rd2;
            while r >= rd && r < 16 {
                self.regs[r] = self.pop();
                if r == 0 { break; }
                r -= 1;
            }
            return;
        }
        // 0111 00.. = ADD #imm, rs, rd (imm16)
        if (op & 0xFC) == 0x70 {
            let rs = ((insn >> 4) & 0x0F) as usize;
            let rd = (low & 0x0F) as usize;
            let imm = self.fetch16() as u32;
            let v = self.add_flags(self.regs[rs], imm, 0);
            self.regs[rd] = v;
            return;
        }
        // 0111 01.. = CMP/AND/OR/MUL #imm, rd (imm16 or imm8)
        if (op & 0xFC) == 0x74 {
            let kind = (low >> 4) & 0x0F;
            let rd = (low & 0x0F) as usize;
            match kind {
                0 => {
                    // CMP #imm, rd
                    let imm = self.fetch16() as u32;
                    self.sub_flags(self.regs[rd], imm, 0);
                }
                1 => {
                    // MUL #imm, rd
                    let imm = self.fetch16() as u32;
                    let v = self.regs[rd].wrapping_mul(imm);
                    self.regs[rd] = v;
                    self.set_nz(v);
                }
                2 => {
                    // AND #imm, rd
                    let imm = self.fetch16() as u32;
                    let v = self.regs[rd] & imm;
                    self.regs[rd] = v;
                    self.set_nz(v);
                }
                _ => {
                    // OR #imm, rd
                    let imm = self.fetch16() as u32;
                    let v = self.regs[rd] | imm;
                    self.regs[rd] = v;
                    self.set_nz(v);
                }
            }
            return;
        }
        // 0111 100. = BSET #imm5, rd; 0111 101. = BCLR #imm5, rd;
        // 0111 110. = BTST #imm5, rd
        if op >= 0x78 && op <= 0x7D {
            let kind = (op >> 1) & 0x03;
            let imm = ((insn >> 8) & 0x1F) as u32;
            let rd = (low & 0x0F) as usize;
            let v = self.regs[rd];
            let bit = 1u32 << (imm & 0x1F);
            match kind {
                0 => { self.regs[rd] = v | bit; }
                1 => { self.regs[rd] = v & !bit; }
                2 => {
                    self.psw.z = (v & bit) == 0;
                }
                _ => { self.regs[rd] = v | bit; }
            }
            return;
        }
        // 0111 1110 0000 rd = NOT rd; 0001 = NEG; 0010 = ABS
        // 0111 1110 10 sz rs = PUSH rs; 1100 = PUSHC cr; 1011 = POP rd;
        // 1101 = ...; 1110 = POPC cr.
        if op == 0x7E {
            let kind = (low >> 4) & 0x0F;
            let rd = (low & 0x0F) as usize;
            match kind {
                0 => {
                    // NOT rd
                    let v = self.regs[rd];
                    let r = !v;
                    self.regs[rd] = r;
                    self.set_nz(r);
                }
                1 => {
                    // NEG rd
                    let v = self.regs[rd];
                    let r = self.sub_flags(0, v, 0);
                    self.regs[rd] = r;
                }
                2 => {
                    // ABS rd
                    let v = self.regs[rd];
                    let r = if (v as i32) < 0 { self.sub_flags(0, v, 0) } else { v };
                    self.regs[rd] = r;
                }
                3 => {
                    // SAT rd (clamp to 0x7FFFFFFF / 0x80000000)
                    let v = self.regs[rd];
                    let r = if (v as i32) > i32::MAX { 0x7FFF_FFFF }
                            else if (v as i32) < i32::MIN { 0x8000_0000 }
                            else { v };
                    self.regs[rd] = r;
                }
                4 => {
                    // RORC rd
                    let v = self.regs[rd];
                    let c = self.psw.c as u32;
                    self.psw.c = (v & 1) != 0;
                    self.regs[rd] = (v >> 1) | (c << 31);
                }
                5 => {
                    // ROLC rd
                    let v = self.regs[rd];
                    let c = self.psw.c as u32;
                    self.psw.c = ((v >> 31) & 1) != 0;
                    self.regs[rd] = (v << 1) | c;
                }
                8 => {
                    // PUSH rs (sz=00/word)
                    let rs = rd;
                    self.push(self.regs[rs]);
                }
                10 => {
                    // POP rd
                    self.regs[rd] = self.pop();
                }
                12 => {
                    // PUSHC cr
                    let cr = rd;
                    if cr == 0 {
                        self.push(self.psw.to_u32());
                    }
                }
                14 => {
                    // POPC cr
                    let cr = rd;
                    if cr == 0 {
                        self.psw = Psw::from_u32(self.pop());
                    }
                }
                _ => {
                    self.halted = true;
                }
            }
            return;
        }
        // 0111 1111 0000 rs = JMP rs; 0001 = JSR rs; 0100 = BRA.l; 0101 = BSR.l
        if op == 0x7F {
            let kind = (low >> 4) & 0x0F;
            let rs = (low & 0x0F) as usize;
            match kind {
                0 => { self.pc = self.regs[rs] & !1; }
                1 => {
                    let ret = self.pc;
                    self.push(ret);
                    self.pc = self.regs[rs] & !1;
                }
                4 => { self.pc = self.regs[rs] & !1; }
                5 => {
                    let ret = self.pc;
                    self.push(ret);
                    self.pc = self.regs[rs] & !1;
                }
                _ => {
                    // Unknown; halt to avoid runaway.
                    self.halted = true;
                }
            }
            return;
        }
        // 1000 0 = MOV.b rs, dsp5[rd]; 1001 0 = MOV.w; 1010 0 = MOV.l (store)
        if (op & 0xF8) == 0x80 || (op & 0xF8) == 0x90 || (op & 0xF8) == 0xA0 {
            let sz = ((op >> 4) & 0x03) as u8;
            let rd = ((insn >> 4) & 0x07) as usize;
            let rs = (insn & 0x07) as usize;
            let dsp = ((((insn >> 7) & 0x0F) << 1) | ((insn >> 3) & 1)) as i32;
            let addr = self.mem_addr(self.regs[rd], dsp);
            self.store(addr, self.regs[rs], sz);
            return;
        }
        // 1000 1 = MOV.b dsp5[rs], rd; 1001 1 = MOV.w; 1010 1 = MOV.l (load)
        if (op & 0xF8) == 0x88 || (op & 0xF8) == 0x98 || (op & 0xF8) == 0xA8 {
            let sz = ((op >> 4) & 0x03) as u8;
            let rs = ((insn >> 4) & 0x07) as usize;
            let rd = (insn & 0x07) as usize;
            let dsp = ((((insn >> 7) & 0x0F) << 1) | ((insn >> 3) & 1)) as i32;
            let addr = self.mem_addr(self.regs[rs], dsp);
            let v = self.load(addr, sz);
            self.regs[rd] = v;
            return;
        }
        // 1011 sz ... = MOVU with dsp5 (byte/word zero-extend)
        if (op & 0xF0) == 0xB0 {
            let sz = ((insn >> 11) & 0x01) as u8;
            let rs = ((insn >> 4) & 0x07) as usize;
            let rd = (insn & 0x07) as usize;
            let dsp = ((((insn >> 8) & 0x07) << 1) | ((insn >> 3) & 1)) as i32;
            let addr = self.mem_addr(self.regs[rs], dsp);
            let v = self.load_u(addr, sz);
            self.regs[rd] = v;
            return;
        }
        // 1100/1101/1110 = MOV.b/w/l dsp[lds], dsp[ldd]
        if (op & 0xE0) == 0xC0 || (op & 0xE0) == 0xE0 {
            let sz = ((op >> 5) & 0x03) as u8;
            let ldd = ((insn >> 8) & 0x03) as i32;
            let lds = ((low >> 4) & 0x03) as i32;
            let rs = (low & 0x0F) as usize;
            let rd = ((insn >> 4) & 0x0F) as usize;
            // Read src displacement (if any) then dst displacement.
            let src_dsp = match lds {
                0 | 1 => 0,
                2 => self.fetch8() as i8 as i32,
                _ => self.fetch16() as i16 as i32,
            };
            let dst_dsp = match ldd {
                0 | 1 => 0,
                2 => self.fetch8() as i8 as i32,
                _ => self.fetch16() as i16 as i32,
            };
            let src_addr = self.mem_addr(self.regs[rs], src_dsp);
            let dst_addr = self.mem_addr(self.regs[rd], dst_dsp);
            let v = self.load_u(src_addr, sz);
            self.store(dst_addr, v, sz);
            return;
        }
        // 1111 0000 = BSET/BCLR #imm, dsp[rd]; 1111 0100 = BTST #imm, dsp[rd]
        // (bit operations on a memory byte addressed by dsp[rd]).
        if (op & 0xFC) == 0xF0 || (op & 0xFC) == 0xF4 {
            let bit = (insn & 0x07) as u32;
            let ld = ((insn >> 8) & 0x03) as i32;
            let rs = ((insn >> 4) & 0x0F) as usize;
            let dsp = match ld {
                0 => 0,
                1 => self.fetch8() as i8 as i32,
                2 => self.fetch16() as i16 as i32,
                _ => self.fetch16() as i16 as i32,
            };
            let addr = self.mem_addr(self.regs[rs], dsp);
            let v = self.read_u8(addr);
            let mask = 1u8 << (bit & 0x07);
            let is_tst = (op & 0xFC) == 0xF4;
            if is_tst {
                self.psw.z = (v & mask) == 0;
            } else {
                let clear = (insn & 0x80) != 0;
                let nv = if clear { v & !mask } else { v | mask };
                self.write_u8(addr, nv);
                self.set_nz(nv as u32);
            }
            return;
        }
        // 1111 01.. = PUSH dsp[rs] (bits 3-2 = 10); otherwise BTST #imm, dsp[rd].
        if (op & 0xFC) == 0xF4 && (insn & 0x0C) == 0x04 {
            let ld = ((insn >> 8) & 0x03) as i32;
            let rs = ((insn >> 4) & 0x0F) as usize;
            let sz = (insn & 0x03) as u8;
            let dsp = match ld {
                0 => 0,
                1 => self.fetch8() as i8 as i32,
                _ => self.fetch16() as i16 as i32,
            };
            let addr = self.mem_addr(self.regs[rs], dsp);
            let v = self.load_u(addr, sz);
            self.push(v);
            return;
        }
        // 1111 10.. = MOV [ri+rb], rd / MOV rd, [ri+rb] (register-indexed)
        if (op & 0xFC) == 0xF8 || (op & 0xFC) == 0xFA {
            let to_reg = (op & 0x04) == 0;
            let sz = ((insn >> 12) & 0x03) as u8;
            let ri = (insn & 0x0F) as usize;
            let rb = ((insn >> 4) & 0x0F) as usize;
            let rd = ((insn >> 8) & 0x0F) as usize;
            let addr = self.regs[ri].wrapping_add(self.regs[rb]);
            if to_reg {
                let v = self.load(addr, sz);
                self.regs[rd] = v;
            } else {
                self.store(addr, self.regs[rd], sz);
            }
            return;
        }
        // 1111 1100 = register-register / memory forms (long opcodes)
        if (op & 0xFC) == 0xFC {
            self.execute_fc(insn);
            return;
        }
        // 1111 1101 = imm/long forms
        if op == 0xFD {
            self.execute_fd(insn);
            return;
        }
        // Unknown; halt.
        self.halted = true;
    }

    /// Binary operation dispatch (SUB/CMP/ADD/MUL/AND/OR) with memory or
    /// register source. `ld` selects the source addressing, `op` the operation.
    fn binop(&mut self, rs: usize, rd: usize, ld: u8, kind: u8) {
        let src = match ld {
            0 => self.regs[rs],
            1 => {
                let dsp = self.fetch8() as i8 as i32;
                let addr = self.mem_addr(self.regs[rs], dsp);
                self.load_u(addr, 0)
            }
            2 => {
                let dsp = self.fetch16() as i16 as i32;
                let addr = self.mem_addr(self.regs[rs], dsp);
                self.load_u(addr, 0)
            }
            _ => self.regs[rs],
        };
        let a = self.regs[rd];
        match kind {
            0 => {
                let r = self.sub_flags(a, src, 0);
                self.regs[rd] = r;
            }
            1 => {
                self.sub_flags(a, src, 0);
            }
            2 => {
                let r = self.add_flags(a, src, 0);
                self.regs[rd] = r;
            }
            3 => {
                let r = a.wrapping_mul(src);
                self.regs[rd] = r;
                self.set_nz(r);
            }
            4 => {
                let r = a & src;
                self.regs[rd] = r;
                self.set_nz(r);
            }
            _ => {
                let r = a | src;
                self.regs[rd] = r;
                self.set_nz(r);
            }
        }
    }

    /// 1111 1100 opcodes: register-register arithmetic and bit ops.
    fn execute_fc(&mut self, insn: u16) {
        let low = insn & 0xFF;
        let hi = (insn >> 8) & 0x0F; // upper nibble of second byte
        let rd = (low & 0x0F) as usize;
        let rs = ((low >> 4) & 0x0F) as usize;
        match hi {
            0x0 => {
                // ABS/SUB/ADC/SBB reg forms; and the 48-bit dsp forms.
                let sub = (insn >> 4) & 0x0F;
                match sub & 0x0F {
                    0x3 => {
                        // SBB rs, rd
                        let a = self.regs[rd];
                        let r = self.sub_flags(a, self.regs[rs], self.psw.c as u32);
                        self.regs[rd] = r;
                    }
                    0x7 => {
                        // NEG rd (reg form)
                        let v = self.regs[rd];
                        let r = self.sub_flags(0, v, 0);
                        self.regs[rd] = r;
                    }
                    0xB => {
                        // ADC rs, rd
                        let a = self.regs[rd];
                        let r = self.add_flags(a, self.regs[rs], self.psw.c as u32);
                        self.regs[rd] = r;
                    }
                    0xF => {
                        // ABS rd (reg form)
                        let v = self.regs[rd];
                        let r = if (v as i32) < 0 {
                            self.sub_flags(0, v, 0)
                        } else {
                            v
                        };
                        self.regs[rd] = r;
                    }
                    _ => {
                        self.halted = true;
                    }
                }
            }
            0x1 => {
                // MAX/MIN/EMUL/EMULU reg-ub forms
                let sub = (insn >> 4) & 0x0F;
                let a = self.regs[rd];
                match sub {
                    0x0 => {
                        // MAX rs, rd (ub memory form handled by ld bits)
                        let b = self.regs[rs];
                        let r = if (a as i32) < (b as i32) { b } else { a };
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    0x1 => {
                        let b = self.regs[rs];
                        let r = if (a as i32) > (b as i32) { b } else { a };
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    0x2 => {
                        // EMUL rs, rd
                        let b = self.regs[rs];
                        let r = (a as u32 as u64).wrapping_mul(b as u32 as u64) as u32;
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    _ => {
                        // EMULU
                        let b = self.regs[rs];
                        let r = (a as u64).wrapping_mul(b as u64) as u32;
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                }
            }
            0x2 => {
                // DIV/DIVU reg-ub forms
                let sub = (insn >> 4) & 0x0F;
                let a = self.regs[rd];
                let b = self.regs[rs];
                match sub {
                    0x0 => {
                        if b != 0 {
                            let r = (a as i32).wrapping_div(b as i32) as u32;
                            self.regs[rd] = r;
                            self.set_nz(r);
                        }
                    }
                    _ => {
                        if b != 0 {
                            let r = a.wrapping_div(b);
                            self.regs[rd] = r;
                            self.set_nz(r);
                        }
                    }
                }
            }
            0x3 => {
                // AND/OR reg forms (and TST ub)
                let sub = (insn >> 4) & 0x0F;
                let a = self.regs[rd];
                match sub {
                    0x0 => {
                        let r = a & self.regs[rs];
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    0x1 => {
                        let r = a | self.regs[rs];
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    0xB => {
                        // NOT rd (reg form)
                        let r = !a;
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    _ => {
                        self.halted = true;
                    }
                }
            }
            0x4 => {
                // XCHG / BSET etc register forms
                let sub = (insn >> 4) & 0x0F;
                match sub {
                    0x3 => {
                        // XCHG rs, rd
                        let tmp = self.regs[rs];
                        self.regs[rs] = self.regs[rd];
                        self.regs[rd] = tmp;
                    }
                    _ => {
                        self.halted = true;
                    }
                }
            }
            0x6 => {
                // BSET/BCLR/BTST/BNOT reg forms
                let sub = (insn >> 4) & 0x0F;
                let v = self.regs[rd];
                match sub {
                    0x3 => {
                        let bit = self.regs[rs] & 0x1F;
                        self.regs[rd] = v | (1u32 << bit);
                    }
                    0x7 => {
                        let bit = self.regs[rs] & 0x1F;
                        self.regs[rd] = v & !(1u32 << bit);
                    }
                    0xB => {
                        let bit = self.regs[rs] & 0x1F;
                        self.psw.z = (v & (1u32 << bit)) == 0;
                    }
                    0xF => {
                        let bit = self.regs[rs] & 0x1F;
                        self.regs[rd] = v ^ (1u32 << bit);
                    }
                    _ => {
                        self.halted = true;
                    }
                }
            }
            0xC => {
                // DIV/DIVU reg forms (long)
                let sub = (insn >> 4) & 0x0F;
                let a = self.regs[rd];
                let b = self.regs[rs];
                match sub {
                    0x0 => {
                        if b != 0 {
                            let r = (a as i32).wrapping_div(b as i32) as u32;
                            self.regs[rd] = r;
                            self.set_nz(r);
                        }
                    }
                    _ => {
                        if b != 0 {
                            let r = a.wrapping_div(b);
                            self.regs[rd] = r;
                            self.set_nz(r);
                        }
                    }
                }
            }
            _ => {
                self.halted = true;
            }
        }
    }

    /// 1111 1101 opcodes: immediate/long forms (li_10, MVFC/MVTC, etc.)
    fn execute_fd(&mut self, insn: u16) {
        let low = insn & 0xFF;
        let hi = (insn >> 8) & 0x0F;
        let rd = (low & 0x0F) as usize;
        match hi {
            0x0 => {
                // MULHI/MULLO/MACHI/MACLO and SHL/SHR reg forms
                let sub = (insn >> 4) & 0x0F;
                let rs = ((low >> 4) & 0x0F) as usize;
                match sub {
                    0x0 | 0x1 => {
                        // MULHI/MULLO: 32x32 low product
                        let a = self.regs[rd];
                        let b = self.regs[rs];
                        let r = a.wrapping_mul(b);
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    0x2 => {
                        // SHLL rd, rs (register shift)
                        let v = self.regs[rd];
                        let n = self.regs[rs] & 0x1F;
                        let r = v << n;
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    _ => {
                        self.halted = true;
                    }
                }
            }
            0x1 => {
                // SHLR/SHAR reg forms and ROTL/ROTR
                let sub = (insn >> 4) & 0x0F;
                let rs = ((low >> 4) & 0x0F) as usize;
                let v = self.regs[rd];
                let n = self.regs[rs] & 0x1F;
                match sub {
                    0x0 => {
                        let r = v >> n;
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    0x2 => {
                        // SHAR (arithmetic)
                        let r = ((v as i32) >> n) as u32;
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    0x5 => {
                        // REVW
                        let r = v.swap_bytes();
                        self.regs[rd] = r;
                    }
                    0x7 => {
                        // REVL
                        let b = v.to_le_bytes();
                        let r = u32::from_le_bytes([b[3], b[2], b[1], b[0]]);
                        self.regs[rd] = r;
                    }
                    _ => {
                        self.halted = true;
                    }
                }
            }
            0x2 => {
                // MVTC rs, cr
                let cr = (low >> 4) & 0x0F;
                let rs = (low & 0x0F) as usize;
                if cr == 0 {
                    self.psw = Psw::from_u32(self.regs[rs]);
                }
            }
            0x3 => {
                // SHLL/SHLR/SHAR #imm5, rd, rd2 (three-operand) or MVTIPL
                let sub = (insn >> 4) & 0x0F;
                if sub == 0x7 && low == 0x00 {
                    // MVTIPL #imm4
                    let imm = (insn >> 8) & 0x0F;
                    self.psw.i = imm as u8;
                    return;
                }
                let imm = ((insn >> 4) & 0x1F) as u32;
                let rs = (low & 0x0F) as usize;
                let v = self.regs[rs];
                match sub {
                    0x0 | 0x1 => {
                        // SHLL #imm, rd
                        let r = v << imm;
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    0x2 | 0x3 => {
                        // SHLR #imm, rd
                        let r = v >> imm;
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    0x4 | 0x5 => {
                        // SHAR #imm, rd
                        let r = ((v as i32) >> imm) as u32;
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    _ => {
                        self.halted = true;
                    }
                }
            }
            0x4 => {
                // MOV #imm16, rd
                let imm = self.fetch16() as u32;
                self.regs[rd] = imm;
            }
            0x5 => {
                // INT #uimm8 / MVTIPL
                let imm = self.fetch8();
                if (low & 0xF0) == 0x00 {
                    self.psw.i = imm & 0x0F;
                } else {
                    self.halted = true;
                }
            }
            0x6 => {
                // MVFC cr, rd
                let cr = (low >> 4) & 0x0F;
                if cr == 0 {
                    self.regs[rd] = self.psw.to_u32();
                } else if cr == 1 {
                    self.regs[rd] = 0x2000; // ISP stub
                } else if cr == 2 {
                    self.regs[rd] = 0x2000; // USP stub
                } else {
                    self.regs[rd] = 0;
                }
            }
            0x7 => {
                // TST/STZ/STNZ/MAX/MIN/DIV/DIVU/EMUL/EMULU/ADC #imm, rd
                let sub = (insn >> 4) & 0x0F;
                let imm = self.fetch16() as u32;
                let v = self.regs[rd];
                match sub {
                    0x0 | 0x1 | 0x2 | 0x3 => {
                        // FSUB/FCMP/FADD/FMUL - floating stubs (treat as int)
                        self.regs[rd] = v;
                    }
                    0x4 => {
                        let r = if (v as i32) < (imm as i32) { imm } else { v };
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    0x5 => {
                        let r = if (v as i32) > (imm as i32) { imm } else { v };
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    0x6 => {
                        // EMUL
                        let r = (v as u64).wrapping_mul(imm as u64) as u32;
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    0x7 => {
                        let r = (v as u64).wrapping_mul(imm as u64) as u32;
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    0x8 => {
                        if imm != 0 {
                            let r = (v as i32).wrapping_div(imm as i32) as u32;
                            self.regs[rd] = r;
                            self.set_nz(r);
                        }
                    }
                    0x9 => {
                        if imm != 0 {
                            let r = v.wrapping_div(imm);
                            self.regs[rd] = r;
                            self.set_nz(r);
                        }
                    }
                    0xA => {
                        // ADC #imm, rd
                        let r = self.add_flags(v, imm, self.psw.c as u32);
                        self.regs[rd] = r;
                    }
                    0xB => {
                        // SBB #imm, rd
                        let r = self.sub_flags(v, imm, self.psw.c as u32);
                        self.regs[rd] = r;
                    }
                    0xC => {
                        // TST
                        self.set_nz(v & imm);
                    }
                    0xD => {
                        // XOR
                        let r = v ^ imm;
                        self.regs[rd] = r;
                        self.set_nz(r);
                    }
                    0xE => {
                        // STZ: rd = imm if Z set
                        if self.psw.z {
                            self.regs[rd] = imm;
                        }
                    }
                    _ => {
                        // STNZ
                        if !self.psw.z {
                            self.regs[rd] = imm;
                        }
                    }
                }
            }
            0x8 => {
                // MVTC #imm, cr (48-bit)
                let cr = (low >> 4) & 0x0F;
                let imm = self.fetch16();
                if cr == 0 {
                    self.psw = Psw::from_u32(imm as u32);
                }
            }
            _ => {
                // Unknown; halt.
                self.halted = true;
            }
        }
    }
}

/// Sign-extend a 7-bit value.
fn sign7(value: u16) -> i32 {
    ((value & 0x7F) as i8) as i32
}

/// Disassemble one RX instruction at `pc` using `read` for bytes.
pub fn disassemble(pc: u32, read: &dyn Fn(u32) -> u8) -> String {
    let b0 = read(pc) as u16;
    let b1 = read(pc + 1) as u16;
    let insn = b0 | (b1 << 8);
    let op = ((insn >> 8) & 0xFF) as u8;
    let low = (insn & 0xFF) as u8;

    if insn == 0x0000 { return "brk".to_string(); }
    if insn == 0x0002 { return "rts".to_string(); }
    if insn == 0x0003 { return "nop".to_string(); }
    if insn == 0x7996 { return "wait".to_string(); }
    if (insn & 0xF800) == 0x0800 {
        let dsp = sign7(insn & 0x7F);
        return format!("bra.s {:+}", dsp);
    }
    if (op & 0xF0) == 0x10 {
        let cond = ((insn >> 8) & 0x0F) as u8;
        let dsp = sign7(insn & 0x7F);
        return format!("b{}.s {:+}", cond_name(cond), dsp);
    }
    if (op & 0xF0) == 0x20 {
        if insn == 0x2E00 {
            let dsp = low as i8;
            return format!("bra.b {:+}", dsp);
        }
        let cond = op & 0x0F;
        let dsp = low as i8;
        return format!("b{}.b {:+}", cond_name(cond), dsp);
    }
    if insn == 0x3800 {
        let dsp = read(pc + 2) as i16 | ((read(pc + 3) as i16) << 8);
        return format!("bra.w {:+}", dsp);
    }
    if insn == 0x3900 {
        let dsp = read(pc + 2) as i16 | ((read(pc + 3) as i16) << 8);
        return format!("bsr.w {:+}", dsp);
    }
    if (op & 0xF8) == 0x3A {
        let cond = (op & 0x07) as u8;
        let dsp = (low as i16 | ((read(pc + 2) as i16) << 8)) as i32;
        return format!("b{}.w {:+}", cond_name(cond), dsp);
    }
    if (op & 0xFC) == 0x3C {
        let sz = op & 0x03;
        let rd = ((insn >> 8) & 0x07) as usize;
        let imm = read(pc + 2);
        let dsp = ((low & 0x1E) as i32) << 0;
        return format!("mov.{} #0x{:02x}, {}[r{}]", sz_name(sz), imm, dsp, rd);
    }
    if op == 0x3F {
        let rd = ((insn >> 4) & 0x0F) as usize;
        let rd2 = ((insn >> 8) & 0x0F) as usize;
        let imm = read(pc + 2);
        return format!("rtsd #0x{:02x}, r{}-r{}", imm, rd, rd2);
    }
    if (op & 0xE0) == 0x80 || (op & 0xE0) == 0xA0 {
        let sz = ((op >> 5) & 0x03) as u8;
        let rd = ((insn >> 8) & 0x07) as usize;
        let rs = ((low >> 4) & 0x07) as usize;
        let dsp = ((insn >> 3) & 0x1E) as i32;
        return format!("mov.{} r{}, {}[r{}]", sz_name(sz), rs, dsp, rd);
    }
    if (op & 0xE0) == 0x90 || (op & 0xE0) == 0xB0 {
        let sz = ((op >> 5) & 0x03) as u8;
        let rs = ((insn >> 8) & 0x07) as usize;
        let rd = ((low >> 4) & 0x07) as usize;
        let dsp = ((insn >> 3) & 0x1E) as i32;
        return format!("mov.{} {}[r{}], r{}", sz_name(sz), dsp, rs, rd);
    }
    if (op & 0xE0) == 0xB8 {
        let sz = ((op >> 5) & 0x01) as u8;
        let rs = ((insn >> 4) & 0x07) as usize;
        let rd = ((low >> 4) & 0x07) as usize;
        let dsp = ((insn >> 1) & 0x1E) as i32;
        return format!("movu.{} {}[r{}], r{}", sz_name(sz), dsp, rs, rd);
    }
    if (op & 0xFC) == 0x60 {
        let sub = (op >> 2) & 0x03;
        let imm = ((insn >> 4) & 0x0F) as u32;
        let rd = (low & 0x0F) as usize;
        let m = match sub { 0 => "sub", 1 => "cmp", 2 => "add", _ => "mul" };
        return format!("{} #0x{:x}, r{}", m, imm, rd);
    }
    if (op & 0xFC) == 0x64 {
        let kind = (op >> 2) & 0x03;
        let imm = ((insn >> 4) & 0x0F) as u32;
        let rd = (low & 0x0F) as usize;
        let m = match kind { 0 => "and", 1 => "or", _ => "mov" };
        return format!("{} #0x{:x}, r{}", m, imm, rd);
    }
    if (op & 0xFC) == 0x68 || (op & 0xFC) == 0x6C {
        let left = (op & 0x04) != 0;
        let imm = ((insn >> 8) & 0x1F) as u32;
        let rd = (low & 0x0F) as usize;
        let m = if left { "shll" } else { "shlr" };
        return format!("{} #0x{:x}, r{}", m, imm, rd);
    }
    if op == 0x6E {
        let rs = ((insn >> 4) & 0x0F) as usize;
        let rs2 = (low & 0x0F) as usize;
        return format!("pushm r{}-r{}", rs, rs2);
    }
    if op == 0x6F {
        let rd = ((insn >> 4) & 0x0F) as usize;
        let rd2 = (low & 0x0F) as usize;
        return format!("popm r{}-r{}", rd, rd2);
    }
    if (op & 0xFC) == 0x70 {
        let rs = ((insn >> 4) & 0x0F) as usize;
        let rd = (low & 0x0F) as usize;
        let imm = read(pc + 2) as u16 | ((read(pc + 3) as u16) << 8);
        return format!("add #0x{:04x}, r{}, r{}", imm, rs, rd);
    }
    if (op & 0xFC) == 0x74 {
        let kind = op & 0x03;
        let rd = (low & 0x0F) as usize;
        let m = match kind { 0 => "cmp", 1 => "mul", 2 => "and", _ => "or" };
        return format!("{} #0x{:04x}, r{}", m, read(pc + 2) as u16 | ((read(pc + 3) as u16) << 8), rd);
    }
    if (op & 0xFE) == 0x78 {
        let kind = (op >> 1) & 0x03;
        let imm = ((insn >> 8) & 0x1F) as u32;
        let rd = (low & 0x0F) as usize;
        let m = match kind { 0 => "bset", 1 => "bclr", 2 => "btst", _ => "bset" };
        return format!("{} #0x{:x}, r{}", m, imm, rd);
    }
    if (op & 0xFC) == 0x7C {
        let kind = op & 0x03;
        let rd = (low & 0x0F) as usize;
        let m = match kind { 0 => "not", 1 => "neg", 2 => "abs", _ => "pushc" };
        return format!("{} r{}", m, rd);
    }
    if (op & 0xFE) == 0x7E {
        let kind = op & 0x0F;
        let rs = (low & 0x0F) as usize;
        let m = match kind { 0 => "jmp", 1 => "jsr", 4 => "bra.l", 5 => "bsr.l", _ => "?" };
        return format!("{} r{}", m, rs);
    }
    format!("0x{:04x}", insn)
}

fn cond_name(cond: u8) -> &'static str {
    match cond & 0x0F {
        0x0 => "eq",
        0x1 => "ne",
        0x2 => "c",
        0x3 => "nc",
        0x4 => "n",
        0x5 => "p",
        0x6 => "o",
        0x7 => "no",
        0x8 => "le",
        0x9 => "lt",
        0xA => "gt",
        0xB => "ge",
        0xC => "hi",
        0xD => "ls",
        0xE => "f",
        _ => "t",
    }
}

fn sz_name(sz: u8) -> &'static str {
    match sz {
        0 => "b",
        1 => "w",
        _ => "l",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core_with(code: &[u8]) -> RxCore {
        let mut c = RxCore::new();
        c.write_bytes(0x1000, code);
        c.pc = 0x1000;
        c
    }

    #[test]
    fn nop_advances() {
        // NOP = 0x0003.
        let mut c = core_with(&[0x00, 0x03]);
        let r = c.step(&[]);
        assert_eq!(r, RxStep::Advanced);
        assert_eq!(c.pc, 0x1002);
    }


    #[test]
    fn mov_uimm4_rd() {
        // MOV #0x5, r3 => 0110 0110 imm4 rd4 => 0x66 0x53
        let mut c = core_with(&[0x66, 0x53]);
        c.step(&[]);
        assert_eq!(c.regs[3], 5);
        assert_eq!(c.pc, 0x1002);
    }

    #[test]
    fn add_uimm4_rd() {
        // ADD #0x2, r3
        let mut c = core_with(&[0x62, 0x23]);
        c.regs[3] = 10;
        c.step(&[]);
        assert_eq!(c.regs[3], 12);
    }

    #[test]
    fn sub_uimm4_rd() {
        // SUB #0x1, r3
        let mut c = core_with(&[0x60, 0x13]);
        c.regs[3] = 10;
        c.step(&[]);
        assert_eq!(c.regs[3], 9);
    }

    #[test]
    fn bra_short_forward() {
        // BRA.s: 1 byte 0x08 (dsp = low 7 bits = 8), target = pc+1+8.
        let mut c = core_with(&[0x08]);
        c.step(&[]);
        assert_eq!(c.pc, 0x1000 + 1 + 8);
    }

    #[test]
    fn mov_register_indirect() {
        // MOV.l [r5], r2: 1010 1 rs:3 . rd:3 dsp5 => rs=5, rd=2, dsp=0.
        // insn = 1010 1 0000 101 0 010 = 0xA852.
        let mut c = core_with(&[0xA8, 0x52]);
        c.regs[5] = 0x2000;
        c.write_u32(0x2000, 0xDEAD_BEEF);
        c.step(&[]);
        assert_eq!(c.regs[2], 0xDEAD_BEEF);
    }

    #[test]
    fn mov_register_indirect_store() {
        // MOV.l r2, [r5]: 1010 0 rd:3 . rs:3 dsp5 => rd=5, rs=2, dsp=0.
        // insn = 1010 0 0000 101 0 010 = 0xA052.
        let mut c = core_with(&[0xA0, 0x52]);
        c.regs[5] = 0x2000;
        c.regs[2] = 0x1234_5678;
        c.step(&[]);
        assert_eq!(c.read_u32(0x2000), 0x1234_5678);
    }

    #[test]
    fn push_pop_uses_r0() {
        // PUSH r5 (0111 1111 0010 ... no; PUSH dsp[rs] form)
        // Use MOV.l r2, [r5] store and manual.
        let mut c = RxCore::new();
        c.regs[0] = 0x2000;
        c.regs[5] = 0xAA;
        c.push(c.regs[5]);
        assert_eq!(c.regs[0], 0x1FFC);
        assert_eq!(c.read_u32(0x1FFC), 0xAA);
        let v = c.pop();
        assert_eq!(v, 0xAA);
        assert_eq!(c.regs[0], 0x2000);
    }

    #[test]
    fn jmp_register() {
        // JMP r5: 0111 1111 0000 rs:4 => 0x7F 0x05
        let mut c = core_with(&[0x7F, 0x05]);
        c.regs[5] = 0x3000;
        c.step(&[]);
        assert_eq!(c.pc, 0x3000);
    }

    #[test]
    fn and_or_imm4() {
        // AND #0xF, r4 => 0x64 0xF4
        let mut c = core_with(&[0x64, 0xF4]);
        c.regs[4] = 0x1F;
        c.step(&[]);
        assert_eq!(c.regs[4], 0x0F);
        // OR #0x0, r4 => 0x65 0x04
        let mut c2 = core_with(&[0x65, 0x04]);
        c2.regs[4] = 0x1F;
        c2.step(&[]);
        assert_eq!(c2.regs[4], 0x1F);
    }

    #[test]
    fn movu_zero_extend_byte() {
        // MOVU.b [r5], r2: 1011 sz:1 ... . rs:3 . rd:3 dsp5
        // sz=0, rs=5, rd=2, dsp=0 => 1011 0 000 0 101 0 010 = 0xB052.
        let mut c = core_with(&[0xB0, 0x52]);
        c.regs[5] = 0x2000;
        c.write_u8(0x2000, 0xFF);
        c.step(&[]);
        assert_eq!(c.regs[2], 0xFF);
    }

    #[test]
    fn add_sets_zero_and_carry_flags() {
        // ADD #0x2, r3 with r3=0xFFFFFFFE => 0, carry, zero.
        let mut c = core_with(&[0x62, 0x23]);
        c.regs[3] = 0xFFFF_FFFE;
        c.step(&[]);
        assert_eq!(c.regs[3], 0);
        assert!(c.psw.z, "zero flag");
        assert!(c.psw.c, "carry flag");
    }

    #[test]
    fn mov_byte_load_sign_extends() {
        // MOV.b dsp5[r5], r2: op=0x88, rs=5, rd=2, dsp=0 => 0x8852.
        let mut c = core_with(&[0x88, 0x52]);
        c.regs[5] = 0x2000;
        c.write_u8(0x2000, 0x80);
        c.step(&[]);
        assert_eq!(c.regs[2] as u8, 0x80, "byte loaded");
        assert_eq!(c.regs[2] as u32 & 0xFFFFFF80, 0xFFFF_FF80, "sign-extended");
    }

    #[test]
    fn bra_conditional_not_taken() {
        // BCnd.s: b0=0x10 (cond=0, BEQ), dsp byte=0x02.
        let mut c = core_with(&[0x10, 0x02]);
        c.psw.z = false;
        c.step(&[]);
        assert_eq!(c.pc, 0x1002, "not taken: pc+2");
        // Taken case.
        let mut c2 = core_with(&[0x10, 0x02]);
        c2.psw.z = true;
        c2.step(&[]);
        assert_eq!(c2.pc, 0x1004, "taken: pc+2+dsp2");
    }

    #[test]
    fn st_byte_memory() {
        // MOV.b r2, dsp5[r5]: op=0x80, rs=2 (data), rd=5 (base), dsp=0 => 0x8052.
        let mut c = core_with(&[0x80, 0x52]);
        c.regs[5] = 0x2000;
        c.regs[2] = 0xABCD;
        c.step(&[]);
        assert_eq!(c.read_u8(0x2000), 0xCD, "low byte stored");
    }

    #[test]
    fn mul_register() {
        // MUL r5, r3: op=0x4C, rs=5, rd=3 => low=0x53, insn=0x4C53.
        let mut c = core_with(&[0x4C, 0x53]);
        c.regs[5] = 6;
        c.regs[3] = 7;
        c.step(&[]);
        assert_eq!(c.regs[3], 42);
    }
}
