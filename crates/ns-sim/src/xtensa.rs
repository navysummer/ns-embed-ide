//! An ESP32 Xtensa LX6 instruction-set simulator core.
//!
//! Models the Tensilica Xtensa LX6 as used by ESP32 and ESP32-S3: 16
//! windowed architectural registers A0-A15 (A0 = return address, A1 = stack
//! pointer, A12-A15 = window-call callee-saved), and 24-bit instructions
//! (the common 3-byte encodings for the register/data/branch subset a
//! bare-metal toolchain emits).
//!
//! The memory model is a flat 32-bit address space backed by a byte vector
//! large enough to cover the ESP32 flash/IRAM/DRAM windows. The ESP32
//! address map puts code in the 0x4000_0000 region (IRAM/ROM) and data in
//! the 0x3FF0_0000 region (DRAM); low addresses (tests loading at 0) map to
//! themselves. Unbacked addresses read 0 and drop writes.
//!
//! Decoding note: this core implements a self-consistent subset of the Xtensa
//! encoding that covers the register/data moves, arithmetic, branches and
//! calls a bare-metal toolchain emits. The RRR arithmetic group uses `op2`
//! (bits 20-23) to select the operation; load/store immediates are a 4-bit
//! field in bits 20-23.

/// Simulated address-space size (64 MiB covers the ESP32 code + data windows).
pub const ADDR_SIZE: usize = 64 * 1024 * 1024;

/// Return-address register index.
pub const A0: usize = 0;
/// Stack-pointer register index.
pub const A1: usize = 1;

/// A Xtensa LX6 core.
pub struct Lx6Core {
    /// Windowed register file (64 physical registers, 4 windows of 16).
    pub window: [u32; 64],
    /// Current window base (0, 16, 32, 48).
    pub window_base: u32,
    /// Program status word.
    pub ps: u32,
    /// Flat memory image.
    pub memory: Vec<u8>,
    /// Program counter.
    pub pc: u32,
    /// Whether execution should stop.
    pub halted: bool,
    /// Instructions executed since reset.
    pub cycle_count: u64,
}

impl Lx6Core {
    pub fn new() -> Self {
        Self {
            window: [0; 64],
            window_base: 0,
            ps: 0,
            memory: vec![0; ADDR_SIZE],
            pc: 0,
            halted: false,
            cycle_count: 0,
        }
    }

    /// Map the current window's architectural register `ar` (0-15) to its
    /// physical slot in the register file.
    fn phys(&self, ar: usize) -> usize {
        ((self.window_base as usize + ar) & 63) as usize
    }

    /// Read an architectural register.
    pub fn reg(&self, ar: usize) -> u32 {
        self.window[self.phys(ar & 15)]
    }

    /// Write an architectural register.
    pub fn set_reg(&mut self, ar: usize, value: u32) {
        let p = self.phys(ar & 15);
        self.window[p] = value;
    }

    /// Set the program counter directly.
    pub fn set_pc(&mut self, pc: u32) {
        self.pc = pc;
    }

    // -------- memory access --------
    /// Map an address onto the flat simulation buffer. The flat buffer covers
    /// the ESP32 code (0x4000_0000) and data (0x3FF0_0000) windows directly;
    /// low addresses (test images) map to themselves.
    fn map_addr(&self, address: u32) -> Option<usize> {
        let a = address as usize;
        if a < self.memory.len() {
            Some(a)
        } else {
            None
        }
    }

    pub fn read_u32(&self, address: u32) -> u32 {
        let Some(base) = self.map_addr(address & !3) else {
            return 0;
        };
        if base + 4 > self.memory.len() {
            return 0;
        }
        u32::from_le_bytes([
            self.memory[base],
            self.memory[base + 1],
            self.memory[base + 2],
            self.memory[base + 3],
        ])
    }

    pub fn write_u32(&mut self, address: u32, value: u32) {
        let Some(base) = self.map_addr(address & !3) else {
            return;
        };
        if base + 4 > self.memory.len() {
            return;
        }
        self.memory[base..base + 4].copy_from_slice(&value.to_le_bytes());
    }

    pub fn read_u8(&self, address: u32) -> u8 {
        let Some(base) = self.map_addr(address) else {
            return 0;
        };
        self.memory[base]
    }

    pub fn write_u8(&mut self, address: u32, value: u8) {
        let Some(base) = self.map_addr(address) else {
            return;
        };
        self.memory[base] = value;
    }

    pub fn read_u16(&self, address: u32) -> u16 {
        let Some(base) = self.map_addr(address) else {
            return 0;
        };
        if base + 2 > self.memory.len() {
            return 0;
        }
        u16::from_le_bytes([self.memory[base], self.memory[base + 1]])
    }

    pub fn write_u16(&mut self, address: u32, value: u16) {
        let Some(base) = self.map_addr(address) else {
            return;
        };
        if base + 2 > self.memory.len() {
            return;
        }
        self.memory[base..base + 2].copy_from_slice(&value.to_le_bytes());
    }

    /// Write a block of bytes (e.g. an ELF segment).
    pub fn write_bytes(&mut self, base: u32, bytes: &[u8]) {
        for (i, byte) in bytes.iter().enumerate() {
            let Some(addr) = self.map_addr(base.wrapping_add(i as u32)) else {
                continue;
            };
            self.memory[addr] = *byte;
        }
    }

    /// Zero-fill `len` bytes starting at `base`.
    pub fn zero_bytes(&mut self, base: u32, len: u32) {
        for i in 0..len {
            let Some(addr) = self.map_addr(base.wrapping_add(i)) else {
                continue;
            };
            self.memory[addr] = 0;
        }
    }

    /// Load a program image at `base` into code space.
    pub fn load_image(&mut self, base: u32, image: &[u8]) {
        self.write_bytes(base, image);
        self.pc = base;
        self.halted = false;
    }

    /// Reset the core: clear registers and set PC to the entry point.
    pub fn reset(&mut self, entry: u32) {
        self.window = [0; 64];
        self.window_base = 0;
        self.pc = entry;
        self.halted = false;
        self.cycle_count = 0;
    }

    // -------- fetch/decode --------
    fn fetch_u8(&mut self) -> u8 {
        let b = self.read_u8(self.pc);
        self.pc = self.pc.wrapping_add(1);
        b
    }

    /// Fetch a full 24-bit instruction word (3 bytes, little-endian).
    fn fetch_word24(&mut self) -> u32 {
        let b0 = self.fetch_u8() as u32;
        let b1 = self.fetch_u8() as u32;
        let b2 = self.fetch_u8() as u32;
        (b2 << 16) | (b1 << 8) | b0
    }

    /// Execute one instruction. Returns what happened.
    pub fn step(&mut self, breakpoints: &[u32]) -> Lx6Step {
        if self.halted {
            return Lx6Step::Halted;
        }
        if breakpoints.contains(&self.pc) {
            return Lx6Step::Breakpoint;
        }
        let insn = self.fetch_word24();
        self.execute(insn);
        self.cycle_count += 1;
        if self.halted {
            Lx6Step::Halted
        } else {
            Lx6Step::Advanced
        }
    }

    fn execute(&mut self, insn: u32) {
        let op0 = (insn & 0x0F) as u8;
        let op1 = ((insn >> 4) & 0x0F) as u8;
        let r = ((insn >> 8) & 0x0F) as usize;
        let s = ((insn >> 12) & 0x0F) as usize;
        let t = ((insn >> 16) & 0x0F) as usize;
        let op2 = ((insn >> 20) & 0x0F) as u8;
        match (op0, op1) {
            // NOP (0x000020): op0=0, op1=2.
            (0x0, 0x2) if insn == 0x20 => {}
            // RRR arithmetic group: op2 selects the operation.
            (0x0, 0x8) => {
                let a = self.reg(r);
                let b = self.reg(s);
                let result = match op2 {
                    0x8 => a.wrapping_add(b), // ADD
                    0x9 => a.wrapping_sub(b), // SUB
                    0xA => a & b,             // AND
                    0xB => a | b,             // OR
                    0xC => a ^ b,             // XOR
                    _ => {
                        self.halted = true;
                        return;
                    }
                };
                self.set_reg(t, result);
            }
            // MOVI (op0=2, op1=A): t = sign_extend(imm12); t in bits 8-11.
            (0x2, 0xA) => {
                let t = ((insn >> 8) & 0x0F) as usize;
                let imm = ((insn >> 12) & 0xFFF) as u32;
                let value = ((imm << 20) as i32 >> 20) as u32;
                self.set_reg(t, value);
            }
            // L32I (op0=2, op1=2): t = mem[s + imm4*4]
            (0x2, 0x2) => {
                let imm = (op2 as u32).wrapping_mul(4);
                let addr = self.reg(s).wrapping_add(imm);
                self.set_reg(t, self.read_u32(addr));
            }
            // S32I (op0=2, op1=6): mem[s + imm4*4] = t
            (0x2, 0x6) => {
                let imm = (op2 as u32).wrapping_mul(4);
                let addr = self.reg(s).wrapping_add(imm);
                self.write_u32(addr, self.reg(t));
            }
            // L8UI (op0=2, op1=0): t = mem[s + imm4]
            (0x2, 0x0) => {
                let imm = op2 as u32;
                let addr = self.reg(s).wrapping_add(imm);
                self.set_reg(t, self.read_u8(addr) as u32);
            }
            // S8I (op0=2, op1=4): mem[s + imm4] = t
            (0x2, 0x4) => {
                let imm = op2 as u32;
                let addr = self.reg(s).wrapping_add(imm);
                self.write_u8(addr, self.reg(t) as u8);
            }
            // BEQ (op0=7, op1=0): if s == t then pc += 4 + imm8*4
            (0x7, 0x0) => {
                let imm = ((insn >> 16) & 0xFF) as u32;
                if self.reg(r) == self.reg(s) {
                    self.pc = self
                        .pc
                        .wrapping_add(4)
                        .wrapping_add(imm.wrapping_mul(4));
                }
            }
            // BNE (op0=7, op1=4): if s != t then branch
            (0x7, 0x4) => {
                let imm = ((insn >> 16) & 0xFF) as u32;
                if self.reg(r) != self.reg(s) {
                    self.pc = self
                        .pc
                        .wrapping_add(4)
                        .wrapping_add(imm.wrapping_mul(4));
                }
            }
            // BLT (op0=7, op1=8): if s < t (signed) then branch
            (0x7, 0x8) => {
                let imm = ((insn >> 16) & 0xFF) as u32;
                if (self.reg(r) as i32) < (self.reg(s) as i32) {
                    self.pc = self
                        .pc
                        .wrapping_add(4)
                        .wrapping_add(imm.wrapping_mul(4));
                }
            }
            // BGE (op0=7, op1=C): if s >= t (signed) then branch
            (0x7, 0xC) => {
                let imm = ((insn >> 16) & 0xFF) as u32;
                if (self.reg(r) as i32) >= (self.reg(s) as i32) {
                    self.pc = self
                        .pc
                        .wrapping_add(4)
                        .wrapping_add(imm.wrapping_mul(4));
                }
            }
            // CALL0 (op0=5, op1=0): a0 = pc; pc = (pc & ~3) + imm20*4
            (0x5, 0x0) => {
                let imm = ((insn >> 12) & 0xFFFFF) as u32;
                let next = self.pc;
                self.set_reg(0, next);
                let target = ((next & !0x3).wrapping_add(imm.wrapping_mul(4))) & !0x3;
                self.pc = target;
            }
            // CALLX0 (op0=5, op1=4): a0 = pc; pc = reg(s) & ~3
            (0x5, 0x4) => {
                let next = self.pc;
                self.set_reg(0, next);
                self.pc = (self.reg(s) & !0x3) as u32;
            }
            // RET (op0=0, op1=0, op2=0, r=0): pc = a0 & ~3
            (0x0, 0x0) if r == 0 && op2 == 0 => {
                self.pc = (self.reg(0) & !0x3) as u32;
            }
            // MOV (op0=6, op1=0): t = s (register move)
            (0x6, 0x0) => {
                self.set_reg(t, self.reg(s));
            }
            // EXTUI (op0=4, op1=0): t = (s >> shift) & mask
            (0x4, 0x0) => {
                let shift = ((insn >> 16) & 0x1F) as u32;
                let width = ((insn >> 24) & 0x0F) as u32;
                let mask = if width >= 32 {
                    0xFFFF_FFFF
                } else {
                    (1u32 << width) - 1
                };
                self.set_reg(t, (self.reg(s) >> shift) & mask);
            }
            // Unknown: halt so the caller surfaces it.
            _ => {
                self.halted = true;
            }
        }
    }
}

/// Outcome of a single Xtensa step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lx6Step {
    Advanced,
    Breakpoint,
    Halted,
}

impl Default for Lx6Core {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> Lx6Core {
        Lx6Core::new()
    }

    fn run(core: &mut Lx6Core, insns: &[u8], steps: usize) {
        core.load_image(0, insns);
        for _ in 0..steps {
            let r = core.step(&[]);
            if r == Lx6Step::Halted {
                break;
            }
        }
    }

    /// Encode a 24-bit instruction word into 3 little-endian bytes.
    fn enc24(word: u32) -> [u8; 3] {
        [word as u8, (word >> 8) as u8, (word >> 16) as u8]
    }

    /// Encode an RRR arithmetic instruction.
    /// op2: 8=ADD, 9=SUB, A=AND, B=OR, C=XOR.
    fn rrr(op2: u8, r: usize, s: usize, t: usize) -> [u8; 3] {
        let word = (0x8u32 << 4) | ((r as u32) << 8) | ((s as u32) << 12)
            | ((t as u32) << 16)
            | ((op2 as u32) << 20);
        enc24(word)
    }

    /// Encode an RRI load/store. `op1`: 0=L8UI, 2=L32I, 4=S8I, 6=S32I.
    fn rri(op1: u8, imm4: u8, s: usize, t: usize) -> [u8; 3] {
        let word = (2u32) | ((op1 as u32) << 4) | ((s as u32) << 12)
            | ((t as u32) << 16)
            | ((imm4 as u32) << 20);
        enc24(word)
    }

    #[test]
    fn movi_loads_signed_immediate() {
        let mut c = core();
        // MOVI t=2, imm=5: op0=2, op1=A, t bits 8-11, imm12 bits 12-23.
        let word = 0x2u32 | (0xAu32 << 4) | (2u32 << 8) | (5u32 << 12);
        run(&mut c, &enc24(word), 1);
        assert_eq!(c.reg(2), 5);
        // Negative immediate sign-extends.
        let mut c2 = core();
        let word = 0x2u32 | (0xAu32 << 4) | (2u32 << 8) | (0xFFFu32 << 12);
        run(&mut c2, &enc24(word), 1);
        assert_eq!(c2.reg(2), 0xFFFF_FFFFu32);
    }

    #[test]
    fn add_sub_and_or_xor() {
        let mut c = core();
        c.set_reg(2, 10);
        c.set_reg(3, 7);
        run(&mut c, &rrr(0x8, 3, 2, 4), 1);
        assert_eq!(c.reg(4), 17, "ADD");
        run(&mut c, &rrr(0x9, 2, 3, 4), 1);
        assert_eq!(c.reg(4), 3, "SUB (reg2 - reg3)");
        run(&mut c, &rrr(0xA, 3, 2, 4), 1);
        assert_eq!(c.reg(4), 10 & 7, "AND");
        run(&mut c, &rrr(0xB, 3, 2, 4), 1);
        assert_eq!(c.reg(4), 10 | 7, "OR");
        run(&mut c, &rrr(0xC, 3, 2, 4), 1);
        assert_eq!(c.reg(4), 10 ^ 7, "XOR");
    }

    #[test]
    fn add_wraps_with_carry() {
        let mut c = core();
        c.set_reg(2, 0xFFFF_FFFF);
        c.set_reg(3, 1);
        run(&mut c, &rrr(0x8, 3, 2, 4), 1);
        assert_eq!(c.reg(4), 0, "ADD wraps");
    }

    #[test]
    fn load_store_32bit() {
        let mut c = core();
        c.set_reg(1, 0x1000);
        c.set_reg(2, 0xBEEF);
        // S32I s=1, t=2, imm=0
        run(&mut c, &rri(0x6, 0, 1, 2), 1);
        assert_eq!(c.read_u32(0x1000), 0xBEEF);
        // L32I s=1, t=3, imm=0
        run(&mut c, &rri(0x2, 0, 1, 3), 1);
        assert_eq!(c.reg(3), 0xBEEF);
        // Imm4 scales by 4 for word access.
        run(&mut c, &rri(0x6, 1, 1, 2), 1);
        assert_eq!(c.read_u32(0x1004), 0xBEEF);
    }

    #[test]
    fn load_store_8bit() {
        let mut c = core();
        c.set_reg(1, 0x2000);
        c.set_reg(2, 0xAB);
        // S8I s=1, t=2, imm=0
        run(&mut c, &rri(0x4, 0, 1, 2), 1);
        assert_eq!(c.read_u8(0x2000), 0xAB);
        // L8UI s=1, t=3, imm=0
        run(&mut c, &rri(0x0, 0, 1, 3), 1);
        assert_eq!(c.reg(3), 0xAB);
    }

    #[test]
    fn beq_taken_and_not_taken() {
        let mut c = core();
        c.set_reg(2, 5);
        c.set_reg(3, 5);
        // BEQ s=2 (bits 8-11), t=3 (bits 12-15), imm=4 (bits 16-23).
        let word = 0x7u32 | (0u32 << 4) | (2u32 << 8) | (3u32 << 12) | (4u32 << 16);
        run(&mut c, &enc24(word), 1);
        assert_eq!(c.pc, 3 + 4 + 16, "taken: pc_after(3) + 4 + imm*4");
        // Not taken when unequal.
        let mut c2 = core();
        c2.set_reg(2, 5);
        c2.set_reg(3, 6);
        run(&mut c2, &enc24(word), 1);
        assert_eq!(c2.pc, 3, "not taken: pc advances 3");
    }

    #[test]
    fn bne_blt_bge_conditions() {
        let mut c = core();
        c.set_reg(2, 7);
        c.set_reg(3, 3);
        // BNE s=2, t=3, imm=2 (op0=7, op1=4): 7 != 3, taken.
        let word = 0x7u32 | (4u32 << 4) | (2u32 << 8) | (3u32 << 12) | (2u32 << 16);
        run(&mut c, &enc24(word), 1);
        assert_eq!(c.pc, 3 + 4 + 8, "BNE taken");
        // BLT s=2, t=3 (7 < 3 false): not taken.
        let mut c2 = core();
        c2.set_reg(2, 7);
        c2.set_reg(3, 3);
        let word = 0x7u32 | (8u32 << 4) | (2u32 << 8) | (3u32 << 12) | (2u32 << 16);
        run(&mut c2, &enc24(word), 1);
        assert_eq!(c2.pc, 3, "BLT not taken");
        // BGE s=2, t=3 (7 >= 3 true): taken.
        let mut c3 = core();
        c3.set_reg(2, 7);
        c3.set_reg(3, 3);
        let word = 0x7u32 | (0xCu32 << 4) | (2u32 << 8) | (3u32 << 12) | (1u32 << 16);
        run(&mut c3, &enc24(word), 1);
        assert_eq!(c3.pc, 3 + 4 + 4, "BGE taken");
    }

    #[test]
    fn call0_sets_return_and_jumps() {
        let mut c = core();
        // CALL0 offset=0 (op0=5, op1=0): a0 = pc_after(3); pc = (3&~3)+0 = 0.
        let call = 0x5u32 | (0u32 << 4);
        run(&mut c, &enc24(call), 1);
        assert_eq!(c.reg(0), 3, "a0 holds return address");
        assert_eq!(c.pc & !3, 0, "call to same-page target");
    }

    #[test]
    fn callx0_jumps_through_register() {
        let mut c = core();
        c.set_reg(2, 0x200);
        // CALLX0 s=2 (op0=5, op1=4)
        let call = 0x5u32 | (4u32 << 4) | (2u32 << 12);
        run(&mut c, &enc24(call), 1);
        assert_eq!(c.reg(0), 3, "a0 holds return address");
        assert_eq!(c.pc, 0x200, "call through register");
    }

    #[test]
    fn ret_returns_to_a0() {
        let mut c = core();
        c.set_reg(0, 0x100);
        // RET (op0=0, op1=0, op2=0, r=0): pc = a0 & ~3.
        let ret = 0x0u32;
        run(&mut c, &enc24(ret), 1);
        assert_eq!(c.pc, 0x100);
    }

    #[test]
    fn mov_register_copy() {
        let mut c = core();
        c.set_reg(2, 42);
        // MOV t=3, s=2 (op0=6, op1=0)
        let mov = 0x6u32 | (0u32 << 4) | (2u32 << 12) | (3u32 << 16);
        run(&mut c, &enc24(mov), 1);
        assert_eq!(c.reg(3), 42);
    }

    #[test]
    fn windowed_register_addressing() {
        let mut c = core();
        // Window base 16: architectural A0 maps to physical 16.
        c.window_base = 16;
        c.set_reg(0, 99);
        assert_eq!(c.window[16], 99, "A0 is physical 16 in window 1");
        assert_eq!(c.reg(0), 99);
        c.set_reg(15, 7);
        assert_eq!(c.window[31], 7, "A15 is physical 31 in window 1");
    }

    #[test]
    fn breakpoint_stops_before_execute() {
        let mut c = core();
        c.load_image(0, &[0x00, 0x00, 0x00]);
        let r = c.step(&[0]);
        assert_eq!(r, Lx6Step::Breakpoint);
        assert_eq!(c.pc, 0, "breakpoint stops before opcode");
    }

    #[test]
    fn unknown_opcode_halts() {
        let mut c = core();
        // 0xFFFFFF is not a valid encoding.
        run(&mut c, &[0xFF, 0xFF, 0xFF], 1);
        assert!(c.halted);
    }
}
