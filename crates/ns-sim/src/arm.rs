//! An ARM Cortex-M (Thumb/Thumb-2) instruction-set simulator core.
//!
//! Models the ARMv7-M architecture as used by STM32, nRF52 and RP2040: 16
//! general-purpose registers (R13=SP, R14=LR, R15=PC), the xPSR program
//! status register, PRIMASK/BASEPRI interrupt masking, the vector table
//! (initial SP + reset vector + exception vectors), and the Thumb instruction
//! set (16-bit Thumb-1 plus the commonly used 32-bit Thumb-2 encodings such
//! as `BL` and wide data-processing forms).
//!
//! The memory model is a flat 32-bit address space backed by a byte vector
//! large enough to cover the common Cortex-M flash/RAM windows; accesses
//! outside the simulated range read 0 and drop writes (mirroring an unbacked
//! address) rather than panicking.

/// Simulated address-space size (64 MiB covers flash + peripheral + RAM
/// windows for the common Cortex-M parts).
pub const ADDR_SIZE: usize = 64 * 1024 * 1024;

/// Stack pointer register index.
pub const SP: usize = 13;
/// Link register index.
pub const LR: usize = 14;
/// Program counter register index.
pub const PC: usize = 15;

/// An ARM Cortex-M core.
pub struct ArmCore {
    /// Register file R0-R15 (R13=SP, R14=LR, R15=PC).
    pub regs: [u32; 16],
    /// Combined program status register (APSR/IPSR/EPSR). Bits 31-28 = N/Z/C/V
    /// condition flags, bit 24 = Thumb, bits 0-8 = exception number.
    pub xpsr: u32,
    /// Interrupt-priority mask register (bit 0 = mask all).
    pub primask: u32,
    /// Base priority mask (values >= this are masked; 0 = disabled).
    pub basepri: u32,
    /// Fault mask register (bit 0 = mask all faults).
    pub faultmask: u32,
    /// Control register (bit 0 = thread stack pointer select, bit 1 = FPU).
    pub control: u32,
    /// Flat memory image.
    pub memory: Vec<u8>,
    /// Vector-table base address (VTOR).
    pub vtor: u32,
    /// Whether the core executes in ARM (A32) state rather than Thumb state.
    /// ARM7/ARM9/Cortex-A reset with a vector whose bit 0 is clear; Cortex-M
    /// always boots in Thumb state (this stays `false`).
    pub arm_state: bool,
    /// Whether execution should stop.
    pub halted: bool,
    /// Number of instructions executed since reset.
    pub cycle_count: u64,
}

impl ArmCore {
    pub fn new() -> Self {
        Self {
            regs: [0; 16],
            xpsr: 0x0100_0000, // Thumb bit set
            primask: 0,
            basepri: 0,
            faultmask: 0,
            control: 0,
            memory: vec![0; ADDR_SIZE],
            vtor: 0,
            arm_state: false,
            halted: false,
            cycle_count: 0,
        }
    }

    /// Set the program counter (keeps the Thumb bit in the low bit).
    pub fn set_pc(&mut self, pc: u32) {
        self.regs[PC] = pc & !1;
    }

    /// Select ARM (A32) or Thumb decoding. ARM7/ARM9/Cortex-A firmware runs
    /// in ARM state; Cortex-M never does.
    pub fn set_arm_state(&mut self, arm: bool) {
        self.arm_state = arm;
        self.set_thumb(!arm);
    }

    // -------- condition flags --------
    pub fn flag_n(&self) -> bool {
        self.xpsr & 0x8000_0000 != 0
    }
    pub fn flag_z(&self) -> bool {
        self.xpsr & 0x4000_0000 != 0
    }
    pub fn flag_c(&self) -> bool {
        self.xpsr & 0x2000_0000 != 0
    }
    pub fn flag_v(&self) -> bool {
        self.xpsr & 0x1000_0000 != 0
    }
    fn set_nz(&mut self, value: u32) {
        self.xpsr = (self.xpsr & !0xC000_0000)
            | (if value & 0x8000_0000 != 0 { 0x8000_0000 } else { 0 });
        if value == 0 {
            self.xpsr |= 0x4000_0000;
        }
    }

    /// The Thumb bit of xPSR (bit 24).
    pub fn thumb(&self) -> bool {
        self.xpsr & 0x0100_0000 != 0
    }
    fn set_thumb(&mut self, value: bool) {
        if value {
            self.xpsr |= 0x0100_0000;
        } else {
            self.xpsr &= !0x0100_0000;
        }
    }

    /// Exception number currently active (bits 0-8 of IPSR).
    pub fn exception_number(&self) -> u32 {
        self.xpsr & 0x1FF
    }
    fn set_exception(&mut self, number: u32) {
        self.xpsr = (self.xpsr & !0x1FF) | (number & 0x1FF);
    }
    // -------- memory access --------
    /// Map a bus address onto the flat simulation buffer. The flash window
    /// (0x0800_0000) maps to offset 0, the Cortex-M SRAM window (0x2000_0000)
    /// maps to offset 0x2000_0000, the ARM7/ARM9 SRAM windows (0x3000_0000 for
    /// S3C2440, 0x4000_0000 for LPC2148) map to 0x3000_0000/0x4000_0000, and
    /// low addresses (code loaded at 0 for tests) map to themselves. Returns
    /// None for unbacked addresses.
    fn map_addr(&self, address: u32) -> Option<usize> {
        let a = address;
        let offset = if (0x0800_0000..0x1000_0000).contains(&a) {
            a - 0x0800_0000
        } else if (0x2000_0000..0x2400_0000).contains(&a) {
            a - 0x2000_0000 + 0x0200_0000
        } else if (0x3000_0000..0x3400_0000).contains(&a) {
            a - 0x3000_0000 + 0x3000_0000
        } else if (0x4000_0000..0x4400_0000).contains(&a) {
            a - 0x4000_0000 + 0x4000_0000
        } else if a < 0x2000_0000 {
            a
        } else {
            return None;
        };
        if (offset as usize) < self.memory.len() {
            Some(offset as usize)
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
            return; // unbacked address: drop write
        };
        if base + 4 > self.memory.len() {
            return;
        }
        self.memory[base..base + 4].copy_from_slice(&value.to_le_bytes());
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

    /// Write a block of bytes (e.g. an ELF segment).
    pub fn write_bytes(&mut self, base: u32, bytes: &[u8]) {
        for (i, byte) in bytes.iter().enumerate() {
            let Some(addr) = self.map_addr(base.wrapping_add(i as u32)) else {
                continue;
            };
            self.memory[addr] = *byte;
        }
    }

    // -------- vector table --------
    /// Reset the core from the vector table at `vtor` (like real hardware).
    ///
    /// Cortex-M stores initial SP at `vtor` and reset PC (Thumb bit set) at
    /// `vtor+4`. ARM7/ARM9/Cortex-A (A32) instead keep *code* at the vector
    /// table — the reset vector is an ARM branch instruction — so the reset PC
    /// comes from the ELF entry (bit 0 clear = ARM state) and SP stays default.
    pub fn reset_from_vector_table(&mut self, vtor: u32, entry: u32) {
        self.vtor = vtor;
        let word0 = self.read_u32(vtor);
        let word1 = self.read_u32(vtor + 4);
        // ARM7/ARM9/Cortex-A keep *code* at the vector table: word0 is an ARM
        // branch instruction (top nibble 0xE). Cortex-M stores data: word0 is
        // the initial SP (high address) and word1 the reset PC (Thumb bit set).
        let is_code_vector = word0 & 0xF000_0000 == 0xE000_0000;
        if is_code_vector {
            // A32 device: the ELF entry (bit 0 clear) is the reset PC.
            let thumb = entry & 1 != 0;
            self.regs[SP] = 0x2000_0000;
            self.regs[PC] = entry & !1;
            self.arm_state = !thumb;
            self.set_thumb(thumb);
        } else {
            // Cortex-M data vector: word0 = initial SP, word1 = reset PC.
            let initial_sp = if word0 >= 0x2000_0000 { word0 } else { 0x2000_0000 };
            let thumb = word1 & 1 != 0;
            let pc = if word1 != 0 { word1 } else { entry };
            self.regs[SP] = initial_sp;
            self.regs[PC] = pc & !1;
            self.arm_state = !thumb;
            self.set_thumb(thumb);
        }
        self.xpsr &= !0x1FF; // clear IPSR
        self.halted = false;
        self.cycle_count = 0;
    }

    /// Read the exception vector for `number` (0 = reset, others from the
    /// table at VTOR + 4*number). Returns None for unbacked/zero vectors.
    pub fn vector(&self, number: u32) -> Option<u32> {
        let addr = self.vtor.wrapping_add(number.wrapping_mul(4));
        if (addr as usize) + 4 > self.memory.len() {
            return None;
        }
        let value = self.read_u32(addr);
        if value == 0 {
            None
        } else {
            Some(value)
        }
    }
}

impl ArmCore {
    /// Execute one Thumb instruction. The instruction at the current PC is
    /// decoded as 16-bit Thumb-1 or 32-bit Thumb-2 (when the first halfword
    /// has the 0xF000-0xF800 prefix). Returns the outcome.
    pub fn step(&mut self, breakpoints: &[u32]) -> ArmStep {
        if self.halted {
            return ArmStep::Halted;
        }
        let pc = self.regs[PC] & !1;
        if breakpoints.iter().any(|bp| *bp & !1 == pc) {
            return ArmStep::Breakpoint;
        }
        if self.arm_state {
            // ARM state: every instruction is a 32-bit word.
            let insn = self.read_u32(pc);
            self.regs[PC] = pc.wrapping_add(4);
            self.execute_arm(insn);
            self.cycle_count += 1;
            if self.halted {
                return ArmStep::Halted;
            }
            return ArmStep::Advanced;
        }
        let half = self.read_u16(pc);
        let (insn, width) = if half & 0xE000 == 0xE000 {
            let second = self.read_u16(pc.wrapping_add(2));
            (((half as u32) << 16) | second as u32, 4)
        } else {
            (half as u32, 2)
        };
        self.regs[PC] = pc.wrapping_add(width as u32);
        self.execute(insn, width == 4);
        self.cycle_count += 1;
        if self.halted {
            ArmStep::Halted
        } else {
            ArmStep::Advanced
        }
    }

    fn execute(&mut self, insn: u32, wide: bool) {
        if wide {
            self.execute_wide(insn);
        } else {
            self.execute_thumb1(insn as u16);
        }
    }
}

/// Outcome of a single ARM step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmStep {
    Advanced,
    Breakpoint,
    Halted,
}

impl ArmCore {

    /// Decode and execute a 16-bit Thumb-1 instruction.
    fn execute_thumb1(&mut self, insn: u16) {
        let top = insn >> 12; // bits 15-12: the Thumb-1 major group
        let rm = ((insn >> 3) & 0x0F) as usize;
        let rn = ((insn >> 6) & 0x07) as usize;
        let rd = (insn & 0x07) as usize;
        let imm8 = (insn & 0xFF) as u32;
        let imm3 = ((insn >> 6) & 0x07) as u32;
        let imm5 = ((insn >> 6) & 0x1F) as u32;
        match top {
            // 0x0: LSL/LSR/ASR immediate (shift op = bits 12-11)
            0x0 => {
                let shift_op = (insn >> 11) & 0x03;
                let shift = imm5;
                let value = self.regs[rm];
                let r = match shift_op {
                    0 => {
                        let r = if shift == 0 { value } else { value << shift };
                        if shift != 0 {
                            self.xpsr = (self.xpsr & !0x2000_0000)
                                | (((value >> (32 - shift)) & 1) << 29);
                        }
                        r
                    }
                    1 => {
                        let r = if shift == 0 { 0 } else { value >> shift };
                        if shift != 0 {
                            self.xpsr = (self.xpsr & !0x2000_0000)
                                | (((value >> (shift - 1)) & 1) << 29);
                        }
                        r
                    }
                    _ => {
                        let shift = if shift == 0 { 32 } else { shift };
                        let r = ((value as i32) >> shift) as u32;
                        if shift != 32 {
                            self.xpsr = (self.xpsr & !0x2000_0000)
                                | (((value >> (shift - 1)) & 1) << 29);
                        }
                        r
                    }
                };
                self.set_nz(r);
                self.regs[rd] = r;
            }
            // 0x1: ADD/SUB (bit 10 = sub, bit 9 = immediate)
            0x1 => {
                let sub = (insn >> 10) & 1 != 0;
                let immediate = (insn >> 9) & 1 != 0;
                let value = self.regs[rn];
                let operand = if immediate { imm3 } else { self.regs[rm] };
                let r = if sub {
                    self.set_add_carry(value, operand, true);
                    value.wrapping_sub(operand)
                } else {
                    self.set_add_carry(value, operand, false);
                    value.wrapping_add(operand)
                };
                self.set_nz(r);
                self.regs[rd] = r;
            }
            // 0x2: MOV/CMP immediate 8-bit (bit 11 = CMP)
            0x2 => {
                let rd = ((insn >> 8) & 0x07) as usize;
                let cmp = (insn >> 11) & 1 != 0;
                if cmp {
                    let value = self.regs[rd];
                    self.set_add_carry(value, imm8, true);
                    self.set_nz(value.wrapping_sub(imm8));
                } else {
                    // MOVS: writes the register and updates N/Z.
                    self.regs[rd] = imm8;
                    self.set_nz(imm8);
                }
            }
            // 0x3: ADD/SUB immediate 8-bit (bit 10 = sub)
            0x3 => {
                let rd = ((insn >> 8) & 0x07) as usize;
                let sub = (insn >> 11) & 1 != 0;
                let value = self.regs[rd];
                let r = if sub {
                    self.set_add_carry(value, imm8, true);
                    value.wrapping_sub(imm8)
                } else {
                    self.set_add_carry(value, imm8, false);
                    value.wrapping_add(imm8)
                };
                self.set_nz(r);
                self.regs[rd] = r;
            }
            // 0x4: ALU (0x4000-0x43FF), high-register/BX (0x4400-0x47FF),
            // or PC-relative LDR (0x4800-0x4FFF).
            0x4 => {
                if insn & 0x0800 != 0 {
                    // PC-relative LDR: rd = bits 10-8, imm8 scaled by 4.
                    let rd = ((insn >> 8) & 0x07) as usize;
                    let addr = ((self.regs[PC] & !2).wrapping_add(imm8.wrapping_mul(4))) & !3;
                    self.regs[rd] = self.read_u32(addr);
                } else if insn & 0x0400 != 0 {
                    // High-register ops / BX (op = bits 9-8)
                    let rd_high = rd | (((insn >> 4) & 0x08) as usize);
                    let rm_high = rm | (((insn >> 3) & 0x08) as usize);
                    let opcode = (insn >> 8) & 0x03;
                    match opcode {
                        0 => {
                            let r = self.regs[rd_high].wrapping_add(self.regs[rm_high]);
                            self.regs[rd_high] = r;
                        }
                        1 => {
                            self.set_add_carry(self.regs[rd_high], self.regs[rm_high], true);
                            let r = self.regs[rd_high].wrapping_sub(self.regs[rm_high]);
                            self.set_nz(r);
                        }
                        2 => {
                            self.regs[rd_high] = self.regs[rm_high];
                        }
                        _ => {
                            let target = self.regs[rm_high];
                            self.set_thumb(target & 1 != 0);
                            self.regs[PC] = target & !1;
                        }
                    }
                } else {
                    // ALU
                    let alu = (insn >> 6) & 0x0F;
                    self.execute_alu(alu as u32, rd, rm);
                }
            }
            // 0x5: load/store register offset (opcode = bits 11-9)
            0x5 => {
                let opcode = (insn >> 9) & 0x07;
                let addr = self.regs[rn].wrapping_add(self.regs[rm]);
                match opcode {
                    0 => self.write_u32(addr, self.regs[rd]),
                    1 => self.write_u16(addr, self.regs[rd] as u16),
                    2 => self.write_u8(addr, self.regs[rd] as u8),
                    3 => {
                        // LDRSB: sign-extend byte
                        let v = self.read_u8(addr) as i8 as i32 as u32;
                        self.regs[rd] = v;
                    }
                    4 => self.regs[rd] = self.read_u32(addr),
                    5 => self.regs[rd] = self.read_u16(addr) as u32,
                    6 => self.regs[rd] = self.read_u8(addr) as u32,
                    7 => {
                        // LDRSH: sign-extend half
                        let v = self.read_u16(addr) as i16 as i32 as u32;
                        self.regs[rd] = v;
                    }
                    _ => {}
                }
            }
            // 0x6: load/store word immediate offset (bit 11 = load)
            0x6 => {
                let load = (insn >> 11) & 1 != 0;
                let addr = self.regs[rn].wrapping_add(imm5 * 4);
                if load {
                    self.regs[rd] = self.read_u32(addr);
                } else {
                    self.write_u32(addr, self.regs[rd]);
                }
            }
            // 0x7: load/store byte immediate offset (bit 11 = load)
            0x7 => {
                let load = (insn >> 11) & 1 != 0;
                let addr = self.regs[rn].wrapping_add(imm5);
                if load {
                    self.regs[rd] = self.read_u8(addr) as u32;
                } else {
                    self.write_u8(addr, self.regs[rd] as u8);
                }
            }
            // 0x8: load/store half immediate offset (bit 11 = load)
            0x8 => {
                let load = (insn >> 11) & 1 != 0;
                let addr = self.regs[rn].wrapping_add(imm5 * 2);
                if load {
                    self.regs[rd] = self.read_u16(addr) as u32;
                } else {
                    self.write_u16(addr, self.regs[rd] as u16);
                }
            }
            // 0x9: SP-relative load/store word (bit 11 = load)
            0x9 => {
                let load = (insn >> 11) & 1 != 0;
                let addr = self.regs[SP].wrapping_add(imm8.wrapping_mul(4));
                if load {
                    self.regs[rd] = self.read_u32(addr);
                } else {
                    self.write_u32(addr, self.regs[rd]);
                }
            }
            // 0xA: load address (bit 11 = SP-relative)
            0xA => {
                let sp_relative = (insn >> 11) & 1 != 0;
                let base = if sp_relative {
                    self.regs[SP]
                } else {
                    (self.regs[PC] & !2).wrapping_add(4)
                };
                self.regs[rd] = base.wrapping_add(imm8.wrapping_mul(4));
            }
            // 0xB: add/sub SP (0xB0-0xB3) or PUSH/POP (0xB4-0xBF)
            0xB => {
                if insn & 0x0C00 == 0x0000 {
                    let sub = (insn >> 7) & 1 != 0;
                    let value = imm8.wrapping_mul(4);
                    self.regs[SP] = if sub {
                        self.regs[SP].wrapping_sub(value)
                    } else {
                        self.regs[SP].wrapping_add(value)
                    };
                } else {
                    let push = (insn >> 11) & 1 == 0;
                    let rlist = (insn & 0xFF) as u32;
                    if push {
                        let mut sp = self.regs[SP];
                        // PUSH stores the lowest-numbered register at the
                        // lowest address, so push in descending order (and LR
                        // first, as it has the highest number).
                        if insn & 0x100 != 0 {
                            sp = sp.wrapping_sub(4);
                            self.write_u32(sp, self.regs[LR]);
                        }
                        for i in (0..8).rev() {
                            if rlist & (1 << i) != 0 {
                                sp = sp.wrapping_sub(4);
                                self.write_u32(sp, self.regs[i as usize]);
                            }
                        }
                        self.regs[SP] = sp;
                    } else {
                        let mut sp = self.regs[SP];
                        for i in 0..8 {
                            if rlist & (1 << i) != 0 {
                                self.regs[i as usize] = self.read_u32(sp);
                                sp = sp.wrapping_add(4);
                            }
                        }
                        if insn & 0x100 != 0 {
                            self.regs[PC] = self.read_u32(sp) & !1;
                            sp = sp.wrapping_add(4);
                        }
                        self.regs[SP] = sp;
                    }
                }
            }
            // 0xC: multiple load/store (bit 11 = load)
            0xC => {
                let load = (insn >> 11) & 1 != 0;
                let rlist = (insn & 0xFF) as u32;
                let base = self.regs[rn];
                let mut addr = base;
                for i in 0..8 {
                    if rlist & (1 << i) != 0 {
                        if load {
                            self.regs[i as usize] = self.read_u32(addr);
                        } else {
                            self.write_u32(addr, self.regs[i as usize]);
                        }
                        addr = addr.wrapping_add(4);
                    }
                }
                if !load {
                    self.regs[rn] = addr;
                }
            }
            // 0xD: conditional branch, or SVC (0xDF00-0xDFFF)
            0xD => {
                if insn & 0xFF00 == 0xDF00 {
                    self.svc();
                } else {
                    let cond = (insn >> 8) & 0x0F;
                    if self.condition_true(cond as u32) {
                        let offset = sign_extend_thumb((insn & 0xFF) as u32, 8) * 2;
                        self.regs[PC] = (self.regs[PC] & !1).wrapping_add(offset as u32);
                    }
                }
            }
            // 0xE: unconditional branch (0xE000-0xEFFF)
            0xE => {
                let offset = sign_extend_thumb((insn & 0x7FF) as u32, 11) * 2;
                self.regs[PC] = (self.regs[PC] & !1).wrapping_add(offset as u32);
            }
            // 0xF: long branch / SVC
            0xF => {
                if insn & 0xFF00 == 0xDF00 {
                    self.svc();
                } else {
                    self.halted = true;
                }
            }
            _ => {
                self.halted = true;
            }
        }
    }

    fn set_add_carry(&mut self, a: u32, b: u32, sub: bool) {
        let (result, carry) = if sub {
            let r = (a as i64) - (b as i64);
            let c = a >= b;
            (r as u32, c)
        } else {
            let r = a.wrapping_add(b);
            let c = r < a;
            (r, c)
        };
        let _ = result;
        if carry {
            self.xpsr |= 0x2000_0000;
        } else {
            self.xpsr &= !0x2000_0000;
        }
    }

    fn condition_true(&self, cond: u32) -> bool {
        let n = self.flag_n();
        let z = self.flag_z();
        let c = self.flag_c();
        let v = self.flag_v();
        match cond {
            0 => z,                                  // EQ
            1 => !z,                                 // NE
            2 => c,                                  // CS
            3 => !c,                                 // CC
            4 => n,                                  // MI
            5 => !n,                                 // PL
            6 => v,                                  // VS
            7 => !v,                                 // VC
            8 => c && !z,                            // HI
            9 => !c || z,                            // LS
            10 => n == v,                            // GE
            11 => n != v,                            // LT
            12 => !z && n == v,                      // GT
            13 => z || n != v,                       // LE
            _ => true,
        }
    }

    fn svc(&mut self) {
        // Supervisor call: set IPSR to 11 (SVCall) — the caller observes it.
        self.set_exception(11);
    }

    fn execute_alu(&mut self, alu: u32, rd: usize, rm: usize) {
        let a = self.regs[rd];
        let b = self.regs[rm];
        // Thumb-1 ALU opcodes (Format 4, op = (insn >> 6) & 0xF).
        let (result, write, flags) = match alu {
            0 => (a & b, true, true),            // AND
            1 => (a ^ b, true, true),            // EOR
            2 => {
                // LSL Rd,Rs
                let shift = b & 0xFF;
                let r = if shift == 0 { a } else { a << shift };
                if shift != 0 {
                    self.xpsr = (self.xpsr & !0x2000_0000)
                        | (((a >> (32 - shift)) & 1) << 29);
                }
                (r, true, false)
            }
            3 => {
                // LSR Rd,Rs
                let shift = b & 0xFF;
                let r = if shift == 0 { a } else { a >> shift };
                if shift != 0 {
                    self.xpsr = (self.xpsr & !0x2000_0000)
                        | (((a >> (shift - 1)) & 1) << 29);
                }
                (r, true, false)
            }
            4 => {
                // ASR Rd,Rs
                let shift = b & 0xFF;
                let shift = if shift == 0 { 32 } else { shift };
                let r = ((a as i32) >> shift) as u32;
                if shift != 32 {
                    self.xpsr = (self.xpsr & !0x2000_0000)
                        | (((a >> (shift - 1)) & 1) << 29);
                }
                (r, true, false)
            }
            5 => {
                // ADC Rd,Rd,Rm (with carry)
                let carry = self.flag_c() as u32;
                let (r, c) = a.overflowing_add(b);
                let (r, c2) = r.overflowing_add(carry);
                self.xpsr = (self.xpsr & !0x2000_0000) | ((c || c2) as u32) << 29;
                (r, true, false)
            }
            6 => {
                // SBC Rd,Rd,Rm (with carry)
                let carry = if self.flag_c() { 1 } else { 0 };
                let (r, c) = a.overflowing_sub(b);
                let (r, c2) = r.overflowing_sub(carry);
                self.xpsr = (self.xpsr & !0x2000_0000) | ((c || c2) as u32) << 29;
                (r, true, false)
            }
            7 => {
                // ROR Rd,Rs
                let shift = b & 0x1F;
                let r = if shift == 0 { a } else { a.rotate_right(shift) };
                if shift != 0 {
                    self.xpsr = (self.xpsr & !0x2000_0000)
                        | (((a >> (shift - 1)) & 1) << 29);
                }
                (r, true, false)
            }
            8 => (a & b, false, true),           // TST
            9 => {
                // NEG Rd,0,Rm
                let (r, c) = 0u32.overflowing_sub(b);
                self.xpsr = (self.xpsr & !0x2000_0000) | ((c as u32) << 29);
                (r, true, false)
            }
            10 => {
                // CMP Rd,Rm
                let (r, c) = a.overflowing_sub(b);
                self.xpsr = (self.xpsr & !0x2000_0000) | ((c as u32) << 29);
                (r, false, true)
            }
            11 => {
                // CMN Rd,Rm
                let (r, c) = a.overflowing_add(b);
                self.xpsr = (self.xpsr & !0x2000_0000) | ((c as u32) << 29);
                (r, false, true)
            }
            12 => (a | b, true, true),           // ORR
            13 => {
                // MUL Rd,Rm
                let r = a.wrapping_mul(b);
                (r, true, false)
            }
            14 => (a & !b, true, true),          // BIC
            _ => (!a, true, true),               // MVN
        };
        if flags {
            self.set_nz(result);
        }
        if write {
            self.regs[rd] = result;
        }
    }

    fn execute_wide(&mut self, insn: u32) {
        // Thumb-2 wide encodings. `first` is the high halfword, `second` the
        // low. Only the forms a bare-metal Cortex-M toolchain actually emits
        // are decoded; anything else halts so the caller surfaces it.
        let first = (insn >> 16) as u16;
        let second = insn as u16;
        let pc = self.regs[PC];

        // BL/BLX immediate (0xF000-0xF7FF prefix, second bits 14-12 = 101).
        if first & 0xF800 == 0xF000 && second & 0xD000 == 0xD000 {
            let s = ((first >> 10) & 1) as u32;
            let j1 = ((second >> 13) & 1) as u32;
            let j2 = ((second >> 11) & 1) as u32;
            let imm10 = (first & 0x3FF) as u32;
            let imm11 = (second & 0x7FF) as u32;
            let mut imm32 = (s << 24) | (j2 << 23) | (j1 << 22) | (imm10 << 12) | (imm11 << 1);
            imm32 = sign_extend_thumb(imm32, 25);
            let link = second & 0x4000 != 0; // bit 14: BL vs BLX
            if link {
                self.regs[LR] = (pc | 1) & !1;
            }
            self.regs[PC] = pc.wrapping_add(imm32 as u32) & !1;
            self.set_thumb(true);
            return;
        }

        // MOVW/MOVT (0xF240-0xF2FF, second bits 15-12 = 0100).
        if first & 0xFBF0 == 0xF240 && second & 0xF000 == 0x4000 {
            let rd = ((second >> 8) & 0x0F) as usize;
            let imm16 = ((first & 0x000F) as u32) << 12
                | ((first & 0x0F00) as u32)
                | ((second & 0x00FF) as u32);
            let top = second & 0x0800 != 0; // MOVT when bit 11 set
            if top {
                self.regs[rd] = (self.regs[rd] & 0x0000_FFFF) | (imm16 << 16);
            } else {
                self.regs[rd] = (self.regs[rd] & 0xFFFF_0000) | imm16;
            }
            return;
        }

        // ADDW/SUBW (0xF200-0xF2FF, second bits 14-12 = 010, bit 15 = 0).
        if first & 0xFBF0 == 0xF200 && second & 0xF000 == 0x2000 {
            let rd = ((second >> 8) & 0x0F) as usize;
            let rn = (first & 0x000F) as usize;
            let imm12 = ((first & 0x0F00) as u32) << 4 | ((second & 0x00FF) as u32);
            let sub = second & 0x0400 != 0; // bit 10
            if sub {
                self.regs[rd] = self.regs[rn].wrapping_sub(imm12);
            } else {
                self.regs[rd] = self.regs[rn].wrapping_add(imm12);
            }
            return;
        }

        // B.W unconditional (0xF000 prefix, second bits 15-14 = 10, 13-12 = 10).
        if first & 0xF800 == 0xF000
            && first & 0x0C00 == 0x0800
            && second & 0xD000 == 0x8000
        {
            let s = ((first >> 10) & 1) as u32;
            let j1 = ((second >> 13) & 1) as u32;
            let j2 = ((second >> 11) & 1) as u32;
            let imm10 = (first & 0x3FF) as u32;
            let imm11 = (second & 0x7FF) as u32;
            let mut imm32 = (s << 24) | (j2 << 23) | (j1 << 22) | (imm10 << 12) | (imm11 << 1);
            imm32 = sign_extend_thumb(imm32, 25);
            self.regs[PC] = pc.wrapping_add(imm32 as u32) & !1;
            self.set_thumb(true);
            return;
        }

        // LDR/STR (immediate 12-bit, 0xF8D0-0xF8DF / 0xF8C0-0xF8CF).
        if first & 0xFFF0 == 0xF8D0 || first & 0xFFF0 == 0xF8C0 {
            let load = first & 0x0010 != 0; // LDR when bit 4 of first set
            let rn = (first & 0x000F) as usize;
            let rd = ((second >> 12) & 0x0F) as usize;
            let imm12 = (second & 0x0FFF) as u32;
            let addr = self.regs[rn].wrapping_add(imm12);
            if load {
                self.regs[rd] = self.read_u32(addr);
            } else {
                self.write_u32(addr, self.regs[rd]);
            }
            return;
        }

        // LDRB/STRB (0xF890-0xF89F / 0xF880-0xF88F).
        if first & 0xFFF0 == 0xF890 || first & 0xFFF0 == 0xF880 {
            let load = first & 0x0010 != 0;
            let rn = (first & 0x000F) as usize;
            let rd = ((second >> 12) & 0x0F) as usize;
            let imm12 = (second & 0x0FFF) as u32;
            let addr = self.regs[rn].wrapping_add(imm12);
            if load {
                self.regs[rd] = self.read_u8(addr) as u32;
            } else {
                self.write_u8(addr, self.regs[rd] as u8);
            }
            return;
        }

        // LDRH/STRH (0xF8B0-0xF8BF / 0xF8A0-0xF8AF).
        if first & 0xFFF0 == 0xF8B0 || first & 0xFFF0 == 0xF8A0 {
            let load = first & 0x0010 != 0;
            let rn = (first & 0x000F) as usize;
            let rd = ((second >> 12) & 0x0F) as usize;
            let imm12 = (second & 0x0FFF) as u32;
            let addr = self.regs[rn].wrapping_add(imm12);
            if load {
                self.regs[rd] = self.read_u16(addr) as u32;
            } else {
                self.write_u16(addr, self.regs[rd] as u16);
            }
            return;
        }

        // Memory barriers DMB/DSB/ISB (0xF3BF 0x8F5F/8F6F/8F7F): no-ops here.
        if first == 0xF3BF && second & 0xFFF0 == 0x8F50 {
            return;
        }

        self.halted = true;
    }
}

impl ArmCore {
    /// ARM state (A32) condition-code evaluation.
    fn condition_passed(&self, cond: u32) -> bool {
        match cond {
            0x0 => self.flag_z(),
            0x1 => !self.flag_z(),
            0x2 => self.flag_c(),
            0x3 => !self.flag_c(),
            0x4 => self.flag_n(),
            0x5 => !self.flag_n(),
            0x6 => self.flag_v(),
            0x7 => !self.flag_v(),
            0x8 => self.flag_c() && !self.flag_z(),
            0x9 => !self.flag_c() || self.flag_z(),
            0xA => self.flag_n() == self.flag_v(),
            0xB => self.flag_n() != self.flag_v(),
            0xC => !self.flag_z() && self.flag_n() == self.flag_v(),
            0xD => self.flag_z() || self.flag_n() != self.flag_v(),
            0xE => true,
            // 0xF is the pre-ARMv5 "never" condition.
            _ => false,
        }
    }

    /// Read an ARM-state register, substituting the pipeline-adjusted PC
    /// value for R15 (reads of PC see the instruction address + 8).
    fn arm_reg(&self, index: usize, pc_base: u32) -> u32 {
        if index == PC { pc_base } else { self.regs[index] }
    }

    fn set_nzcv(&mut self, n: bool, z: bool, c: bool, v: bool) {
        let mut value = self.xpsr & 0x0FFF_FFFF;
        if n {
            value |= 0x8000_0000;
        }
        if z {
            value |= 0x4000_0000;
        }
        if c {
            value |= 0x2000_0000;
        }
        if v {
            value |= 0x1000_0000;
        }
        self.xpsr = value;
    }

    /// Branch through a value that may select Thumb state via bit 0 (BX/BLX
    /// and `LDR pc, ...` on ARMv5T).
    fn branch_exchange_value(&mut self, target: u32) {
        let thumb = target & 1 != 0;
        self.arm_state = !thumb;
        self.set_thumb(thumb);
        self.regs[PC] = target & !1;
    }

    fn arm_add(a: u32, b: u32, carry: bool) -> (u32, bool, bool) {
        let sum = a as u64 + b as u64 + carry as u64;
        let result = sum as u32;
        let c = sum > 0xFFFF_FFFF;
        let v = (a ^ result) & (b ^ result) & 0x8000_0000 != 0;
        (result, c, v)
    }

    /// `a - b - !C` (the carry input is the ARM C flag; a set C means no
    /// borrow). Returns `(result, carry_out, overflow)`.
    fn arm_sub(a: u32, b: u32, carry: bool) -> (u32, bool, bool) {
        let borrow = if carry { 0u64 } else { 1u64 };
        let diff = a as u64 as i64 - b as i64 - borrow as i64;
        let result = diff as u32;
        let c = diff >= 0;
        let v = (a ^ b) & (a ^ result) & 0x8000_0000 != 0;
        (result, c, v)
    }

    /// Compute the (shifted) operand 2 and the shifter carry-out.
    fn arm_operand2(&self, insn: u32, pc_base: u32) -> (u32, bool) {
        if insn & (1 << 25) != 0 {
            let imm = insn & 0xFF;
            let rotate = ((insn >> 8) & 0x0F) * 2;
            return (imm.rotate_right(rotate), self.flag_c());
        }
        let rm = (insn & 0x0F) as usize;
        let value = self.arm_reg(rm, pc_base);
        let shift_type = (insn >> 5) & 0x03;
        let (amount, by_register) = if insn & 0x10 != 0 {
            let rs = ((insn >> 8) & 0x0F) as usize;
            (self.arm_reg(rs, pc_base) & 0xFF, true)
        } else {
            ((insn >> 7) & 0x1F, false)
        };
        if !by_register && amount == 0 {
            if shift_type == 3 {
                // RRX: rotate right by one through the carry flag.
                let carry_in = if self.flag_c() { 0x8000_0000 } else { 0 };
                return ((value >> 1) | carry_in, value & 1 != 0);
            }
            return (value, self.flag_c());
        }
        let carry_in = self.flag_c();
        match shift_type {
            0 => shift_lsl(value, amount, carry_in),
            1 => shift_lsr(value, amount, carry_in),
            2 => shift_asr(value, amount, carry_in),
            _ => shift_ror(value, amount, carry_in),
        }
    }

    /// Decode and execute a 32-bit ARM-state (A32) instruction.
    fn execute_arm(&mut self, insn: u32) {
        let cond = insn >> 28;
        if !self.condition_passed(cond) {
            return;
        }
        // PC reads see instruction address + 8; `step` already advanced PC by
        // 4, so this is one word ahead of the current PC register.
        let pc_base = self.regs[PC].wrapping_add(4);

        // Branch and branch-with-link: 101x.
        if insn & 0x0E00_0000 == 0x0A00_0000 {
            let link = insn & (1 << 24) != 0;
            let offset = sign_extend_thumb(insn & 0x00FF_FFFF, 24) << 2;
            let target = pc_base.wrapping_add(offset);
            if link {
                self.regs[LR] = self.regs[PC];
            }
            self.regs[PC] = target & !3;
            return;
        }
        // BX / BLX (register).
        if insn & 0x0FFF_FFF0 == 0x012F_FF10 || insn & 0x0FFF_FFF0 == 0x012F_FF30 {
            let rm = (insn & 0x0F) as usize;
            let target = self.arm_reg(rm, pc_base);
            if insn & 0x20 != 0 {
                self.regs[LR] = self.regs[PC];
            }
            self.branch_exchange_value(target);
            return;
        }
        // Block data transfer: 100x.
        if insn & 0x0E00_0000 == 0x0800_0000 {
            self.arm_block_transfer(insn, pc_base);
            return;
        }
        // Single data transfer (word/byte): 01xx.
        if insn & 0x0C00_0000 == 0x0400_0000 {
            self.arm_single_transfer(insn, pc_base);
            return;
        }
        // MSR (immediate) and MOVW/MOVT live in the data-processing space.
        if insn & 0x0FF0_0000 == 0x0300_0000 || insn & 0x0FF0_0000 == 0x0340_0000 {
            // MOVW (bit 22 clear) / MOVT (bit 22 set).
            let rd = ((insn >> 12) & 0x0F) as usize;
            let imm16 = ((insn >> 4) & 0xF000) | (insn & 0x0FFF);
            if insn & 0x0040_0000 == 0 {
                self.regs[rd] = imm16;
            } else {
                self.regs[rd] = (self.regs[rd] & 0x0000_FFFF) | (imm16 << 16);
            }
            return;
        }
        if insn & 0x0FBF_0FFF == 0x010F_0000 {
            // MRS Rd, CPSR
            let rd = ((insn >> 12) & 0x0F) as usize;
            self.regs[rd] = self.xpsr;
            return;
        }
        if insn & 0x0FB0_FFF0 == 0x0120_F000 {
            // MSR CPSR_<fields>, Rm
            let rm = (insn & 0x0F) as usize;
            let fields = (insn >> 16) & 0x0F;
            self.arm_msr(fields, self.arm_reg(rm, pc_base));
            return;
        }
        if insn & 0x0FB0_F000 == 0x0320_F000 {
            // MSR CPSR_<fields>, #imm
            let fields = (insn >> 16) & 0x0F;
            let imm = (insn & 0xFF).rotate_right(((insn >> 8) & 0x0F) * 2);
            self.arm_msr(fields, imm);
            return;
        }
        // Multiply / multiply-accumulate: bits 27-22 = 000000, bits 7-4 = 1001.
        if insn & 0x0FC0_00F0 == 0x0000_0090 {
            let accumulate = insn & (1 << 21) != 0;
            let set_flags = insn & (1 << 20) != 0;
            let rd = ((insn >> 16) & 0x0F) as usize;
            let rn = ((insn >> 12) & 0x0F) as usize;
            let rs = ((insn >> 8) & 0x0F) as usize;
            let rm = (insn & 0x0F) as usize;
            let mut result = self.arm_reg(rm, pc_base).wrapping_mul(self.arm_reg(rs, pc_base));
            if accumulate {
                result = result.wrapping_add(self.arm_reg(rn, pc_base));
            }
            if set_flags {
                self.set_nz(result);
            }
            self.regs[rd] = result;
            return;
        }
        // Long multiply: 00001 U A S.
        if insn & 0x0F80_00F0 == 0x0080_0090 {
            let signed = insn & (1 << 22) != 0;
            let accumulate = insn & (1 << 21) != 0;
            let set_flags = insn & (1 << 20) != 0;
            let rdhi = ((insn >> 16) & 0x0F) as usize;
            let rdlo = ((insn >> 12) & 0x0F) as usize;
            let rs = ((insn >> 8) & 0x0F) as usize;
            let rm = (insn & 0x0F) as usize;
            let (hi, lo) = if signed {
                let product = self.arm_reg(rm, pc_base) as i32 as i64
                    * self.arm_reg(rs, pc_base) as i32 as i64;
                ((product >> 32) as u32, product as u32)
            } else {
                let product =
                    self.arm_reg(rm, pc_base) as u64 * self.arm_reg(rs, pc_base) as u64;
                ((product >> 32) as u32, product as u32)
            };
            let (mut hi, mut lo) = (hi, lo);
            if accumulate {
                let sum = ((self.regs[rdhi] as u64) << 32)
                    .wrapping_add(self.regs[rdlo] as u64)
                    .wrapping_add(((hi as u64) << 32) | lo as u64);
                hi = (sum >> 32) as u32;
                lo = sum as u32;
            }
            self.regs[rdlo] = lo;
            self.regs[rdhi] = hi;
            if set_flags {
                self.set_nzcv(hi & 0x8000_0000 != 0, hi == 0 && lo == 0, self.flag_c(), self.flag_v());
            }
            return;
        }
        // Halfword / signed transfer: 000 P U I W L, bit 7 = 1, bit 4 = 1.
        if insn & 0x0E00_0090 == 0x0000_0090 && insn & 0x60 != 0 {
            self.arm_halfword_transfer(insn, pc_base);
            return;
        }
        // CLZ Rd, Rm (0x016F0F10).
        if insn & 0x0FFF_0FF0 == 0x016F_0F10 {
            let rd = ((insn >> 12) & 0x0F) as usize;
            let rm = (insn & 0x0F) as usize;
            self.regs[rd] = self.arm_reg(rm, pc_base).leading_zeros();
            return;
        }
        // Supervisor call.
        if insn & 0x0F00_0000 == 0x0F00_0000 {
            self.set_exception(11);
            self.halted = true;
            return;
        }
        // Data processing (register/immediate) with optional flag update.
        if insn & 0x0C00_0000 == 0 {
            self.arm_data_processing(insn, pc_base);
            return;
        }
        // SWP/SWPB (single word swap) is rare in compiler output; halt
        // honestly rather than mis-executing.
        self.halted = true;
    }

    fn arm_msr(&mut self, fields: u32, value: u32) {
        // Only the application/status flag byte (field mask 8) is modelled; the
        // processor mode/control byte is kept as-is.
        if fields & 0x8 != 0 {
            self.set_nzcv(
                value & 0x8000_0000 != 0,
                value & 0x4000_0000 != 0,
                value & 0x2000_0000 != 0,
                value & 0x1000_0000 != 0,
            );
        }
    }

    fn arm_data_processing(&mut self, insn: u32, pc_base: u32) {
        let opcode = (insn >> 21) & 0x0F;
        let set_flags = insn & (1 << 20) != 0;
        let rn = ((insn >> 16) & 0x0F) as usize;
        let rd = ((insn >> 12) & 0x0F) as usize;
        let (b, shifter_carry) = self.arm_operand2(insn, pc_base);
        let a = self.arm_reg(rn, pc_base);
        let compare_only = (8..=11).contains(&opcode);
        let mut flags: Option<(bool, bool, bool, bool)> = None;
        let result = match opcode {
            0 => a & b,                              // AND
            1 => a ^ b,                              // EOR
            2 | 3 => {
                let (r, c, v) = if opcode == 2 {
                    Self::arm_sub(a, b, true)
                } else {
                    Self::arm_sub(b, a, true)
                };
                flags = Some((r & 0x8000_0000 != 0, r == 0, c, v));
                r
            }
            4 | 5 | 6 | 7 => {
                let (r, c, v) = match opcode {
                    4 => Self::arm_add(a, b, false),
                    5 => Self::arm_add(a, b, self.flag_c()),
                    6 => Self::arm_sub(a, b, self.flag_c()),
                    _ => Self::arm_sub(b, a, self.flag_c()),
                };
                flags = Some((r & 0x8000_0000 != 0, r == 0, c, v));
                r
            }
            8 => {
                // TST
                let r = a & b;
                flags = Some((r & 0x8000_0000 != 0, r == 0, shifter_carry, self.flag_v()));
                r
            }
            9 => {
                // TEQ
                let r = a ^ b;
                flags = Some((r & 0x8000_0000 != 0, r == 0, shifter_carry, self.flag_v()));
                r
            }
            10 => {
                // CMP
                let (r, c, v) = Self::arm_sub(a, b, true);
                flags = Some((r & 0x8000_0000 != 0, r == 0, c, v));
                r
            }
            11 => {
                // CMN
                let (r, c, v) = Self::arm_add(a, b, false);
                flags = Some((r & 0x8000_0000 != 0, r == 0, c, v));
                r
            }
            12 => a | b,                            // ORR
            13 => b,                                // MOV
            14 => a & !b,                           // BIC
            _ => !b,                                // MVN
        };
        if (set_flags || compare_only) && flags.is_none() {
            // Logical operations only update N/Z from the result, C from the
            // shifter and leave V untouched.
            flags = Some((result & 0x8000_0000 != 0, result == 0, shifter_carry, self.flag_v()));
        }
        if set_flags || compare_only {
            if let Some((n, z, c, v)) = flags {
                self.set_nzcv(n, z, c, v);
            }
        }
        if !compare_only {
            if rd == PC {
                self.branch_exchange_value(result);
            } else {
                self.regs[rd] = result;
            }
        }
    }

    fn arm_single_transfer(&mut self, insn: u32, pc_base: u32) {
        let register_offset = insn & (1 << 25) != 0;
        let pre_index = insn & (1 << 24) != 0;
        let add = insn & (1 << 23) != 0;
        let byte = insn & (1 << 22) != 0;
        let writeback = insn & (1 << 21) != 0;
        let load = insn & (1 << 20) != 0;
        let rn = ((insn >> 16) & 0x0F) as usize;
        let rd = ((insn >> 12) & 0x0F) as usize;
        let offset = if register_offset {
            let rm = (insn & 0x0F) as usize;
            let (value, _) = self.arm_operand2(insn & !(1 << 25), pc_base);
            let _ = rm;
            value
        } else {
            insn & 0x0FFF
        };
        let base = self.arm_reg(rn, pc_base);
        let address = if add { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
        let effective = if pre_index { address } else { base };
        if load {
            let value = if byte {
                self.read_u8(effective) as u32
            } else {
                self.read_u32(effective & !3)
            };
            if rd == PC {
                self.branch_exchange_value(value);
            } else {
                self.regs[rd] = value;
            }
        } else {
            let value = self.arm_reg(rd, pc_base);
            if byte {
                self.write_u8(effective, value as u8);
            } else {
                self.write_u32(effective & !3, value);
            }
        }
        if (!pre_index || writeback) && rn != PC {
            self.regs[rn] = if add {
                base.wrapping_add(offset)
            } else {
                base.wrapping_sub(offset)
            };
        }
    }

    fn arm_halfword_transfer(&mut self, insn: u32, pc_base: u32) {
        let pre_index = insn & (1 << 24) != 0;
        let add = insn & (1 << 23) != 0;
        let immediate = insn & (1 << 22) != 0;
        let writeback = insn & (1 << 21) != 0;
        let load = insn & (1 << 20) != 0;
        let rn = ((insn >> 16) & 0x0F) as usize;
        let rd = ((insn >> 12) & 0x0F) as usize;
        let sh = (insn >> 5) & 0x03;
        let offset = if immediate {
            ((insn >> 4) & 0xF0) | (insn & 0x0F)
        } else {
            self.arm_reg((insn & 0x0F) as usize, pc_base)
        };
        let base = self.arm_reg(rn, pc_base);
        let address = if add { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
        let effective = if pre_index { address } else { base };
        if load {
            let value = match sh {
                1 => self.read_u16(effective) as u32,
                2 => self.read_u8(effective) as i8 as i32 as u32,
                _ => self.read_u16(effective) as i16 as i32 as u32,
            };
            self.regs[rd] = value;
        } else {
            let value = self.arm_reg(rd, pc_base);
            match sh {
                1 => self.write_u16(effective, value as u16),
                2 => self.write_u8(effective, value as u8),
                _ => self.write_u16(effective, value as u16),
            }
        }
        if (!pre_index || writeback) && rn != PC {
            self.regs[rn] = if add {
                base.wrapping_add(offset)
            } else {
                base.wrapping_sub(offset)
            };
        }
    }

    fn arm_block_transfer(&mut self, insn: u32, pc_base: u32) {
        let pre_index = insn & (1 << 24) != 0;
        let add = insn & (1 << 23) != 0;
        let writeback = insn & (1 << 21) != 0;
        let load = insn & (1 << 20) != 0;
        let rn = ((insn >> 16) & 0x0F) as usize;
        let list = insn & 0xFFFF;
        let count = list.count_ones();
        if count == 0 {
            return;
        }
        let base = self.arm_reg(rn, pc_base);
        let start = match (add, pre_index) {
            (true, false) => base,                          // IA
            (true, true) => base.wrapping_add(4),           // IB
            (false, false) => base.wrapping_sub(4 * count).wrapping_add(4), // DA (reverse)
            (false, true) => base.wrapping_sub(4 * count),  // DB
        };
        let mut address = start;
        let writeback_value = if add {
            base.wrapping_add(4 * count)
        } else {
            base.wrapping_sub(4 * count)
        };
        if !load {
            for index in 0..16 {
                if list & (1 << index) == 0 {
                    continue;
                }
                let value = self.arm_reg(index, pc_base);
                self.write_u32(address & !3, value);
                address = address.wrapping_add(4);
            }
            if writeback && rn != PC {
                self.regs[rn] = writeback_value;
            }
            return;
        }
        for index in 0..16 {
            if list & (1 << index) == 0 {
                continue;
            }
            let value = self.read_u32(address & !3);
            if index == PC {
                self.branch_exchange_value(value);
            } else {
                self.regs[index] = value;
            }
            address = address.wrapping_add(4);
        }
        if writeback && rn != PC && list & (1 << rn) == 0 {
            self.regs[rn] = writeback_value;
        }
    }
}

fn shift_lsl(value: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    if amount == 0 {
        (value, carry_in)
    } else if amount < 32 {
        (value << amount, (value >> (32 - amount)) & 1 != 0)
    } else if amount == 32 {
        (0, value & 1 != 0)
    } else {
        (0, false)
    }
}

fn shift_lsr(value: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    if amount == 0 {
        (value, carry_in)
    } else if amount < 32 {
        (value >> amount, (value >> (amount - 1)) & 1 != 0)
    } else if amount == 32 {
        (0, value >> 31 != 0)
    } else {
        (0, false)
    }
}

fn shift_asr(value: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    if amount == 0 {
        (value, carry_in)
    } else if amount < 32 {
        (
            ((value as i32) >> amount) as u32,
            (value >> (amount - 1)) & 1 != 0,
        )
    } else if value & 0x8000_0000 != 0 {
        (0xFFFF_FFFF, true)
    } else {
        (0, false)
    }
}

fn shift_ror(value: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    let n = amount % 32;
    if amount == 0 {
        (value, carry_in)
    } else if n == 0 {
        (value, value >> 31 != 0)
    } else {
        let result = value.rotate_right(n);
        (result, result >> 31 != 0)
    }
}

fn sign_extend_thumb(value: u32, bits: u32) -> u32 {
    let shift = 32 - bits;
    ((value << shift) as i32 >> shift) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> ArmCore {
        ArmCore::new()
    }

    fn run(core: &mut ArmCore, insns: &[u16], steps: usize) {
        for (i, insn) in insns.iter().enumerate() {
            core.write_u16((i * 2) as u32, *insn);
        }
        core.regs[PC] = 0;
        core.set_thumb(true);
        for _ in 0..steps {
            let r = core.step(&[]);
            if r == ArmStep::Halted {
                break;
            }
        }
    }

    #[test]
    fn mov_immediate_sets_register() {
        let mut c = core();
        // MOVS r0, #5  (0x2005)
        run(&mut c, &[0x2005], 1);
        assert_eq!(c.regs[0], 5);
    }

    #[test]
    fn add_immediate_8bit() {
        let mut c = core();
        c.regs[0] = 10;
        // ADDS r0, r0, #7  (0x3007)
        run(&mut c, &[0x3007], 1);
        assert_eq!(c.regs[0], 17);
    }

    #[test]
    fn sub_immediate_8bit_sets_flags() {
        let mut c = core();
        c.regs[0] = 3;
        // SUBS r0, r0, #3  (0x3803)
        run(&mut c, &[0x3803], 1);
        assert_eq!(c.regs[0], 0);
        assert!(c.flag_z(), "zero flag set");
        assert!(c.flag_c(), "borrow sets carry (no borrow => carry)");
    }

    #[test]
    fn alu_operations() {
        let mut c = core();
        c.regs[0] = 0b1100;
        c.regs[1] = 0b1010;
        // ANDS r0, r1  (0x4008)
        run(&mut c, &[0x4008], 1);
        assert_eq!(c.regs[0], 0b1000);
    }

    #[test]
    fn load_store_word() {
        let mut c = core();
        c.regs[0] = 0x2000_0000;
        c.regs[1] = 0xDEAD_BEEF;
        // STR r1, [r0, #0]  (0x6009)
        // LDR r2, [r0, #0]  (0x6812)
        run(&mut c, &[0x6009, 0x6812], 2);
        assert_eq!(c.regs[2], 0xDEAD_BEEF);
    }

    #[test]
    fn push_pop_restores_stack() {
        let mut c = core();
        c.regs[SP] = 0x2000_1000;
        c.regs[0] = 0x1111_1111;
        c.regs[1] = 0x2222_2222;
        // PUSH {r0, r1}  (0xB403)
        // POP {r0, r1}   (0xBC03)
        run(&mut c, &[0xB403, 0xBC03], 2);
        assert_eq!(c.regs[0], 0x1111_1111);
        assert_eq!(c.regs[1], 0x2222_2222);
        assert_eq!(c.regs[SP], 0x2000_1000);
    }

    #[test]
    fn conditional_branch_taken() {
        let mut c = core();
        c.regs[0] = 0;
        // MOVS r0, #0 (0x2000); BEQ +2 (0xD000): EQ is cond 0, offset 0
        // The BEQ with offset 0 loops; use a branch forward.
        // BEQ .+4: 0xD001 (offset 1 halfword = 2 bytes)
        run(&mut c, &[0x2000, 0xD001, 0x2000], 2);
        assert_eq!(c.regs[0], 0);
    }

    #[test]
    fn unconditional_branch() {
        let mut c = core();
        // B .+4: 0xE001 (offset 1 halfword)
        run(&mut c, &[0xE001, 0xE000], 1);
        assert_eq!(c.regs[PC], 4);
    }

    #[test]
    fn bl_sets_link_register() {
        let mut c = core();
        // BL with offset 0: first=0xF000, second=0xD000 (link set)
        c.write_u16(0, 0xF000);
        c.write_u16(2, 0xD000);
        c.regs[PC] = 0;
        c.set_thumb(true);
        let r = c.step(&[]);
        assert_eq!(r, ArmStep::Advanced);
        assert_eq!(c.regs[LR], 4);
        assert_eq!(c.regs[PC], 4);
    }

    #[test]
    fn reset_from_vector_table() {
        let mut c = core();
        c.write_u32(0, 0x2000_0000); // initial SP
        c.write_u32(4, 0x0000_0009); // reset vector | 1 (Thumb)
        c.reset_from_vector_table(0, 0x0000_0008);
        assert_eq!(c.regs[SP], 0x2000_0000);
        assert_eq!(c.regs[PC], 0x0000_0008);
        assert!(c.thumb());
    }

    #[test]
    fn memory_barrier_is_nop() {
        let mut c = core();
        // DMB: 0xF3BF 0x8F5F
        c.write_u16(0, 0xF3BF);
        c.write_u16(2, 0x8F5F);
        c.regs[PC] = 0;
        c.set_thumb(true);
        let r = c.step(&[]);
        assert_eq!(r, ArmStep::Advanced);
        assert_eq!(c.regs[PC], 4);
    }

    #[test]
    fn movw_wide_loads_immediate() {
        let mut c = core();
        // MOVW r0, #0x1234: first=0xF241 (imm4=1), second=0x4034
        c.write_u16(0, 0xF241);
        c.write_u16(2, 0x4034);
        c.regs[PC] = 0;
        c.set_thumb(true);
        let r = c.step(&[]);
        assert_eq!(r, ArmStep::Advanced);
        assert_eq!(c.regs[0], 0x1234);
    }

    // -------- ARM state (A32) --------

    fn run_arm(core: &mut ArmCore, insns: &[u32], steps: usize) {
        for (i, insn) in insns.iter().enumerate() {
            core.write_u32((i * 4) as u32, *insn);
        }
        core.regs[PC] = 0;
        core.set_thumb(false);
        core.arm_state = true;
        for _ in 0..steps {
            let r = core.step(&[]);
            if r == ArmStep::Halted {
                break;
            }
        }
    }

    #[test]
    fn arm_mov_immediate_and_add_register() {
        let mut c = core();
        // MOV r0, #5 (0xE3A00005); ADD r0, r0, #7 (0xE2800007)
        run_arm(&mut c, &[0xE3A0_0005, 0xE280_0007], 2);
        assert_eq!(c.regs[0], 12);
        assert_eq!(c.regs[PC], 8);
    }

    #[test]
    fn arm_a32_breakpoint_stops_before_execute() {
        let mut c = core();
        // MOV r0, #5 (0xE3A00005); ADD r0, r0, #7 (0xE2800007)
        c.write_u32(0, 0xE3A0_0005);
        c.write_u32(4, 0xE280_0007);
        c.regs[PC] = 0;
        c.set_thumb(false);
        c.arm_state = true;
        // Breakpoint at the ADD (address 4); the first MOV executes, then stop.
        assert_eq!(c.step(&[4]), ArmStep::Advanced);
        assert_eq!(c.regs[0], 5);
        assert_eq!(c.step(&[4]), ArmStep::Breakpoint);
        assert_eq!(c.regs[PC], 4, "breakpoint stops before the ADD executes");
        assert_eq!(c.regs[0], 5, "ADD must not have run");
    }

    #[test]
    fn arm_data_processing_flags_and_conditional_execution() {
        let mut c = core();
        // MOV r0, #3; CMP r0, #3; MOVEQ r1, #7 (EQ executes)
        run_arm(
            &mut c,
            &[0xE3A0_0003, 0xE350_0003, 0x03A0_1007],
            3,
        );
        assert!(c.flag_z(), "CMP should set Z");
        assert_eq!(c.regs[1], 7);
    }

    #[test]
    fn arm_conditional_instruction_skipped() {
        let mut c = core();
        // MOV r0, #1; CMP r0, #1 (Z=1); MOVNE r1, #9 must not run
        run_arm(&mut c, &[0xE3A0_0001, 0xE350_0001, 0x13A0_1009], 3);
        assert_eq!(c.regs[1], 0);
    }

    #[test]
    fn arm_branch_and_link() {
        let mut c = core();
        // BL .+4: target = pc_base(8) + 0, LR = 4
        run_arm(&mut c, &[0xEB00_0000], 1);
        assert_eq!(c.regs[LR], 4);
        assert_eq!(c.regs[PC], 8);
    }

    #[test]
    fn arm_branch_backward() {
        let mut c = core();
        // B at 0x100 back to 0: offset = 0 - (0x100 + 8) = -0x108, imm24 = -0x42
        c.write_u32(0x100, 0xEAFF_FFBE);
        c.regs[PC] = 0x100;
        c.set_thumb(false);
        c.arm_state = true;
        let r = c.step(&[]);
        assert_eq!(r, ArmStep::Advanced);
        assert_eq!(c.regs[PC], 0);
    }

    #[test]
    fn arm_load_store_word_and_byte() {
        let mut c = core();
        c.regs[1] = 0x2000_0100;
        c.regs[2] = 0xDEAD_BEEF;
        // STR r2, [r1, #4] (0xE5812004); LDR r3, [r1, #4] (0xE5913004)
        run_arm(&mut c, &[0xE581_2004, 0xE591_3004], 2);
        assert_eq!(c.regs[3], 0xDEAD_BEEF);
        // MOV r4, #0xAB; STRB r4, [r1, #8] (0xE5C14008); LDRB r5, [r1, #8]
        run_arm(&mut c, &[0xE3A0_40AB, 0xE5C1_4008, 0xE5D1_5008], 3);
        assert_eq!(c.regs[5], 0xAB);
    }

    #[test]
    fn arm_halfword_transfer() {
        let mut c = core();
        c.regs[1] = 0x2000_0200;
        c.regs[2] = 0x1234_5678;
        // STRH r2, [r1, #2] (0xE1C120B2); LDRH r3, [r1, #2] (0xE1D130B2)
        run_arm(&mut c, &[0xE1C1_20B2, 0xE1D1_30B2], 2);
        assert_eq!(c.regs[3], 0x5678);
    }

    #[test]
    fn arm_block_transfer() {
        let mut c = core();
        c.regs[0] = 0x2000_0300;
        c.regs[1] = 0x1111_1111;
        c.regs[2] = 0x2222_2222;
        // STMIA r0, {r1,r2} (0xE8800006); LDMIA r0, {r3,r4} (0xE8900018)
        run_arm(&mut c, &[0xE880_0006, 0xE890_0018], 2);
        assert_eq!(c.regs[3], 0x1111_1111);
        assert_eq!(c.regs[4], 0x2222_2222);
    }

    #[test]
    fn arm_bx_switches_to_thumb() {
        let mut c = core();
        c.regs[0] = 0x0000_0021;
        // BX r0 (0xE12FFF10)
        run_arm(&mut c, &[0xE12F_FF10], 1);
        assert_eq!(c.regs[PC], 0x20);
        assert!(c.thumb());
        assert!(!c.arm_state);
    }

    #[test]
    fn arm_multiply_accumulate() {
        let mut c = core();
        c.regs[1] = 6;
        c.regs[2] = 7;
        c.regs[3] = 100;
        // MLA r0, r1, r2, r3 (0xE0203291)
        run_arm(&mut c, &[0xE020_3291], 1);
        assert_eq!(c.regs[0], 142);
    }

    #[test]
    fn arm_movw_movt_build_constant() {
        let mut c = core();
        // MOVW r0, #0x5678 (0xE3050678); MOVT r0, #0x1234 (0xE3410234)
        run_arm(&mut c, &[0xE305_0678, 0xE341_0234], 2);
        assert_eq!(c.regs[0], 0x1234_5678);
    }

    #[test]
    fn arm_reset_vector_selects_arm_state() {
        let mut c = core();
        c.write_u32(0, 0x2000_0000);
        c.write_u32(4, 0x0000_0100); // bit 0 clear => ARM state
        c.reset_from_vector_table(0, 0);
        assert_eq!(c.regs[PC], 0x0100);
        assert!(!c.thumb());
        assert!(c.arm_state);
    }

    #[test]
    fn arm7_code_vector_resets_to_arm_state_entry() {
        let mut c = core();
        // ARM7TDMI vector table: word0 is a B instruction (0xEA000000),
        // word1 is the undefined-instruction vector (also code).
        c.write_u32(0, 0xEA00_0000);
        c.write_u32(4, 0xEA00_0004);
        // ELF entry 0x0000_0100 (bit 0 clear => ARM state).
        c.reset_from_vector_table(0, 0x0000_0100);
        assert_eq!(c.regs[PC], 0x0100);
        assert!(c.arm_state);
        assert!(!c.thumb());
        assert_eq!(c.regs[SP], 0x2000_0000);
    }

    #[test]
    fn cortex_m_data_vector_resets_sp_and_pc() {
        let mut c = core();
        c.write_u32(0, 0x2000_0000); // initial SP
        c.write_u32(4, 0x0800_0009); // reset PC | 1 (Thumb)
        c.reset_from_vector_table(0, 0x0800_0008);
        assert_eq!(c.regs[SP], 0x2000_0000);
        assert_eq!(c.regs[PC], 0x0800_0008);
        assert!(c.thumb());
        assert!(!c.arm_state);
    }
}
