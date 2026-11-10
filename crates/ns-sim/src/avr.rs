//! An AVR (Atmel/Microchip, ATmega/ATtiny) instruction-set simulator core.
//!
//! The AVR is an 8-bit RISC architecture: 32 general-purpose 8-bit registers
//! (R0-R31), an 8-bit status register (SREG: C/Z/N/V/S/H/T/I), a 16-bit stack
//! pointer, a Harvard memory model (16-bit program words, 16-bit data space),
//! and 64 I/O registers reachable through `IN`/`OUT`.
//!
//! Data-space layout matches the classic ATmega parts: `0x00-0x1F` map to the
//! register file, `0x20-0x5F` to the I/O space and `0x60+` to SRAM. Because the
//! register file is memory-mapped, `LDS`/`STS`/`LD`/`ST` can address registers,
//! I/O and SRAM alike.
//!
//! Coverage: the instruction subset a bare-metal toolchain (avr-gcc / IAR AVR)
//! emits for control flow, arithmetic/logic, loads/stores, stack, bit
//! operations and I/O. The AVR is little-endian in data space and stores each
//! 16-bit instruction word low byte first in program memory.

/// Program memory size in bytes (64 KiB covers ATmega2560 program *bytes*; the
/// classic 16-bit PC parts fit comfortably).
pub const ROM_SIZE: usize = 64 * 1024;
/// Data SRAM size in bytes (the `0x60+` window).
pub const SRAM_SIZE: usize = 64 * 1024 - 0x60;
/// I/O register space: `IN`/`OUT` addresses `0x00-0x3F`.
pub const IO_SIZE: usize = 64;
/// Register file size.
pub const REG_COUNT: usize = 32;

/// SREG bit masks.
pub const C: u8 = 0x01; // carry
pub const Z: u8 = 0x02; // zero
pub const N: u8 = 0x04; // negative
pub const V: u8 = 0x08; // two's-complement overflow
pub const S: u8 = 0x10; // N ^ V
pub const H: u8 = 0x20; // half carry
pub const T: u8 = 0x40; // bit copy storage
pub const I: u8 = 0x80; // global interrupt enable

/// AVR data-space bound (register + I/O + SRAM).
const DATA_SIZE: usize = 0x60 + SRAM_SIZE;

/// Outcome of a single AVR step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvrStep {
    /// A normal instruction advanced the PC.
    Advanced,
    /// A breakpoint was reached before executing.
    Breakpoint,
    /// The core halted (`SLEEP`, `BREAK`, or an unknown opcode).
    Halted,
}

/// An AVR core.
pub struct AvrCore {
    /// General-purpose registers R0-R31.
    pub regs: [u8; REG_COUNT],
    /// Status register (C/Z/N/V/S/H/T/I).
    pub sreg: u8,
    /// Stack pointer (byte address in the data space).
    pub sp: u16,
    /// Program memory (byte-addressed, little-endian instruction words).
    pub rom: Vec<u8>,
    /// I/O register space (`0x20-0x5F` in the data space).
    pub io: [u8; IO_SIZE],
    /// Data SRAM (data addresses `0x60+`).
    pub sram: Vec<u8>,
    /// Program counter: a **byte** address into program memory.
    pub pc: u32,
    /// Whether execution should stop.
    pub halted: bool,
    /// Instructions executed since reset.
    pub cycle_count: u64,
}

impl AvrCore {
    pub fn new() -> Self {
        Self {
            regs: [0; REG_COUNT],
            sreg: 0,
            // ATmega328P resets SP to RAMEND (0x08FF).
            sp: 0x08FF,
            rom: vec![0; ROM_SIZE],
            io: [0; IO_SIZE],
            sram: vec![0; SRAM_SIZE],
            pc: 0,
            halted: false,
            cycle_count: 0,
        }
    }

    /// Reset the core to `entry` (byte address) without clearing memory.
    pub fn reset(&mut self, entry: u32) {
        self.pc = entry;
        self.regs = [0; REG_COUNT];
        self.sreg = 0;
        self.sp = 0x08FF;
        self.halted = false;
        self.cycle_count = 0;
    }

    /// Load a program image at `base` (a byte address into program memory).
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

    /// Set the program counter (byte address).
    pub fn set_pc(&mut self, pc: u32) {
        self.pc = pc;
    }

    // -------- status register --------
    pub fn flag(&self, mask: u8) -> bool {
        self.sreg & mask != 0
    }
    fn set_flag(&mut self, mask: u8, value: bool) {
        if value {
            self.sreg |= mask;
        } else {
            self.sreg &= !mask;
        }
    }
    pub fn carry(&self) -> bool {
        self.flag(C)
    }
    pub fn zero(&self) -> bool {
        self.flag(Z)
    }
    pub fn negative(&self) -> bool {
        self.flag(N)
    }
    pub fn overflow(&self) -> bool {
        self.flag(V)
    }

    // -------- memory --------
    fn read_prog(&self, addr: u32) -> u8 {
        self.rom.get(addr as usize).copied().unwrap_or(0)
    }

    fn fetch_word(&mut self) -> u16 {
        let lo = self.read_prog(self.pc) as u16;
        let hi = self.read_prog(self.pc.wrapping_add(1)) as u16;
        self.pc = self.pc.wrapping_add(2) & 0xFFFF;
        (hi << 8) | lo
    }

    /// Read a byte from the unified data space (registers, I/O, SRAM).
    pub fn read_data(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x001F => self.regs[addr as usize],
            0x0020..=0x005F => self.io[(addr - 0x20) as usize],
            _ => {
                let index = addr as usize - 0x60;
                self.sram.get(index).copied().unwrap_or(0)
            }
        }
    }

    /// Write a byte into the unified data space.
    pub fn write_data(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x001F => self.regs[addr as usize] = value,
            0x0020..=0x005F => self.io[(addr - 0x20) as usize] = value,
            _ => {
                let index = addr as usize - 0x60;
                if index < self.sram.len() {
                    self.sram[index] = value;
                }
            }
        }
    }

    fn push8(&mut self, value: u8) {
        let addr = self.sp;
        self.write_data(addr, value);
        self.sp = self.sp.wrapping_sub(1);
    }

    fn pop8(&mut self) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        self.read_data(self.sp)
    }

    // -------- ALU helpers --------
    fn finish_logic(&mut self, result: u8) -> u8 {
        self.set_flag(V, false);
        self.set_flag(N, result & 0x80 != 0);
        self.set_flag(Z, result == 0);
        self.set_flag(S, self.flag(N) ^ self.flag(V));
        result
    }

    fn add_with_carry(&mut self, a: u8, b: u8, carry_in: u8) -> u8 {
        let sum = a as u16 + b as u16 + carry_in as u16;
        let result = sum as u8;
        self.set_flag(H, (a & 0x0F) as u16 + (b & 0x0F) as u16 + carry_in as u16 > 0x0F);
        self.set_flag(V, (a ^ result) & (b ^ result) & 0x80 != 0);
        self.set_flag(C, sum > 0xFF);
        self.set_flag(N, result & 0x80 != 0);
        self.set_flag(Z, result == 0);
        self.set_flag(S, self.flag(N) ^ self.flag(V));
        result
    }

    fn sub_with_carry(&mut self, a: u8, b: u8, carry_in: u8) -> u8 {
        let diff = a as i16 - b as i16 - carry_in as i16;
        let result = diff as u8;
        self.set_flag(H, (a & 0x0F) as i16 - (b & 0x0F) as i16 - (carry_in as i16) < 0);
        self.set_flag(V, (a ^ b) & (a ^ result) & 0x80 != 0);
        self.set_flag(C, diff < 0);
        self.set_flag(N, result & 0x80 != 0);
        self.set_flag(Z, result == 0);
        self.set_flag(S, self.flag(N) ^ self.flag(V));
        result
    }

    fn compare(&mut self, a: u8, b: u8, carry_in: u8) {
        let _ = self.sub_with_carry(a, b, carry_in);
    }

    /// Size in bytes of the instruction word at the current PC. AVR's 32-bit
    /// forms are the only 4-byte instructions (LDS/STS/JMP/CALL).
    fn current_instruction_size(&self) -> u32 {
        let lo = self.read_prog(self.pc) as u16;
        let hi = self.read_prog(self.pc.wrapping_add(1)) as u16;
        let word = (hi << 8) | lo;
        match word & 0xFE0F {
            0x9000 | 0x9200 | 0x940C | 0x940E => 4,
            _ => 2,
        }
    }

    fn skip_next(&mut self) {
        let size = self.current_instruction_size();
        self.pc = self.pc.wrapping_add(size) & 0xFFFF;
    }

    /// Execute one instruction.
    pub fn step(&mut self, breakpoints: &[u32]) -> AvrStep {
        if self.halted {
            return AvrStep::Halted;
        }
        for bp in breakpoints {
            if *bp == self.pc {
                return AvrStep::Breakpoint;
            }
        }
        let word = self.fetch_word();
        self.execute(word);
        self.cycle_count += 1;
        if self.halted {
            AvrStep::Halted
        } else {
            AvrStep::Advanced
        }
    }

    fn relative_word_offset(word: u16) -> i32 {
        let k = word & 0x0FFF;
        let k = if k & 0x0800 != 0 { k as i32 - 0x1000 } else { k as i32 };
        k
    }

    fn branch(&mut self, offset_words: i32) {
        // The PC has already advanced past the 16-bit instruction.
        self.pc = (self.pc as i32 + offset_words * 2) as u32 & 0xFFFF;
    }

    fn execute(&mut self, word: u16) {
        match word {
            // NOP
            0x0000 => {}
            // MOVW Rd, Rr: 0000 0001 dddd rrrr (register pairs)
            w if w & 0xFF00 == 0x0100 => {
                let d = ((w >> 4) & 0x0F) as usize * 2;
                let r = (w & 0x0F) as usize * 2;
                if d + 1 < REG_COUNT && r + 1 < REG_COUNT {
                    self.regs[d] = self.regs[r];
                    self.regs[d + 1] = self.regs[r + 1];
                }
            }
            // MULS Rd, Rr (signed, R16-R31): 0000 0010 dddd rrrr
            w if w & 0xFF00 == 0x0200 => {
                let d = 16 + ((w >> 4) & 0x0F) as usize;
                let r = 16 + (w & 0x0F) as usize;
                let product = (self.regs[d] as i8 as i16 * self.regs[r] as i8 as i16) as u16;
                self.regs[0] = product as u8;
                self.regs[1] = (product >> 8) as u8;
                self.set_flag(C, product & 0x8000 != 0);
                self.set_flag(Z, product == 0);
            }
            // Two-operand register ALU group: 00xx xxxx dddd rrrr
            // CPC 0000 01, SBC 0000 10, ADD 0000 11, CPSE 0001 00,
            // CP 0001 01, SUB 0001 10, ADC 0001 11, AND 0010 00,
            // EOR 0010 01, OR 0010 10, MOV 0010 11
            w if w & 0xFC00 == 0x0C00 => {
                let (d, r) = self.reg_pair(w);
                let a = self.regs[d];
                let b = self.regs[r];
                self.regs[d] = self.add_with_carry(a, b, 0);
            }
            w if w & 0xFC00 == 0x1C00 => {
                let (d, r) = self.reg_pair(w);
                let carry = if self.carry() { 1 } else { 0 };
                let (a, b) = (self.regs[d], self.regs[r]);
                self.regs[d] = self.add_with_carry(a, b, carry);
            }
            w if w & 0xFC00 == 0x1800 => {
                let (d, r) = self.reg_pair(w);
                let (a, b) = (self.regs[d], self.regs[r]);
                self.regs[d] = self.sub_with_carry(a, b, 0);
            }
            w if w & 0xFC00 == 0x0800 => {
                let (d, r) = self.reg_pair(w);
                let carry = if self.carry() { 1 } else { 0 };
                let (a, b) = (self.regs[d], self.regs[r]);
                self.regs[d] = self.sub_with_carry(a, b, carry);
            }
            w if w & 0xFC00 == 0x0400 => {
                // CPC Rd, Rr: compare with carry, discard result.
                let (d, r) = self.reg_pair(w);
                let carry = if self.carry() { 1 } else { 0 };
                let (a, b) = (self.regs[d], self.regs[r]);
                self.compare(a, b, carry);
            }
            w if w & 0xFC00 == 0x1400 => {
                // CP Rd, Rr
                let (d, r) = self.reg_pair(w);
                let (a, b) = (self.regs[d], self.regs[r]);
                self.compare(a, b, 0);
            }
            w if w & 0xFC00 == 0x2000 => {
                let (d, r) = self.reg_pair(w);
                let result = self.regs[d] & self.regs[r];
                self.regs[d] = self.finish_logic(result);
            }
            w if w & 0xFC00 == 0x2400 => {
                let (d, r) = self.reg_pair(w);
                let result = self.regs[d] ^ self.regs[r];
                self.regs[d] = self.finish_logic(result);
            }
            w if w & 0xFC00 == 0x2800 => {
                let (d, r) = self.reg_pair(w);
                let result = self.regs[d] | self.regs[r];
                self.regs[d] = self.finish_logic(result);
            }
            w if w & 0xFC00 == 0x2C00 => {
                let (d, r) = self.reg_pair(w);
                self.regs[d] = self.regs[r];
            }
            // CPSE Rd, Rr
            w if w & 0xFC00 == 0x1000 => {
                let (d, r) = self.reg_pair(w);
                if self.regs[d] == self.regs[r] {
                    self.skip_next();
                }
            }
            // MUL Rd, Rr (unsigned): 1001 11rd dddd rrrr
            w if w & 0xFC00 == 0x9C00 => {
                let (d, r) = self.reg_pair(w);
                let product = self.regs[d] as u16 * self.regs[r] as u16;
                self.regs[0] = product as u8;
                self.regs[1] = (product >> 8) as u8;
                self.set_flag(C, product & 0x8000 != 0);
                self.set_flag(Z, product == 0);
            }
            // Immediate ALU group with Rd = R16-R31:
            // CPI 0011, SBCI 0100, SUBI 0101, ORI 0110, ANDI 0111, LDI 1110
            w if w & 0xF000 == 0x3000 => {
                let (d, k) = self.imm_pair(w);
                let (a, b) = (self.regs[d], k);
                self.compare(a, b, 0);
            }
            w if w & 0xF000 == 0x4000 => {
                let (d, k) = self.imm_pair(w);
                let carry = if self.carry() { 1 } else { 0 };
                let (a, b) = (self.regs[d], k);
                self.regs[d] = self.sub_with_carry(a, b, carry);
            }
            w if w & 0xF000 == 0x5000 => {
                let (d, k) = self.imm_pair(w);
                let (a, b) = (self.regs[d], k);
                self.regs[d] = self.sub_with_carry(a, b, 0);
            }
            w if w & 0xF000 == 0x6000 => {
                let (d, k) = self.imm_pair(w);
                let result = self.regs[d] | k;
                self.regs[d] = self.finish_logic(result);
            }
            w if w & 0xF000 == 0x7000 => {
                let (d, k) = self.imm_pair(w);
                let result = self.regs[d] & k;
                self.regs[d] = self.finish_logic(result);
            }
            w if w & 0xF000 == 0xE000 => {
                let (d, k) = self.imm_pair(w);
                self.regs[d] = k;
            }
            // Unconditional relative jumps/calls.
            w if w & 0xF000 == 0xC000 => {
                let offset = Self::relative_word_offset(w);
                self.branch(offset);
            }
            w if w & 0xF000 == 0xD000 => {
                let offset = Self::relative_word_offset(w);
                let return_word = (self.pc >> 1) as u16;
                self.push8(return_word as u8);
                self.push8((return_word >> 8) as u8);
                self.branch(offset);
            }
            // Conditional branches (BRBS 1111 00 / BRBC 1111 01).
            w if w & 0xFC00 == 0xF000 => {
                let bit = (w & 0x07) as u8;
                let offset = ((w >> 3) & 0x7F) as i32;
                let offset = if offset & 0x40 != 0 { offset - 0x80 } else { offset };
                if self.flag(1 << bit) {
                    self.branch(offset);
                }
            }
            w if w & 0xFC00 == 0xF400 => {
                let bit = (w & 0x07) as u8;
                let offset = ((w >> 3) & 0x7F) as i32;
                let offset = if offset & 0x40 != 0 { offset - 0x80 } else { offset };
                if !self.flag(1 << bit) {
                    self.branch(offset);
                }
            }
            // BLD Rd, b / BST Rd, b
            w if w & 0xFE08 == 0xF800 => {
                let d = ((w >> 4) & 0x1F) as usize;
                let bit = (w & 0x07) as u8;
                let value = if self.flag(T) { 1 } else { 0 };
                self.regs[d] = (self.regs[d] & !(1 << bit)) | (value << bit);
            }
            w if w & 0xFE08 == 0xFA00 => {
                let d = ((w >> 4) & 0x1F) as usize;
                let bit = (w & 0x07) as u8;
                self.set_flag(T, self.regs[d] & (1 << bit) != 0);
            }
            // SBRC / SBRS
            w if w & 0xFE08 == 0xFC00 => {
                let d = ((w >> 4) & 0x1F) as usize;
                let bit = (w & 0x07) as u8;
                if self.regs[d] & (1 << bit) == 0 {
                    self.skip_next();
                }
            }
            w if w & 0xFE08 == 0xFE00 => {
                let d = ((w >> 4) & 0x1F) as usize;
                let bit = (w & 0x07) as u8;
                if self.regs[d] & (1 << bit) != 0 {
                    self.skip_next();
                }
            }
            // I/O bit set/clear and skip: SBI/CBI/SBIC/SBIS
            w if w & 0xFF00 == 0x9A00 => self.io_bit(w, IoOp::Set),
            w if w & 0xFF00 == 0x9800 => self.io_bit(w, IoOp::Clear),
            w if w & 0xFF00 == 0x9900 => self.io_bit(w, IoOp::SkipIfClear),
            w if w & 0xFF00 == 0x9B00 => self.io_bit(w, IoOp::SkipIfSet),
            // Single-register group: 1001 010d dddd xxxx
            w if w & 0xFE00 == 0x9400 => self.single_register_op(w),
            // Load/store group: 1001 000/001d dddd xxxx (LDS/STS/LD/ST/POP/
            // PUSH/LPM) plus the 32-bit LDS/STS forms.
            w if w & 0xFC00 == 0x9000 => self.control_op(w),
            // IN / OUT
            w if w & 0xF800 == 0xB000 => {
                let port = self.io_port(w);
                let d = ((w >> 4) & 0x1F) as usize;
                self.regs[d] = self.io[port];
            }
            w if w & 0xF800 == 0xB800 => {
                let port = self.io_port(w);
                let d = ((w >> 4) & 0x1F) as usize;
                self.io[port] = self.regs[d];
            }
            // LDD/STD with Y+q / Z+q displacement: 10q0 qqxd dddd xqqq
            w if w & 0xD000 == 0x8000 => self.displacement_load_store(w),
            _ => {
                self.halted = true;
            }
        }
    }

    fn reg_pair(&self, word: u16) -> (usize, usize) {
        let d = (word & 0x0F) as usize | ((word >> 5) & 0x10) as usize;
        let r = ((word >> 5) & 0x0F) as usize | ((word >> 5) & 0x10) as usize;
        (d, r)
    }

    fn imm_pair(&self, word: u16) -> (usize, u8) {
        let d = 16 + ((word >> 4) & 0x0F) as usize;
        let k = (((word >> 4) & 0xF0) as u8) | (word & 0x0F) as u8;
        (d, k)
    }

    fn io_port(&self, word: u16) -> usize {
        (((word >> 5) & 0x30) | (word & 0x0F)) as usize
    }

    fn io_bit(&mut self, word: u16, op: IoOp) {
        let port = ((word >> 3) & 0x1F) as usize;
        let bit = (word & 0x07) as u8;
        let set = self.io[port] & (1 << bit) != 0;
        match op {
            IoOp::Set => self.io[port] |= 1 << bit,
            IoOp::Clear => self.io[port] &= !(1 << bit),
            IoOp::SkipIfClear => {
                if !set {
                    self.skip_next();
                }
            }
            IoOp::SkipIfSet => {
                if set {
                    self.skip_next();
                }
            }
        }
    }

    fn displacement_load_store(&mut self, word: u16) {
        let d = ((word >> 4) & 0x1F) as usize;
        let y = word & 0x08 != 0;
        let base = if y {
            ((self.regs[29] as u16) << 8) | self.regs[28] as u16
        } else {
            ((self.regs[31] as u16) << 8) | self.regs[30] as u16
        };
        let q = (((word >> 8) & 0x20) | ((word >> 7) & 0x18) | (word & 0x07)) as u16;
        let addr = base.wrapping_add(q);
        if word & 0x0200 != 0 {
            // ST
            self.write_data(addr, self.regs[d]);
        } else {
            // LD
            self.regs[d] = self.read_data(addr);
        }
    }

    fn single_register_op(&mut self, word: u16) {
        let d = ((word >> 4) & 0x1F) as usize;
        match word & 0x000F {
            0x0 => {
                // COM
                let result = !self.regs[d];
                self.regs[d] = result;
                self.set_flag(C, true);
                self.set_flag(V, false);
                self.set_flag(N, result & 0x80 != 0);
                self.set_flag(Z, result == 0);
                self.set_flag(S, self.flag(N) ^ self.flag(V));
            }
            0x1 => {
                // NEG
                let a = self.regs[d];
                let result = 0u8.wrapping_sub(a);
                self.set_flag(C, result != 0);
                self.set_flag(V, result == 0x80);
                self.set_flag(H, (result | a) & 0x08 != 0);
                self.set_flag(N, result & 0x80 != 0);
                self.set_flag(Z, result == 0);
                self.set_flag(S, self.flag(N) ^ self.flag(V));
                self.regs[d] = result;
            }
            0x2 => {
                // SWAP
                let a = self.regs[d];
                self.regs[d] = a.rotate_left(4);
            }
            0x3 => {
                // INC
                let result = self.regs[d].wrapping_add(1);
                self.set_flag(V, result == 0x80);
                self.set_flag(N, result & 0x80 != 0);
                self.set_flag(Z, result == 0);
                self.set_flag(S, self.flag(N) ^ self.flag(V));
                self.regs[d] = result;
            }
            0x5 => {
                // ASR
                let a = self.regs[d];
                let result = (a >> 1) | (a & 0x80);
                let carry = a & 0x01 != 0;
                self.set_flag(C, carry);
                self.set_flag(N, result & 0x80 != 0);
                self.set_flag(V, self.flag(N) ^ carry);
                self.set_flag(Z, result == 0);
                self.set_flag(S, self.flag(N) ^ self.flag(V));
                self.regs[d] = result;
            }
            0x6 => {
                // LSR
                let a = self.regs[d];
                let result = a >> 1;
                let carry = a & 0x01 != 0;
                self.set_flag(C, carry);
                self.set_flag(N, false);
                self.set_flag(V, carry);
                self.set_flag(Z, result == 0);
                self.set_flag(S, self.flag(N) ^ self.flag(V));
                self.regs[d] = result;
            }
            0x7 => {
                // ROR
                let a = self.regs[d];
                let carry_in = if self.carry() { 0x80 } else { 0 };
                let result = (a >> 1) | carry_in;
                let carry = a & 0x01 != 0;
                self.set_flag(C, carry);
                self.set_flag(N, result & 0x80 != 0);
                self.set_flag(V, self.flag(N) ^ carry);
                self.set_flag(Z, result == 0);
                self.set_flag(S, self.flag(N) ^ self.flag(V));
                self.regs[d] = result;
            }
            0xA => {
                // DEC
                let result = self.regs[d].wrapping_sub(1);
                self.set_flag(V, result == 0x7F);
                self.set_flag(N, result & 0x80 != 0);
                self.set_flag(Z, result == 0);
                self.set_flag(S, self.flag(N) ^ self.flag(V));
                self.regs[d] = result;
            }
            0x8 | 0x9 => {
                // Control group (RET/RETI/IJMP/ICALL/SLEEP/...), selected by
                // the full word below.
                self.control_op(word);
            }
            0xF => {
                // PUSH
                let value = self.regs[d];
                self.push8(value);
            }
            0xE => {
                // SPM: program-memory self-write is not modelled; it is a
                // no-op here (bootloaders only).
            }
            _ => {
                self.halted = true;
            }
        }
    }

    fn control_op(&mut self, word: u16) {
        match word {
            0x9508 => {
                // RET
                let hi = self.pop8();
                let lo = self.pop8();
                self.pc = (((hi as u32) << 8) | lo as u32) * 2 & 0xFFFF;
            }
            0x9518 => {
                // RETI
                let hi = self.pop8();
                let lo = self.pop8();
                self.pc = (((hi as u32) << 8) | lo as u32) * 2 & 0xFFFF;
                self.set_flag(I, true);
            }
            0x9409 => {
                // IJMP
                let z = ((self.regs[31] as u32) << 8) | self.regs[30] as u32;
                self.pc = z * 2 & 0xFFFF;
            }
            0x9509 => {
                // ICALL
                let return_word = (self.pc >> 1) as u16;
                self.push8(return_word as u8);
                self.push8((return_word >> 8) as u8);
                let z = ((self.regs[31] as u32) << 8) | self.regs[30] as u32;
                self.pc = z * 2 & 0xFFFF;
            }
            0x9588 => self.halted = true, // SLEEP
            0x9598 => self.halted = true, // BREAK
            0x95A8 => {}                  // WDR
            0x9478 => self.set_flag(I, true),  // SEI
            0x94F8 => self.set_flag(I, false), // CLI
            0x95C8 => {}                  // LPM (R0 <- program[Z])
            0x95E8 => {}                  // SPM / reserved
            w if w & 0xFE0F == 0x9000 => {
                // LDS Rd, k (32-bit)
                let d = ((word >> 4) & 0x1F) as usize;
                let address = self.fetch_word();
                self.regs[d] = self.read_data(address);
            }
            w if w & 0xFE0F == 0x9200 => {
                // STS k, Rr (32-bit)
                let d = ((word >> 4) & 0x1F) as usize;
                let address = self.fetch_word();
                let value = self.regs[d];
                self.write_data(address, value);
            }
            0x940C => {
                // JMP k (32-bit, k is a word address)
                let target = self.fetch_word() as u32;
                self.pc = target * 2 & 0xFFFF;
            }
            0x940E => {
                // CALL k (32-bit, k is a word address)
                let target = self.fetch_word() as u32;
                let return_word = (self.pc >> 1) as u16;
                self.push8(return_word as u8);
                self.push8((return_word >> 8) as u8);
                self.pc = target * 2 & 0xFFFF;
            }
            _ => {
                // LD/ST through X/Y/Z and the pointer post/pre-increment forms.
                match word & 0xFE0F {
                    0x900C => {
                        let d = ((word >> 4) & 0x1F) as usize;
                        let addr = self.pointer(26);
                        self.regs[d] = self.read_data(addr);
                    }
                    0x900D => {
                        let d = ((word >> 4) & 0x1F) as usize;
                        let addr = self.pointer(26);
                        self.regs[d] = self.read_data(addr);
                        self.inc_pointer(26);
                    }
                    0x900E => {
                        let d = ((word >> 4) & 0x1F) as usize;
                        self.dec_pointer(26);
                        let addr = self.pointer(26);
                        self.regs[d] = self.read_data(addr);
                    }
                    0x9009 => {
                        let d = ((word >> 4) & 0x1F) as usize;
                        let addr = self.pointer(28);
                        self.regs[d] = self.read_data(addr);
                        self.inc_pointer(28);
                    }
                    0x900A => {
                        let d = ((word >> 4) & 0x1F) as usize;
                        self.dec_pointer(28);
                        let addr = self.pointer(28);
                        self.regs[d] = self.read_data(addr);
                    }
                    0x9001 => {
                        let d = ((word >> 4) & 0x1F) as usize;
                        let addr = self.pointer(30);
                        self.regs[d] = self.read_data(addr);
                        self.inc_pointer(30);
                    }
                    0x9002 => {
                        let d = ((word >> 4) & 0x1F) as usize;
                        self.dec_pointer(30);
                        let addr = self.pointer(30);
                        self.regs[d] = self.read_data(addr);
                    }
                    0x920C => {
                        let d = ((word >> 4) & 0x1F) as usize;
                        let addr = self.pointer(26);
                        let value = self.regs[d];
                        self.write_data(addr, value);
                    }
                    0x920D => {
                        let d = ((word >> 4) & 0x1F) as usize;
                        let addr = self.pointer(26);
                        self.inc_pointer(26);
                        let value = self.regs[d];
                        self.write_data(addr, value);
                    }
                    0x920E => {
                        let d = ((word >> 4) & 0x1F) as usize;
                        self.dec_pointer(26);
                        let addr = self.pointer(26);
                        let value = self.regs[d];
                        self.write_data(addr, value);
                    }
                    0x9209 => {
                        let d = ((word >> 4) & 0x1F) as usize;
                        let addr = self.pointer(28);
                        let value = self.regs[d];
                        self.write_data(addr, value);
                        self.inc_pointer(28);
                    }
                    0x920A => {
                        let d = ((word >> 4) & 0x1F) as usize;
                        self.dec_pointer(28);
                        let addr = self.pointer(28);
                        let value = self.regs[d];
                        self.write_data(addr, value);
                    }
                    0x9201 => {
                        let d = ((word >> 4) & 0x1F) as usize;
                        let addr = self.pointer(30);
                        let value = self.regs[d];
                        self.write_data(addr, value);
                        self.inc_pointer(30);
                    }
                    0x9202 => {
                        let d = ((word >> 4) & 0x1F) as usize;
                        self.dec_pointer(30);
                        let addr = self.pointer(30);
                        let value = self.regs[d];
                        self.write_data(addr, value);
                    }
                    0x900F => {
                        // POP
                        let d = ((word >> 4) & 0x1F) as usize;
                        self.regs[d] = self.pop8();
                    }
                    0x920F => {
                        // PUSH
                        let d = ((word >> 4) & 0x1F) as usize;
                        let value = self.regs[d];
                        self.push8(value);
                    }
                    0x9004 => {
                        // LPM Rd, Z
                        let d = ((word >> 4) & 0x1F) as usize;
                        let addr = self.pointer(30) as u32;
                        self.regs[d] = self.read_prog(addr);
                    }
                    _ => {
                        self.halted = true;
                    }
                }
            }
        }
    }

    fn pointer(&self, low: usize) -> u16 {
        ((self.regs[low + 1] as u16) << 8) | self.regs[low] as u16
    }

    fn inc_pointer(&mut self, low: usize) {
        let value = self.pointer(low).wrapping_add(1);
        self.regs[low] = value as u8;
        self.regs[low + 1] = (value >> 8) as u8;
    }

    fn dec_pointer(&mut self, low: usize) {
        let value = self.pointer(low).wrapping_sub(1);
        self.regs[low] = value as u8;
        self.regs[low + 1] = (value >> 8) as u8;
    }

    /// Bytes of the data space that are backed by memory (for the IDE memory
    /// view). Registers and I/O are exposed through `read_data` but not dumped.
    pub fn data_byte(&self, addr: u16) -> u8 {
        self.read_data(addr)
    }

    /// Total data-space size (for bounds checks in the IDE).
    pub fn data_size(&self) -> usize {
        DATA_SIZE
    }
}

/// Which I/O-bit operation to apply.
#[derive(Debug, Clone, Copy)]
enum IoOp {
    Set,
    Clear,
    SkipIfClear,
    SkipIfSet,
}

impl Default for AvrCore {
    fn default() -> Self {
        Self::new()
    }
}

/// Disassemble one AVR instruction at `pc` using `read` for program bytes.
/// Produces the common mnemonics; unknown words print as `.word 0xXXXX`.
pub fn disassemble(pc: u32, read: &dyn Fn(u32) -> u8) -> String {
    let lo = read(pc) as u16;
    let hi = read(pc + 1) as u16;
    let word = (hi << 8) | lo;
    let reg = |w: u16| ((w & 0x0F) | ((w >> 5) & 0x10)) as usize;
    let reg2 = |w: u16| ((w >> 5) & 0x0F) as usize | ((w >> 5) & 0x10) as usize;
    match word {
        0x0000 => "nop".to_string(),
        w if w & 0xFF00 == 0x0100 => format!(
            "movw r{}, r{}",
            ((w >> 4) & 0x0F) * 2,
            (w & 0x0F) * 2
        ),
        w if w & 0xFC00 == 0x0C00 => format!("add r{}, r{}", reg(w), reg2(w)),
        w if w & 0xFC00 == 0x1C00 => format!("adc r{}, r{}", reg(w), reg2(w)),
        w if w & 0xFC00 == 0x1800 => format!("sub r{}, r{}", reg(w), reg2(w)),
        w if w & 0xFC00 == 0x0800 => format!("sbc r{}, r{}", reg(w), reg2(w)),
        w if w & 0xFC00 == 0x1400 => format!("cp r{}, r{}", reg(w), reg2(w)),
        w if w & 0xFC00 == 0x0400 => format!("cpc r{}, r{}", reg(w), reg2(w)),
        w if w & 0xFC00 == 0x2000 => format!("and r{}, r{}", reg(w), reg2(w)),
        w if w & 0xFC00 == 0x2400 => format!("eor r{}, r{}", reg(w), reg2(w)),
        w if w & 0xFC00 == 0x2800 => format!("or r{}, r{}", reg(w), reg2(w)),
        w if w & 0xFC00 == 0x2C00 => format!("mov r{}, r{}", reg(w), reg2(w)),
        w if w & 0xFC00 == 0x1000 => format!("cpse r{}, r{}", reg(w), reg2(w)),
        w if w & 0xFC00 == 0x9C00 => format!("mul r{}, r{}", reg(w), reg2(w)),
        w if w & 0xF000 == 0x3000 => format!("cpi r{}, 0x{:02x}", 16 + ((w >> 4) & 0x0F), (((w >> 4) & 0xF0) as u8) | (w & 0x0F) as u8),
        w if w & 0xF000 == 0x4000 => format!("sbci r{}, 0x{:02x}", 16 + ((w >> 4) & 0x0F), (((w >> 4) & 0xF0) as u8) | (w & 0x0F) as u8),
        w if w & 0xF000 == 0x5000 => format!("subi r{}, 0x{:02x}", 16 + ((w >> 4) & 0x0F), (((w >> 4) & 0xF0) as u8) | (w & 0x0F) as u8),
        w if w & 0xF000 == 0x6000 => format!("ori r{}, 0x{:02x}", 16 + ((w >> 4) & 0x0F), (((w >> 4) & 0xF0) as u8) | (w & 0x0F) as u8),
        w if w & 0xF000 == 0x7000 => format!("andi r{}, 0x{:02x}", 16 + ((w >> 4) & 0x0F), (((w >> 4) & 0xF0) as u8) | (w & 0x0F) as u8),
        w if w & 0xF000 == 0xE000 => format!("ldi r{}, 0x{:02x}", 16 + ((w >> 4) & 0x0F), (((w >> 4) & 0xF0) as u8) | (w & 0x0F) as u8),
        w if w & 0xF000 == 0xC000 => format!("rjmp .{:+}", sign12(w) * 2 + 2),
        w if w & 0xF000 == 0xD000 => format!("rcall .{:+}", sign12(w) * 2 + 2),
        w if w & 0xFC00 == 0xF000 => format!("brbs {}, .{:+}", w & 7, sign7((w >> 3) & 0x7F) * 2 + 2),
        w if w & 0xFC00 == 0xF400 => format!("brbc {}, .{:+}", w & 7, sign7((w >> 3) & 0x7F) * 2 + 2),
        w if w & 0xFE08 == 0xF800 => format!("bld r{}, {}", (w >> 4) & 0x1F, w & 7),
        w if w & 0xFE08 == 0xFA00 => format!("bst r{}, {}", (w >> 4) & 0x1F, w & 7),
        w if w & 0xFE08 == 0xFC00 => format!("sbrc r{}, {}", (w >> 4) & 0x1F, w & 7),
        w if w & 0xFE08 == 0xFE00 => format!("sbrs r{}, {}", (w >> 4) & 0x1F, w & 7),
        w if w & 0xFF00 == 0x9800 => format!("cbi 0x{:02x}, {}", (w >> 3) & 0x1F, w & 7),
        w if w & 0xFF00 == 0x9A00 => format!("sbi 0x{:02x}, {}", (w >> 3) & 0x1F, w & 7),
        w if w & 0xFF00 == 0x9B00 => format!("sbis 0x{:02x}, {}", (w >> 3) & 0x1F, w & 7),
        w if w & 0xFF00 == 0x9900 => format!("sbic 0x{:02x}, {}", (w >> 3) & 0x1F, w & 7),
        w if w & 0xF800 == 0xB000 => format!("in r{}, 0x{:02x}", (w >> 4) & 0x1F, ((w >> 5) & 0x30) | (w & 0x0F)),
        w if w & 0xF800 == 0xB800 => format!("out 0x{:02x}, r{}", ((w >> 5) & 0x30) | (w & 0x0F), (w >> 4) & 0x1F),
        w if w & 0xFE00 == 0x9400 => match w & 0x000F {
            0x0 => format!("com r{}", (w >> 4) & 0x1F),
            0x1 => format!("neg r{}", (w >> 4) & 0x1F),
            0x2 => format!("swap r{}", (w >> 4) & 0x1F),
            0x3 => format!("inc r{}", (w >> 4) & 0x1F),
            0x5 => format!("asr r{}", (w >> 4) & 0x1F),
            0x6 => format!("lsr r{}", (w >> 4) & 0x1F),
            0x7 => format!("ror r{}", (w >> 4) & 0x1F),
            0xA => format!("dec r{}", (w >> 4) & 0x1F),
            0xF => format!("push r{}", (w >> 4) & 0x1F),
            0xE => "spm".to_string(),
            _ => match w {
                0x9508 => "ret".to_string(),
                0x9518 => "reti".to_string(),
                0x9409 => "ijmp".to_string(),
                0x9509 => "icall".to_string(),
                0x9588 => "sleep".to_string(),
                0x9598 => "break".to_string(),
                0x95A8 => "wdr".to_string(),
                0x9478 => "sei".to_string(),
                0x94F8 => "cli".to_string(),
                0x9000 => format!("lds r{}, 0x{:04x}", (w >> 4) & 0x1F, read16(pc + 2, read)),
                0x9200 => format!("sts 0x{:04x}, r{}", read16(pc + 2, read), (w >> 4) & 0x1F),
                0x940C => format!("jmp 0x{:x}", read16(pc + 2, read)),
                0x940E => format!("call 0x{:x}", read16(pc + 2, read)),
                0x900C => format!("ld r{}, x", (w >> 4) & 0x1F),
                0x900D => format!("ld r{}, x+", (w >> 4) & 0x1F),
                0x900E => format!("ld r{}, -x", (w >> 4) & 0x1F),
                0x9009 => format!("ld r{}, y+", (w >> 4) & 0x1F),
                0x900A => format!("ld r{}, -y", (w >> 4) & 0x1F),
                0x9001 => format!("ld r{}, z+", (w >> 4) & 0x1F),
                0x9002 => format!("ld r{}, -z", (w >> 4) & 0x1F),
                0x920C => format!("st x, r{}", (w >> 4) & 0x1F),
                0x920D => format!("st x+, r{}", (w >> 4) & 0x1F),
                0x920E => format!("st -x, r{}", (w >> 4) & 0x1F),
                0x9209 => format!("st y+, r{}", (w >> 4) & 0x1F),
                0x920A => format!("st -y, r{}", (w >> 4) & 0x1F),
                0x9201 => format!("st z+, r{}", (w >> 4) & 0x1F),
                0x9202 => format!("st -z, r{}", (w >> 4) & 0x1F),
                0x900F => format!("pop r{}", (w >> 4) & 0x1F),
                0x9004 => format!("lpm r{}, z", (w >> 4) & 0x1F),
                _ => format!(".word 0x{word:04x}"),
            },
        },
        w if w & 0xD000 == 0x8000 => {
            let d = (w >> 4) & 0x1F;
            let q = ((w >> 8) & 0x20) | ((w >> 7) & 0x18) | (w & 0x07);
            let y = w & 0x08 != 0;
            let store = w & 0x0200 != 0;
            let pointer = if y { "y" } else { "z" };
            if store {
                format!("std {pointer}+{q}, r{d}")
            } else {
                format!("ldd r{d}, {pointer}+{q}")
            }
        }
        _ => format!(".word 0x{word:04x}"),
    }
}

fn read16(pc: u32, read: &dyn Fn(u32) -> u8) -> u16 {
    (read(pc + 1) as u16) << 8 | read(pc) as u16
}

fn sign12(word: u16) -> i32 {
    let k = (word & 0x0FFF) as i32;
    if k & 0x0800 != 0 { k - 0x1000 } else { k }
}

fn sign7(value: u16) -> i32 {
    let k = value as i32;
    if k & 0x40 != 0 { k - 0x80 } else { k }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core_with(program: &[u16]) -> AvrCore {
        let mut core = AvrCore::new();
        let mut bytes = Vec::new();
        for word in program {
            bytes.push(*word as u8);
            bytes.push((*word >> 8) as u8);
        }
        core.load_image(0, &bytes);
        core
    }

    #[test]
    fn ldi_add_and_flags() {
        // ldi r16, 0x0A ; ldi r17, 0x05 ; add r16, r17
        let mut core = core_with(&[0xE00A, 0xE015, 0x0E30]);
        core.step(&[]);
        core.step(&[]);
        core.step(&[]);
        assert_eq!(core.regs[16], 0x0F);
        assert!(core.flag(Z) == false);
        assert!(core.flag(C) == false);
        assert!(core.flag(N) == false);
    }

    #[test]
    fn add_sets_carry_and_zero() {
        // ldi r16, 0xFF ; ldi r17, 0x01 ; add r16, r17
        let mut core = core_with(&[0xEF0F, 0xE011, 0x0E30]);
        core.step(&[]);
        core.step(&[]);
        core.step(&[]);
        assert_eq!(core.regs[16], 0x00);
        assert!(core.flag(C));
        assert!(core.flag(Z));
    }

    #[test]
    fn sub_flags_borrow() {
        // ldi r16, 1 ; ldi r17, 2 ; sub r16, r17 -> 0xFF, C set
        let mut core = core_with(&[0xE001, 0xE012, 0x1A30]);
        for _ in 0..3 {
            core.step(&[]);
        }
        assert_eq!(core.regs[16], 0xFF);
        assert!(core.flag(C));
        assert!(core.flag(N));
    }

    #[test]
    fn loop_counts_down_and_halts_at_sleep() {
        // ldi r16, 5
        // loop: dec r16
        //       brne loop
        //       sleep
        // Words: 0xE005, dec r16 = 0x950A, brne -. Words at byte 4 and 6.
        let mut core = core_with(&[0xE005, 0x950A, 0xF7F1, 0x9588]);
        let mut steps = 0;
        let mut result = AvrStep::Advanced;
        while result == AvrStep::Advanced && steps < 100 {
            result = core.step(&[]);
            steps += 1;
        }
        assert_eq!(result, AvrStep::Halted);
        assert_eq!(core.regs[16], 0);
        assert_eq!(steps, 1 + 1 + 5 * 2);
        assert!(core.flag(Z));
    }

    #[test]
    fn push_pop_round_trip() {
        // ldi r16, 0x42 ; push r16 ; ldi r16, 0 ; pop r16
        let mut core = core_with(&[0xE402, 0x930F, 0xE000, 0x910F]);
        for _ in 0..4 {
            core.step(&[]);
        }
        assert_eq!(core.regs[16], 0x42);
        assert_eq!(core.sp, 0x08FF);
    }

    #[test]
    fn rcall_and_ret() {
        // rcall 0 (call +2) ; ldi r16, 1 ; sleep ; ret
        // bytes: 0: rcall +0 (target word 1, byte 2)
        //        2: ldi r16,1 (target of call)
        //        4: sleep (return address word 2)
        //        6: ret
        let mut core = core_with(&[0xD000, 0xE011, 0x9588, 0x9508]);
        core.step(&[]); // rcall -> pc = 2
        assert_eq!(core.pc, 2);
        core.step(&[]); // ldi
        core.step(&[]); // sleep -> halted
        assert!(core.halted);
        // Reset halted and jump to ret to verify the return address.
        core.halted = false;
        core.set_pc(6);
        core.step(&[]);
        assert_eq!(core.pc, 2);
    }

    #[test]
    fn lds_sts_use_data_space() {
        // ldi r16, 0x99 ; sts 0x0100, r16 ; lds r17, 0x0100
        // sts = 0x9200 | d<<4 ; lds = 0x9000 | d<<4
        let mut core = core_with(&[
            0xE909,
            0x9200 | (16 << 4),
            0x0100,
            0x9000 | (17 << 4),
            0x0100,
        ]);
        for _ in 0..3 {
            core.step(&[]);
        }
        assert_eq!(core.regs[17], 0x99);
        assert_eq!(core.read_data(0x0100), 0x99);
    }

    #[test]
    fn pointer_store_and_load() {
        // Load a pointer into Z (r31:r30) then st Z+, r16 / ld r17, -Z.
        // ldi r30, 0x00; ldi r31, 0x02; ldi r16, 0x7E;
        // st z+, r16 (0x9201|0x100); ld r17,-z (0x9002|0x110)
        let mut core = core_with(&[
            0xE0E0,
            0xE0F2,
            0xE70E,
            0x9201 | (16 << 4),
            0x9002 | (17 << 4),
        ]);
        for _ in 0..5 {
            core.step(&[]);
        }
        assert_eq!(core.read_data(0x0200), 0x7E);
        assert_eq!(core.regs[17], 0x7E);
    }

    #[test]
    fn io_out_and_in() {
        // ldi r16, 0x81 ; out 0x05, r16 ; in r17, 0x05
        let mut core = core_with(&[0xE801, 0xB905, 0xB115]);
        for _ in 0..3 {
            core.step(&[]);
        }
        assert_eq!(core.io[5], 0x81);
        assert_eq!(core.regs[17], 0x81);
    }

    #[test]
    fn skip_instruction_on_compare() {
        // cpse r16, r17 (equal -> skip ldi) ; ldi r18, 1 ; ldi r19, 2
        let mut core = core_with(&[0x1000 | (17 << 5) | 16, 0xE021, 0xE032]);
        core.step(&[]); // skip
        core.step(&[]); // ldi r19
        assert_eq!(core.regs[18], 0);
        assert_eq!(core.regs[19], 2);
    }

    #[test]
    fn breakpoint_stops_before_executing() {
        let mut core = core_with(&[0xE011, 0xE022]);
        let result = core.step(&[0]);
        assert_eq!(result, AvrStep::Breakpoint);
        assert_eq!(core.pc, 0);
    }

    #[test]
    fn disassembles_common_instructions() {
        let bytes: Vec<u8> = [0x0A, 0xE0, 0x0F, 0xE0].to_vec();
        let read = |addr: u32| *bytes.get(addr as usize).unwrap_or(&0);
        assert_eq!(disassemble(0, &read), "ldi r16, 0x0a");
        assert_eq!(disassemble(2, &read), "ldi r16, 0x0f");
    }

    #[test]
    fn indirect_call_uses_z() {
        // ldi r30, 4 ; ldi r31, 0 ; icall ; nop ; ret
        let mut core = core_with(&[0xE0E4, 0xE010, 0x9509, 0x0000, 0x9508]);
        core.step(&[]);
        core.step(&[]);
        core.step(&[]);
        assert_eq!(core.pc, 8);
    }
}
