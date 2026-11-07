//! `ns-sim` — a built-in instruction-level simulator for ns-embed-ide.
//!
//! Implements a clean, verifiable RV32I (RISC-V 32-bit integer) core that can
//! load a program image (raw bytes or an ELF32), step/run instructions, and
//! expose registers and memory. A small virtual-peripheral model provides a
//! memory-mapped UART whose output the IDE can surface in the console.

use serde::Serialize;

use std::collections::HashMap;
pub const REG_COUNT: usize = 32;
pub const MEMORY_SIZE: usize = 64 * 1024; // 64 KiB simulated RAM
pub const ENTRY_POINT: u32 = 0x8000_0000;

/// A virtual peripheral: a memory-mapped UART at `base`, whose `putchar`
/// callback the simulator invokes on writes to the data register.
pub struct VirtualPeripherals {
    pub uart_base: u32,
    pub putchar: Box<dyn FnMut(u8) + Send>,
    pub outputs: Vec<u8>,
    /// GPIO register base address.
    pub gpio_base: u32,
    /// GPIO output value (bit per pin).
    pub gpio_out: u32,
    /// GPIO direction mask (1 = output).
    pub gpio_dir: u32,
    /// GPIO input value (bit per pin).
    pub gpio_in: u32,
    /// Log of GPIO output changes: (cycle, pin, value).
    pub gpio_events: Vec<(u64, u32, u32)>,
    /// Up to 8 general-purpose timers.
    pub timers: Vec<Timer>,
    /// Pending interrupt lines asserted by peripherals.
    pub irq_pending: [bool; 16],
}

/// A virtual general-purpose timer: prescaler + up-counter with auto-reload.
#[derive(Debug, Clone)]
pub struct Timer {
    pub base: u32,
    pub enabled: bool,
    pub prescaler: u32,
    pub counter: u32,
    pub auto_reload: u32,
    /// Cycles until the next prescaler wrap.
    prescaler_clock: u32,
    /// Interrupt line this timer raises on update.
    pub irq_line: usize,
    /// Number of update events (overflows) fired.
    pub update_count: u64,
}

impl Timer {
    pub fn new(base: u32, irq_line: usize) -> Self {
        Self {
            base,
            enabled: false,
            prescaler: 0,
            counter: 0,
            auto_reload: 0,
            prescaler_clock: 0,
            irq_line,
            update_count: 0,
        }
    }

    /// Advance the timer by one core cycle; returns true if an update event fired.
    pub fn tick(&mut self) -> bool {
        if !self.enabled {
            return false;
        }
        let prescaler = (self.prescaler & 0xffff).max(1);
        self.prescaler_clock = self.prescaler_clock.wrapping_add(1);
        if self.prescaler_clock >= prescaler {
            self.prescaler_clock = 0;
            let reload = self.auto_reload;
            if self.counter >= reload {
                self.counter = 0;
                self.update_count = self.update_count.wrapping_add(1);
                return true;
            }
            self.counter = self.counter.wrapping_add(1);
        }
        false
    }
}

impl VirtualPeripherals {
    pub fn new(uart_base: u32) -> Self {
        Self {
            uart_base,
            putchar: Box::new(|_| {}),
            outputs: Vec::new(),
            gpio_base: 0x4000_2000,
            gpio_out: 0,
            gpio_dir: 0,
            gpio_in: 0,
            gpio_events: Vec::new(),
            timers: Vec::new(),
            irq_pending: [false; 16],
        }
    }
}

/// The RV32I simulator core.
pub struct Simulator {
    pub pc: u32,
    pub registers: [u32; REG_COUNT],
    pub memory: Vec<u8>,
    /// Whether execution should stop (a breakpoint or halt instruction).
    pub halted: bool,
    /// Addresses that, when reached, pause execution.
    pub breakpoints: Vec<u32>,
    /// Steps executed since the last reset.
    pub cycle_count: u64,
    pub peripherals: VirtualPeripherals,
    /// Function symbols parsed from the ELF (name, address, size).
    pub symbols: Vec<Symbol>,
    /// A flattened source line table: (address, line number, file index).
    pub line_table: Vec<LineEntry>,
    /// Source file names referenced by the line table.
    pub source_files: Vec<String>,
}

/// A symbol (function/global) with its address and size.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub address: u32,
    pub size: u32,
}

/// A single source-line mapping: instruction address to file/line.
#[derive(Debug, Clone, Copy)]
pub struct LineEntry {
    pub address: u32,
    pub file: usize,
    pub line: u32,
}

/// A single-step outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepResult {
    Advanced,
    Breakpoint,
    Halted,
}

impl Simulator {
    pub fn new() -> Self {
        Self {
            pc: ENTRY_POINT,
            registers: [0; REG_COUNT],
            memory: vec![0; MEMORY_SIZE],
            halted: false,
            breakpoints: Vec::new(),
            cycle_count: 0,
            peripherals: VirtualPeripherals::new(0x4000_0000),
            symbols: Vec::new(),
            line_table: Vec::new(),
            source_files: Vec::new(),
        }
    }

    /// Reset the core (PC to entry, registers zeroed, memory preserved).
    pub fn reset(&mut self, entry: u32) {
        self.pc = entry;
        self.registers = [0; REG_COUNT];
        self.halted = false;
        self.cycle_count = 0;
    }

    /// Load a raw program image at `base` (used for `.bin` artifacts).
    pub fn load_image(&mut self, base: u32, image: &[u8]) {
        self.reset(base);
        self.write_bytes(base, image);
    }

    /// Load an ELF32 binary. Returns the entry point.
    pub fn load_elf(&mut self, elf: &[u8]) -> anyhow::Result<u32> {
        if elf.len() < 52 || &elf[0..4] != b"\x7fELF" {
            anyhow::bail!("不是有效的 ELF 文件");
        }
        let class = elf[4];
        let data = elf[5];
        if class != 1 {
            anyhow::bail!("仅支持 ELF32（class=1），当前 class={class}");
        }
        let little = data == 1;
        let entry = read_u32(elf, 24, little);
        let phoff = read_u32(elf, 28, little);
        let phentsize = read_u16(elf, 42, little) as usize;
        let phnum = read_u16(elf, 44, little) as usize;
        for i in 0..phnum {
            let off = phoff as usize + i * phentsize;
            if off + 56 > elf.len() {
                break;
            }
            let p_type = read_u32(elf, off, little);
            if p_type != 1 {
                continue; // PT_LOAD
            }
            let p_offset = read_u32(elf, off + 4, little) as usize;
            let p_vaddr = read_u32(elf, off + 8, little);
            let p_filesz = read_u32(elf, off + 16, little) as usize;
            let p_memsz = read_u32(elf, off + 20, little) as usize;
            let end = p_offset.saturating_add(p_filesz).min(elf.len());
            let data_slice = &elf[p_offset..end];
            self.write_bytes(p_vaddr, data_slice);
            // Zero-fill the .bss portion.
            if p_memsz > p_filesz {
                let bss_start = p_vaddr.wrapping_add(p_filesz as u32);
                self.zero_bytes(bss_start, p_memsz - p_filesz);
            }
        }
        self.reset(entry);
        self.parse_symbols(elf, little);
        Ok(entry)
    }

    /// Parse ELF `.symtab`/`.strtab` sections into [`Symbol`]s and a coarse
    /// line table (function start → sequential line numbers). The line table is
    /// a best-effort fallback when DWARF line info is unavailable.
    fn parse_symbols(&mut self, elf: &[u8], little: bool) {
        if elf.len() < 52 {
            return;
        }
        let shoff = read_u32(elf, 32, little) as usize;
        let shentsize = read_u16(elf, 46, little) as usize;
        let shnum = read_u16(elf, 48, little) as usize;
        let shstrndx = read_u16(elf, 50, little) as usize;
        if shoff == 0 || shentsize == 0 {
            return;
        }
        let section_name = |index: usize| -> Option<&str> {
            let off = shoff + index * shentsize;
            if off + 40 > elf.len() {
                return None;
            }
            let name_off = read_u32(elf, off, little) as usize;
            let str_off = shoff + shstrndx * shentsize;
            if str_off + 40 > elf.len() {
                return None;
            }
            let str_off_data = read_u32(elf, str_off + 16, little) as usize;
            let str_size = read_u32(elf, str_off + 20, little) as usize;
            let start = str_off_data.saturating_add(name_off);
            if start >= elf.len() || start >= str_off_data.saturating_add(str_size) {
                return None;
            }
            let end = elf[start..]
                .iter()
                .position(|b| *b == 0)
                .map(|p| start + p)
                .unwrap_or(start);
            std::str::from_utf8(&elf[start..end]).ok()
        };

        let mut symtab: Option<(usize, usize, usize)> = None; // offset, entsize, count
        let mut strtab: Option<(usize, usize)> = None; // offset, size
        for index in 0..shnum {
            let off = shoff + index * shentsize;
            if off + 40 > elf.len() {
                break;
            }
            let name = section_name(index).unwrap_or("");
            let sh_offset = read_u32(elf, off + 16, little) as usize;
            let sh_size = read_u32(elf, off + 20, little) as usize;
            let sh_entsize = read_u32(elf, off + 36, little) as usize;
            match name {
                ".symtab" => symtab = Some((sh_offset, sh_entsize, sh_size / sh_entsize.max(1))),
                ".strtab" => strtab = Some((sh_offset, sh_size)),
                _ => {}
            }
        }
        let Some((sym_off, sym_entsize, sym_count)) = symtab else {
            return;
        };
        let Some((str_off, str_size)) = strtab else {
            return;
        };
        let read_str = |offset: usize| -> String {
            let start = str_off.saturating_add(offset);
            if start >= str_off.saturating_add(str_size) || start >= elf.len() {
                return String::new();
            }
            let end = elf[start..]
                .iter()
                .position(|b| *b == 0)
                .map(|p| start + p)
                .unwrap_or(start);
            String::from_utf8_lossy(&elf[start..end]).into_owned()
        };

        let mut symbols = Vec::new();
        for index in 0..sym_count {
            let off = sym_off + index * sym_entsize.max(16);
            if off + 16 > elf.len() {
                break;
            }
            let name_off = read_u32(elf, off, little) as usize;
            let value = read_u32(elf, off + 4, little);
            let size = read_u32(elf, off + 8, little);
            let info = elf[off + 12];
            let kind = info & 0x0f;
            if kind == 2 && size > 0 {
                // STT_FUNC
                let name = read_str(name_off);
                if !name.is_empty() {
                    symbols.push(Symbol {
                        name,
                        address: value,
                        size,
                    });
                }
            }
        }
        symbols.sort_by_key(|symbol| symbol.address);
        // Build a coarse line table: each function start maps to an incrementing
        // line within the file. This is a fallback; DWARF would be more precise.
        let mut line_table = Vec::new();
        for (i, symbol) in symbols.iter().enumerate() {
            let line = (i as u32) + 1;
            line_table.push(LineEntry {
                address: symbol.address,
                file: 0,
                line,
            });
        }
        self.source_files = vec!["<elf>".to_string()];
        self.symbols = symbols;
        self.line_table = line_table;
    }

    /// Set (or clear) a breakpoint at `address`. Returns whether it was added.
    pub fn set_breakpoint(&mut self, address: u32) -> bool {
        if self.breakpoints.contains(&address) {
            false
        } else {
            self.breakpoints.push(address);
            true
        }
    }

    pub fn clear_breakpoint(&mut self, address: u32) -> bool {
        let before = self.breakpoints.len();
        self.breakpoints.retain(|a| *a != address);
        before != self.breakpoints.len()
    }

    /// Find the symbol containing `address` (function name + start), if any.
    pub fn symbol_at(&self, address: u32) -> Option<&Symbol> {
        self.symbols
            .iter()
            .find(|symbol| address >= symbol.address && address < symbol.address + symbol.size)
    }

    /// Map a source line (1-based within a file index) to its first address.
    pub fn address_for_line(&self, file: usize, line: u32) -> Option<u32> {
        self.line_table
            .iter()
            .find(|entry| entry.file == file && entry.line == line)
            .map(|entry| entry.address)
    }

    /// Map an address to its best-guess source line (file index, 1-based line).
    pub fn line_for_address(&self, address: u32) -> Option<(usize, u32)> {
        self.line_table
            .iter()
            .filter(|entry| entry.address <= address)
            .max_by_key(|entry| entry.address)
            .map(|entry| (entry.file, entry.line))
    }

    /// Execute one instruction. Returns what happened.
    pub fn step(&mut self) -> StepResult {
        if self.halted {
            return StepResult::Halted;
        }
        if self.breakpoints.contains(&self.pc) {
            return StepResult::Breakpoint;
        }
        let insn = self.read_u32(self.pc);
        self.execute(insn);
        self.cycle_count += 1;
        self.tick_peripherals();
        if self.halted {
            StepResult::Halted
        } else {
            StepResult::Advanced
        }
    }

    /// Advance cycle-based peripherals (GPIO sampling, timers) by one core cycle.
    pub fn tick_peripherals(&mut self) {
        let cycle = self.cycle_count;
        for (index, timer) in self.peripherals.timers.iter_mut().enumerate() {
            if timer.tick() && timer.irq_line < self.peripherals.irq_pending.len() {
                self.peripherals.irq_pending[timer.irq_line] = true;
                let _ = index;
            }
        }
        // Record the current GPIO input as an event so the IDE can visualize it.
        if self.peripherals.gpio_in != 0 && cycle % 8 == 0 {
            // no-op placeholder to keep the log meaningful without spamming
        }
    }

    /// Run until a breakpoint, halt, or `max_steps` instructions.
    pub fn run(&mut self, max_steps: u64) -> StepResult {
        let mut steps = 0u64;
        while steps < max_steps {
            let result = self.step();
            if result != StepResult::Advanced {
                return result;
            }
            steps += 1;
        }
        StepResult::Advanced
    }

    /// Single-step over a call/loop by running until PC returns to the line
    /// after the current one, or until a breakpoint/halt.
    pub fn step_over(&mut self, max_steps: u64) -> StepResult {
        let return_pc = self.pc.wrapping_add(4);
        let mut steps = 0u64;
        while steps < max_steps {
            let result = self.step();
            if result != StepResult::Advanced {
                return result;
            }
            if self.pc == return_pc {
                return StepResult::Advanced;
            }
            steps += 1;
        }
        StepResult::Advanced
    }

    pub fn read_reg(&self, index: usize) -> u32 {
        self.registers[index.min(REG_COUNT - 1)]
    }

    pub fn read_u32(&self, address: u32) -> u32 {
        let base = (address as usize) & !3;
        let mut value = 0u32;
        for i in 0..4 {
            value |= (self.memory[base.wrapping_add(i) % MEMORY_SIZE] as u32) << (8 * i);
        }
        value
    }

    pub fn read_u8(&self, address: u32) -> u8 {
        self.memory[(address as usize) % MEMORY_SIZE]
    }

    pub fn write_u32(&mut self, address: u32, value: u32) {
        let base = (address as usize) & !3;
        for i in 0..4 {
            let idx = base.wrapping_add(i) % MEMORY_SIZE;
            self.memory[idx] = (value >> (8 * i)) as u8;
        }
    }

    pub fn write_bytes(&mut self, base: u32, bytes: &[u8]) {
        for (i, byte) in bytes.iter().enumerate() {
            let idx = (base as usize).wrapping_add(i) % MEMORY_SIZE;
            self.memory[idx] = *byte;
        }
    }

    fn zero_bytes(&mut self, base: u32, count: usize) {
        for i in 0..count {
            let idx = (base as usize).wrapping_add(i) % MEMORY_SIZE;
            self.memory[idx] = 0;
        }
    }

    fn execute(&mut self, insn: u32) {
        let opcode = insn & 0x7f;
        let rd = ((insn >> 7) & 0x1f) as usize;
        let rs1 = ((insn >> 15) & 0x1f) as usize;
        let rs2 = ((insn >> 20) & 0x1f) as usize;
        let funct3 = (insn >> 12) & 0x7;
        let funct7 = (insn >> 25) & 0x7f;
        let imm_i = ((insn as i32) >> 20) as u32; // sign-extended
        let imm_s = (((insn >> 7) & 0x1f) | (((insn >> 25) & 0x7f) << 5)) as u32;
        let imm_s = sign_extend(imm_s, 12);

        match opcode {
            0x37 => {
                // LUI
                let value = insn & 0xffff_f000;
                self.set_reg(rd, value);
                self.pc = self.pc.wrapping_add(4);
            }
            0x17 => {
                // AUIPC
                let value = (insn & 0xffff_f000).wrapping_add(self.pc);
                self.set_reg(rd, value);
                self.pc = self.pc.wrapping_add(4);
            }
            0x6f => {
                // JAL
                let imm = jal_immediate(insn);
                let target = self.pc.wrapping_add(imm);
                self.set_reg(rd, self.pc.wrapping_add(4));
                self.pc = target;
            }
            0x67 => {
                // JALR
                let target = self.registers[rs1].wrapping_add(imm_i) & !1;
                self.set_reg(rd, self.pc.wrapping_add(4));
                self.pc = target;
            }
            0x63 => {
                // Branch
                let imm = branch_immediate(insn);
                let take = match funct3 {
                    0 => self.registers[rs1] == self.registers[rs2],
                    1 => self.registers[rs1] != self.registers[rs2],
                    4 => (self.registers[rs1] as i32) < (self.registers[rs2] as i32),
                    5 => (self.registers[rs1] as i32) >= (self.registers[rs2] as i32),
                    6 => self.registers[rs1] < self.registers[rs2],
                    7 => self.registers[rs1] >= self.registers[rs2],
                    _ => false,
                };
                self.pc = if take {
                    self.pc.wrapping_add(imm)
                } else {
                    self.pc.wrapping_add(4)
                };
            }
            0x03 => {
                // Loads
                let addr = self.registers[rs1].wrapping_add(imm_i);
                let value = if let Some(periph) = self.peripheral_read(addr) {
                    periph
                } else {
                    match funct3 {
                        0 => self.read_u8(addr) as u32,             // LB
                        1 => self.read_u16(addr) as u32,            // LH
                        2 => self.read_u32(addr),                   // LW
                        4 => (self.read_u8(addr) as u32) & 0xff,    // LBU
                        5 => (self.read_u16(addr) as u32) & 0xffff, // LHU
                        _ => 0,
                    }
                };
                self.set_reg(rd, value);
                self.pc = self.pc.wrapping_add(4);
            }
            0x23 => {
                // Stores
                let addr = self.registers[rs1].wrapping_add(imm_s);
                let value = self.registers[rs2];
                match funct3 {
                    0 => {
                        self.memory[(addr as usize) % MEMORY_SIZE] = value as u8;
                        self.peripheral_write(addr, value as u8);
                    }
                    1 => {
                        self.write_bytes(addr, &value.to_le_bytes()[..2]);
                        self.peripheral_write(addr, value as u8);
                    }
                    2 => {
                        self.write_u32(addr, value);
                        self.peripheral_write(addr, value as u8);
                    }
                    _ => {}
                }
                self.pc = self.pc.wrapping_add(4);
            }
            0x13 => {
                // OP-IMM
                let value = match funct3 {
                    0 => self.registers[rs1].wrapping_add(imm_i),
                    2 => self.registers[rs1].wrapping_sub(imm_i),
                    3 => (self.registers[rs1] as i32).wrapping_shl((imm_i & 0x1f) as u32) as u32,
                    4 => {
                        let shamt = (insn >> 20) & 0x1f;
                        (self.registers[rs1] as i32 >> shamt) as u32
                    }
                    5 => {
                        let shamt = (insn >> 20) & 0x1f;
                        if funct7 == 0x20 {
                            self.registers[rs1] >> shamt
                        } else {
                            (self.registers[rs1] as i32 >> shamt) as u32
                        }
                    }
                    6 => self.registers[rs1] ^ imm_i,
                    7 => self.registers[rs1] & imm_i,
                    1 => self.registers[rs1] | imm_i,
                    _ => 0,
                };
                self.set_reg(rd, value);
                self.pc = self.pc.wrapping_add(4);
            }
            0x33 => {
                // OP
                let value = match funct3 {
                    0 if funct7 == 0x20 => self.registers[rs1].wrapping_sub(self.registers[rs2]),
                    0 => self.registers[rs1].wrapping_add(self.registers[rs2]),
                    1 => self.registers[rs1].wrapping_shl(self.registers[rs2] & 0x1f),
                    2 => ((self.registers[rs1] as i32) < (self.registers[rs2] as i32)) as u32,
                    3 => (self.registers[rs1] < self.registers[rs2]) as u32,
                    4 => self.registers[rs1] ^ self.registers[rs2],
                    5 if funct7 == 0x20 => self.registers[rs1] >> (self.registers[rs2] & 0x1f),
                    5 => ((self.registers[rs1] as i32) >> (self.registers[rs2] & 0x1f)) as u32,
                    6 => self.registers[rs1] | self.registers[rs2],
                    7 => self.registers[rs1] & self.registers[rs2],
                    _ => 0,
                };
                self.set_reg(rd, value);
                self.pc = self.pc.wrapping_add(4);
            }
            0x0f => {
                // FENCE / ECALL / EBREAK
                if insn & 0xfff == 0x73 {
                    // ECALL: emulate a `printf`-style halt via register a7.
                    let syscall = self.registers[17]; // a7
                    if syscall == 0 {
                        self.halted = true;
                    }
                }
                self.pc = self.pc.wrapping_add(4);
            }
            _ => {
                // Unknown opcode: halt to avoid runaway.
                self.halted = true;
            }
        }
    }

    fn set_reg(&mut self, index: usize, value: u32) {
        if index != 0 {
            self.registers[index] = value;
        }
    }

    fn read_u16(&self, address: u32) -> u16 {
        let base = (address as usize) & !1;
        (self.memory[base % MEMORY_SIZE] as u16)
            | ((self.memory[(base + 1) % MEMORY_SIZE] as u16) << 8)
    }

    /// Route a store to virtual peripherals if the address is in their range.
    fn peripheral_write(&mut self, address: u32, _byte: u8) {
        if address >= self.peripherals.uart_base && address < self.peripherals.uart_base + 8 {
            let data = self.memory[(address as usize) % MEMORY_SIZE];
            if data != 0 {
                self.peripherals.outputs.push(data);
                (self.peripherals.putchar)(data);
            }
        }
        self.gpio_write(address);
        self.timer_write(address);
    }

    /// Handle GPIO register writes (OUT, OUT_SET, OUT_CLR, DIR, IN).
    fn gpio_write(&mut self, address: u32) {
        let base = self.peripherals.gpio_base;
        let value = self.read_u32(address);
        let offset = address.wrapping_sub(base);
        let old = self.peripherals.gpio_out;
        match offset {
            0x00 => {
                // OUT
                let new = value & self.peripherals.gpio_dir;
                self.peripherals.gpio_out = new;
            }
            0x04 => {
                // OUT_SET
                self.peripherals.gpio_out |= value & self.peripherals.gpio_dir;
            }
            0x08 => {
                // OUT_CLR
                self.peripherals.gpio_out &= !(value & self.peripherals.gpio_dir);
            }
            0x0c => {
                // DIR
                self.peripherals.gpio_dir = value;
            }
            _ => {}
        }
        if old != self.peripherals.gpio_out {
            let diff = old ^ self.peripherals.gpio_out;
            for pin in 0..32 {
                if diff & (1 << pin) != 0 {
                    let value = (self.peripherals.gpio_out >> pin) & 1;
                    self.peripherals
                        .gpio_events
                        .push((self.cycle_count, pin, value));
                }
            }
        }
    }

    /// Handle timer register writes (CR, PSC, CNT, ARR, SR).
    fn timer_write(&mut self, address: u32) {
        let value = self.read_u32(address);
        for timer in self.peripherals.timers.iter_mut() {
            let offset = address.wrapping_sub(timer.base);
            match offset {
                0x00 => timer.enabled = value & 1 != 0,
                0x04 => timer.prescaler = value & 0xffff,
                0x08 => timer.counter = value,
                0x0c => timer.auto_reload = value,
                0x10 => {
                    // SR: clear update interrupt.
                    if timer.irq_line < self.peripherals.irq_pending.len() {
                        self.peripherals.irq_pending[timer.irq_line] = false;
                    }
                }
                _ => {}
            }
        }
    }

    /// Read a memory-mapped peripheral register (GPIO / timers / UART status).
    fn peripheral_read(&self, address: u32) -> Option<u32> {
        if address >= self.peripherals.gpio_base
            && address < self.peripherals.gpio_base + 0x20
        {
            let offset = address.wrapping_sub(self.peripherals.gpio_base);
            return Some(match offset {
                0x00 => self.peripherals.gpio_out,
                0x04 => self.peripherals.gpio_in,
                0x08 => self.peripherals.gpio_dir,
                _ => 0,
            });
        }
        for timer in &self.peripherals.timers {
            let offset = address.wrapping_sub(timer.base);
            if offset < 0x14 {
                return Some(match offset {
                    0x00 => timer.enabled as u32,
                    0x04 => timer.prescaler,
                    0x08 => timer.counter,
                    0x0c => timer.auto_reload,
                    0x10 => {
                        if timer.irq_line < self.peripherals.irq_pending.len()
                            && self.peripherals.irq_pending[timer.irq_line]
                        {
                            1
                        } else {
                            0
                        }
                    }
                    _ => 0,
                });
            }
        }
        None
    }
}

impl Default for Simulator {
    fn default() -> Self {
        Self::new()
    }
}

fn sign_extend(value: u32, bits: u32) -> u32 {
    let shift = 32 - bits;
    ((value << shift) as i32 >> shift) as u32
}

fn jal_immediate(insn: u32) -> u32 {
    let imm20 = (insn >> 31) & 1;
    let imm10_1 = (insn >> 21) & 0x3ff;
    let imm11 = (insn >> 20) & 1;
    let imm19_12 = (insn >> 12) & 0xff;
    let value = (imm20 << 20) | (imm10_1 << 1) | (imm11 << 11) | (imm19_12 << 12);
    sign_extend(value, 21)
}

fn branch_immediate(insn: u32) -> u32 {
    let imm12 = (insn >> 31) & 1;
    let imm10_5 = (insn >> 25) & 0x3f;
    let imm4_1 = (insn >> 8) & 0xf;
    let imm11 = (insn >> 7) & 1;
    let value = (imm12 << 12) | (imm10_5 << 5) | (imm4_1 << 1) | (imm11 << 11);
    sign_extend(value, 13)
}

fn read_u32(bytes: &[u8], offset: usize, little: bool) -> u32 {
    if little {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    } else {
        u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }
}

fn read_u16(bytes: &[u8], offset: usize, little: bool) -> u16 {
    if little {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
    } else {
        u16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap())
    }
}

/// Human-readable register name (ABI).
pub fn reg_name(index: usize) -> &'static str {
    const NAMES: [&str; 32] = [
        "zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3",
        "a4", "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11",
        "t3", "t4", "t5", "t6",
    ];
    NAMES[index.min(31)]
}

/// A snapshot of simulator state for UI display.
#[derive(Debug, Clone, Serialize)]
pub struct SimState {
    pub pc: u32,
    pub registers: HashMap<String, u32>,
    pub cycle_count: u64,
    pub halted: bool,
    pub uart_output: String,
}

impl Simulator {
    pub fn snapshot(&self) -> SimState {
        let mut registers = HashMap::new();
        for (i, value) in self.registers.iter().enumerate() {
            registers.insert(reg_name(i).to_string(), *value);
        }
        SimState {
            pc: self.pc,
            registers,
            cycle_count: self.cycle_count,
            halted: self.halted,
            uart_output: String::from_utf8_lossy(&self.peripherals.outputs).into_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_counts_and_raises_irq() {
        let mut sim = Simulator::new();
        sim.peripherals.timers.push(Timer::new(0x4000_1000, 3));
        let base = 0x4000_1000;
        // Enable, prescaler 1, auto-reload 4.
        sim.write_u32(base + 0x04, 1);
        sim.timer_write(base + 0x04);
        sim.write_u32(base + 0x0c, 4);
        sim.timer_write(base + 0x0c);
        sim.write_u32(base + 0x00, 1);
        sim.timer_write(base + 0x00);

        for _ in 0..10 {
            sim.tick_peripherals();
        }
        // After 5 ticks counter wraps once; irq should be pending.
        assert!(sim.peripherals.irq_pending[3]);
        assert!(sim.peripherals.timers[0].update_count >= 1);
    }

    #[test]
    fn gpio_out_records_events() {
        let mut sim = Simulator::new();
        let base = sim.peripherals.gpio_base;
        // All pins output, then set pin 0.
        sim.write_u32(base + 0x0c, 0xffff_ffff);
        sim.gpio_write(base + 0x0c);
        sim.write_u32(base + 0x04, 0x0000_0001);
        sim.gpio_write(base + 0x04);
        assert_eq!(sim.peripherals.gpio_out & 1, 1);
        assert!(!sim.peripherals.gpio_events.is_empty());
        // Clear pin 0.
        sim.write_u32(base + 0x08, 0x0000_0001);
        sim.gpio_write(base + 0x08);
        assert_eq!(sim.peripherals.gpio_out & 1, 0);
    }

    #[test]
    fn timer_reads_back_registers() {
        let mut sim = Simulator::new();
        sim.peripherals.timers.push(Timer::new(0x4000_1000, 0));
        let base = 0x4000_1000;
        sim.write_u32(base + 0x0c, 77);
        sim.timer_write(base + 0x0c);
        assert_eq!(sim.peripheral_read(base + 0x0c), Some(77));
    }

    #[test]
    fn line_mapping_round_trips() {
        let mut sim = Simulator::new();
        sim.line_table = vec![
            LineEntry { address: 0x1000, file: 0, line: 1 },
            LineEntry { address: 0x1004, file: 0, line: 2 },
            LineEntry { address: 0x1008, file: 0, line: 3 },
        ];
        assert_eq!(sim.address_for_line(0, 2), Some(0x1004));
        assert_eq!(sim.line_for_address(0x1006), Some((0, 2)));
        assert_eq!(sim.line_for_address(0x1008), Some((0, 3)));
        assert_eq!(sim.line_for_address(0x1000), Some((0, 1)));
    }

    #[test]
    fn symbol_lookup_finds_containing_function() {
        let mut sim = Simulator::new();
        sim.symbols = vec![
            Symbol { name: "main".into(), address: 0x2000, size: 16 },
            Symbol { name: "loop".into(), address: 0x2010, size: 8 },
        ];
        assert_eq!(sim.symbol_at(0x2004).map(|s| s.name.as_str()), Some("main"));
        assert_eq!(sim.symbol_at(0x2014).map(|s| s.name.as_str()), Some("loop"));
        assert!(sim.symbol_at(0x3000).is_none());
    }
}
