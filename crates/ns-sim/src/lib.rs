//! `ns-sim` — a built-in instruction-level simulator for ns-embed-ide.
//!
//! Implements a clean, verifiable RV32I (RISC-V 32-bit integer) core that can
//! load a program image (raw bytes or an ELF32), step/run instructions, and
//! expose registers and memory. A small virtual-peripheral model provides a
//! memory-mapped UART whose output the IDE can surface in the console.

use serde::Serialize;

pub mod mcs51;

pub mod arm;

pub mod x51;

pub mod c166;

pub mod xtensa;

pub mod avr;

pub mod msp430;

pub mod v850;

pub mod stm8;

pub mod rx;

pub mod rl78;

use std::collections::HashMap;
pub const REG_COUNT: usize = 32;
pub const MEMORY_SIZE: usize = 64 * 1024; // 64 KiB simulated RAM
pub const ENTRY_POINT: u32 = 0x8000_0000;

/// The target instruction-set architecture a simulator instance runs.
///
/// The built-in simulator currently implements only [`Arch::RiscV`] (RV32I);
/// the other architectures are recognized so the IDE can report a clear
/// "not yet implemented" error instead of silently mis-decoding the artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    /// RISC-V 32-bit (RV32I); used by ESP32-C3 and generic RISC-V targets.
    RiscV,
    /// ARM Cortex-M (Thumb/Thumb-2); used by STM32, nRF52, RP2040.
    Arm,
    /// Intel 8051 / MCS-51 8-bit microcontroller.
    Mcs51,
    /// Intel 80251 / MCS-251 16-bit (8051 superset).
    Mcs251,
    /// Infineon C166 16-bit (and its XC166/XC2000 derivatives).
    C166,
    /// ESP32 Xtensa (LX6/LX7, e.g. ESP32/ESP32-S3).
    Xtensa,
    /// Atmel/Microchip AVR 8-bit (ATmega/ATtiny; IAR AVR).
    Avr,
    /// TI MSP430 16-bit (IAR MSP430).
    Msp430,
    /// Renesas V850E 32-bit (IAR V850).
    V850,
    /// ST Microelectronics STM8 (ST8 core, IAR STM8).
    Stm8,
    /// Renesas RX (RX600 family, IAR RX).
    Rx,
    /// Renesas RL78 (16-bit CISC, IAR RL78).
    Rl78,

}

impl Arch {
    /// Human-readable label for status messages.
    pub fn label(self) -> &'static str {
        match self {
            Arch::RiscV => "RISC-V (RV32I)",
            Arch::Arm => "ARM Cortex-M",
            Arch::Mcs51 => "Intel 8051 (MCS-51)",
            Arch::Mcs251 => "Intel 80251 (MCS-251)",
            Arch::C166 => "Infineon C166 (16-bit)",
            Arch::Xtensa => "ESP32 Xtensa",
            Arch::Avr => "Atmel AVR (8-bit)",
            Arch::Msp430 => "TI MSP430 (16-bit)",
            Arch::V850 => "Renesas V850 (32-bit)",
            Arch::Stm8 => "STMicroelectronics STM8",
            Arch::Rx => "Renesas RX (32-bit)",
            Arch::Rl78 => "Renesas RL78 (16-bit)",
        }
    }

    /// Map an ELF `e_machine` field to an architecture.
    pub fn from_elf_machine(machine: u16) -> Option<Arch> {
        match machine {
            243 => Some(Arch::RiscV),  // EM_RISCV
            40 => Some(Arch::Arm),     // EM_ARM
            94 => Some(Arch::Xtensa),  // EM_XTENSA
            83 => Some(Arch::Avr),     // EM_AVR
            105 | 167 => Some(Arch::Msp430), // EM_MSP430 / EM_MSP430_2
            87 => Some(Arch::V850),    // EM_V850
            186 => Some(Arch::Stm8),   // EM_STM8
            173 => Some(Arch::Rx),     // EM_RX (Renesas RX)
            197 => Some(Arch::Rl78),   // EM_RL78 (Renesas RL78)
            _ => None,
        }
    }

    /// Map a `project.toml` target key (e.g. `stm32f103c8`, `esp32c3`) to an
    /// architecture. `catalog_entry`-independent: uses the same core naming the
    /// device catalog uses. Unknown targets default to [`Arch::RiscV`] since
    /// that is the only implemented core and the historical default.
    pub fn from_target(target: &str) -> Arch {
        let t = target.to_ascii_lowercase();
        if t.contains("riscv") || t.starts_with("esp32c") {
            Arch::RiscV
        } else if t.contains("esp32") {
            Arch::Xtensa
        } else if t.contains("80251") || t.contains("mcs251") || t.contains("c251") {
            Arch::Mcs251
        } else if t.contains("c166")
            || t.contains("xc166")
            || t.contains("xc2000")
            || t.contains("c16x")
            || t.contains("st10")
        {
            Arch::C166
        } else if t.contains("8051") || t.contains("mcs51") || t.contains("c51") {
            Arch::Mcs51
        } else if t.contains("atmega")
            || t.contains("attiny")
            || t.contains("at90")
            || t.contains("avr")
        {
            Arch::Avr
        } else if t.contains("msp430") {
            Arch::Msp430
        } else if t.contains("v850") {
            Arch::V850
        } else if t.contains("stm8")
            || t.contains("stm8s")
            || t.contains("stm8l")
            || t.starts_with("stm8")
        {
            Arch::Stm8
        } else if t.contains("rx") || t.starts_with("r5f") {
            Arch::Rx
        } else if t.contains("rl78") {
            Arch::Rl78
        } else if t.contains("arm7")
            || t.contains("arm9")
            || t.contains("arm926")
            || t.contains("arm920")
            || t.contains("lpc21")
            || t.contains("lpc22")
            || t.contains("s3c24")
            || t.contains("s3c2440")
            || t.contains("at91")
            || t.contains("imx")
            || t.contains("a32")
            || t.contains("arm-state")
        {
            Arch::Arm
        } else if t.starts_with("stm32")
            || t.starts_with("nrf")
            || t.starts_with("rp2040")
            || t.contains("cortex")
        {
            Arch::Arm
        } else {
            Arch::RiscV
        }
    }
}

/// A single GPIO output-change event. `cycle` is the cycle count when the
/// write happened, and `pc` is the program counter of the instruction that
/// caused it — so the IDE can align the event with the source location of the
/// store, not just a wall-clock tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpioEvent {
    /// Cycle count when the change was committed.
    pub cycle: u64,
    /// Pin index (0..32).
    pub pin: u32,
    /// New pin value (0 or 1).
    pub value: u32,
    /// Program counter of the store instruction that caused the change.
    pub pc: u32,
}

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
    pub gpio_events: Vec<GpioEvent>,
    /// Up to 8 general-purpose timers.
    pub timers: Vec<Timer>,
    /// The watchdog timer (WDT), if enabled by the firmware.
    pub watchdog: Option<Watchdog>,
    /// Pending interrupt lines asserted by peripherals.
    pub irq_pending: [bool; 16],
}

/// A virtual watchdog timer. When enabled, it counts core cycles; if the
/// firmware fails to feed it (write 0xAA55 to the reload register) before
/// `timeout_cycles` elapse, the watchdog fires a reset (`reset_count`
/// increments and `last_reset_pc` records the PC that overflowed). The IDE
/// can surface the reset count so runaway firmware is visible rather than
/// silently stalling.
#[derive(Debug, Clone)]
pub struct Watchdog {
    pub base: u32,
    pub enabled: bool,
    /// Cycles before the watchdog fires (0 = never while disabled).
    pub timeout_cycles: u64,
    /// Cycles elapsed since the last feed or enable.
    pub elapsed: u64,
    /// Number of times the watchdog has fired (reset events).
    pub reset_count: u64,
    /// PC at which the watchdog fired (for the IDE to report).
    pub last_reset_pc: u32,
}

impl Watchdog {
    pub fn new(base: u32) -> Self {
        Self {
            base,
            enabled: false,
            timeout_cycles: 0,
            elapsed: 0,
            reset_count: 0,
            last_reset_pc: 0,
        }
    }
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

    /// Advance the timer by one core cycle. Returns the number of update events
    /// (overflows) fired by this cycle: 0 for a normal prescaler tick, and 1+
    /// when the up-counter crosses its auto-reload in one cycle (the counter may
    /// be several reloads ahead when `run()` loops at high speed, so the exact
    /// count is computed rather than treated as a single event).
    pub fn tick(&mut self) -> u64 {
        if !self.enabled {
            return 0;
        }
        let prescaler = (self.prescaler & 0xffff).max(1);
        self.prescaler_clock = self.prescaler_clock.wrapping_add(1);
        if self.prescaler_clock >= prescaler {
            self.prescaler_clock = 0;
            let reload = self.auto_reload.max(1);
            if self.counter >= reload {
                let overflows = self.counter / reload;
                self.counter = self.counter % reload;
                self.update_count = self.update_count.saturating_add(overflows as u64);
                overflows as u64
            } else {
                self.counter = self.counter.saturating_add(1);
                0
            }
        } else {
            0
        }
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
            watchdog: None,
            irq_pending: [false; 16],
        }
    }
}

/// Machine-mode control and status registers for the RV32I core. The simulator
/// implements the minimal set needed for interrupt handling: the trap vector,
/// the saved program counter/cause, global interrupt enable, and pending/mask
/// bits that gate which peripheral lines are taken.
#[derive(Debug, Clone)]
pub struct Csr {
    /// `mstatus`: machine status. Bit 3 = MIE (global interrupt enable).
    pub mstatus: u32,
    /// `mtvec`: trap vector base. Bits 0..1 mode: 0 = direct, 1 = vectored.
    pub mtvec: u32,
    /// `mepc`: program counter saved on trap entry.
    pub mepc: u32,
    /// `mcause`: cause of the last trap (interrupt = bit 31 set).
    pub mcause: u32,
    /// `mie`: machine interrupt-enable bits (one per peripheral line).
    pub mie: u32,
    /// `mhartid`: hardware thread id (always 0 for this single-core model).
    pub mhartid: u32,
}

impl Default for Csr {
    fn default() -> Self {
        Self {
            mstatus: 0,
            mtvec: 0,
            mepc: 0,
            mcause: 0,
            mie: 0,
            mhartid: 0,
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
    /// Machine-mode control and status registers (interrupt support).
    pub csr: Csr,
    /// Target architecture this simulator was created for. Controls which
    /// instruction decoder is used; only [`Arch::RiscV`] is implemented so far.
    pub arch: Arch,
    /// The 8051 core, present only when `arch == Arch::Mcs51`. The RV32I
    /// fields (`pc`, `registers`, `memory`) are unused in that mode.
    pub mcs51: Option<mcs51::Mc51Core>,
    /// The ARM Cortex-M core, present only when `arch == Arch::Arm`. The
    /// RV32I/8051 fields are unused in that mode.
    pub arm: Option<arm::ArmCore>,
    /// The 80251 core, present only when `arch == Arch::Mcs251`.
    pub x51: Option<x51::X51Core>,
    /// The C166 core, present only when `arch == Arch::C166`.
    pub c166: Option<c166::C166Core>,
    /// The Xtensa LX6 core, present only when `arch == Arch::Xtensa`.
    pub xtensa: Option<xtensa::Lx6Core>,
    /// The AVR core, present only when `arch == Arch::Avr`.
    pub avr: Option<avr::AvrCore>,
    /// The MSP430 core, present only when `arch == Arch::Msp430`.
    pub msp430: Option<msp430::Msp430Core>,
    /// The V850 core, present only when `arch == Arch::V850`.
    pub v850: Option<v850::V850Core>,
    /// The STM8 core, present only when `arch == Arch::Stm8`.
    pub stm8: Option<stm8::Stm8Core>,
    /// The RX core, present only when `arch == Arch::Rx`.
    pub rx: Option<rx::RxCore>,
    /// The RL78 core, present only when `arch == Arch::Rl78`.
    pub rl78: Option<rl78::Rl78Core>,
    /// Cycle-accurate modeling: when enabled, each `step` advances
    /// `cycle_count` by the instruction's estimated latency instead of 1.
    pub cycle_accurate: bool,
    /// Cycles accounted for by the most recently stepped instruction (0 when
    /// cycle-accurate is off). Lets the UI show per-instruction timing.
    pub last_instruction_cycles: u32,
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
            csr: Csr::default(),
            arch: Arch::RiscV,
            mcs51: None,
            arm: None,
            x51: None,
            c166: None,
            xtensa: None,
            avr: None,
            msp430: None,
            v850: None,
            stm8: None,
            rx: None,
            rl78: None,
            cycle_accurate: false,
            last_instruction_cycles: 0,
        }
    }
    /// Create a simulator for a target architecture. RISC-V, 8051, ARM,
    /// 80251, C166, Xtensa, AVR, MSP430 and V850 are implemented.
    pub fn for_arch(arch: Arch) -> anyhow::Result<Self> {
        let mut sim = Self::new();
        sim.arch = arch;
        match arch {
            Arch::RiscV
            | Arch::Mcs51
            | Arch::Arm
            | Arch::Mcs251
            | Arch::C166
            | Arch::Xtensa
            | Arch::Avr
            | Arch::Msp430
            | Arch::V850
            | Arch::Stm8
            | Arch::Rx
            | Arch::Rl78 => {
                if arch == Arch::Mcs51 {
                    sim.mcs51 = Some(mcs51::Mc51Core::new());
                } else if arch == Arch::Arm {
                    sim.arm = Some(arm::ArmCore::new());
                } else if arch == Arch::Mcs251 {
                    sim.x51 = Some(x51::X51Core::new());
                } else if arch == Arch::C166 {
                    sim.c166 = Some(c166::C166Core::new());
                } else if arch == Arch::Xtensa {
                    sim.xtensa = Some(xtensa::Lx6Core::new());
                } else if arch == Arch::Avr {
                    sim.avr = Some(avr::AvrCore::new());
                } else if arch == Arch::Msp430 {
                    sim.msp430 = Some(msp430::Msp430Core::new());
                } else if arch == Arch::V850 {
                    sim.v850 = Some(v850::V850Core::new());
                } else if arch == Arch::Stm8 {
                    sim.stm8 = Some(stm8::Stm8Core::new());
                } else if arch == Arch::Rx {
                    sim.rx = Some(rx::RxCore::new());
                } else if arch == Arch::Rl78 {
                    sim.rl78 = Some(rl78::Rl78Core::new());
                }
                Ok(sim)
            }
        }
    }
    /// Reset the core (PC to entry, registers zeroed, memory preserved).
    /// For ARM, the vector table at the entry base is used when present.
    pub fn reset(&mut self, entry: u32) {
        if let Some(core) = self.arm.as_mut() {
            core.reset_from_vector_table(entry & !3, entry);
            self.pc = core.regs[arm::PC];
            self.halted = core.halted;
            self.cycle_count = 0;
            return;
        }
        if let Some(core) = self.xtensa.as_mut() {
            core.reset(entry);
            self.pc = core.pc;
            self.halted = core.halted;
            self.cycle_count = 0;
            return;
        }
        if let Some(core) = self.avr.as_mut() {
            core.reset(entry);
            self.pc = core.pc;
            self.halted = core.halted;
            self.cycle_count = 0;
            return;
        }
        if let Some(core) = self.msp430.as_mut() {
            core.reset(entry);
            self.pc = core.pc;
            self.halted = core.halted;
            self.cycle_count = 0;
            return;
        }
        if let Some(core) = self.v850.as_mut() {
            core.reset(entry);
            self.pc = core.pc;
            self.halted = core.halted;
            self.cycle_count = 0;
            return;
        }
        if let Some(core) = self.stm8.as_mut() {
            core.reset(entry);
            self.pc = core.pc;
            self.halted = core.halted;
            self.cycle_count = 0;
            return;
        }
        if let Some(core) = self.rx.as_mut() {
            core.reset(entry);
            self.pc = core.pc;
            self.halted = core.halted;
            self.cycle_count = 0;
            return;
        }
        if let Some(core) = self.rl78.as_mut() {
            core.reset(entry);
            self.pc = core.pc;
            self.halted = core.halted;
            self.cycle_count = 0;
            return;
        }
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

    /// Load a decoded program image into the 8051 code space. `base` is the
    /// lowest load address and `image` the contiguous bytes from `ns_flash`'s
    /// Intel HEX decoder. Only valid when `arch == Arch::Mcs51`.
    pub fn load_mcs51(&mut self, base: u16, image: &[u8]) -> anyhow::Result<()> {
        let core = self.mcs51.as_mut().ok_or_else(|| {
            anyhow::anyhow!("8051 内核未创建（目标架构非 8051）")
        })?;
        for (i, byte) in image.iter().enumerate() {
            let addr = (base as usize).wrapping_add(i);
            if addr >= mcs51::ROM_SIZE {
                anyhow::bail!("8051 代码地址 0x{addr:x} 超出 64 KiB 代码空间");
            }
            core.rom[addr] = *byte;
        }
        core.pc = base;
        core.halted = false;
        Ok(())
    }

    /// Load a decoded program image into the 80251 code space. `base` is the
    /// lowest load address. Only valid when `arch == Arch::Mcs251`.
    pub fn load_x51(&mut self, base: u32, image: &[u8]) -> anyhow::Result<()> {
        let core = self.x51.as_mut().ok_or_else(|| {
            anyhow::anyhow!("80251 内核未创建（目标架构非 80251）")
        })?;
        for (i, byte) in image.iter().enumerate() {
            let addr = (base as usize).wrapping_add(i);
            if addr >= x51::ROM_SIZE {
                anyhow::bail!("80251 代码地址 0x{addr:x} 超出代码空间");
            }
            core.rom[addr] = *byte;
        }
        core.pc = base;
        core.halted = false;
        Ok(())
    }

    /// Load a decoded program image into the C166 code space. `base` is the
    /// lowest load address. Only valid when `arch == Arch::C166`.
    pub fn load_c166(&mut self, base: u16, image: &[u8]) -> anyhow::Result<()> {
        let core = self.c166.as_mut().ok_or_else(|| {
            anyhow::anyhow!("C166 内核未创建（目标架构非 C166）")
        })?;
        for (i, byte) in image.iter().enumerate() {
            let addr = (base as usize).wrapping_add(i);
            if addr >= c166::ROM_SIZE {
                anyhow::bail!("C166 代码地址 0x{addr:x} 超出 64 KiB 代码空间");
            }
            core.rom[addr] = *byte;
        }
        core.pc = base;
        core.halted = false;
        Ok(())
    }
    /// Load a program image into the AVR core's program memory. `base` is the
    /// byte address. Only valid when `arch == Arch::Avr`.
    pub fn load_avr(&mut self, base: u32, image: &[u8]) -> anyhow::Result<()> {
        let core = self.avr.as_mut().ok_or_else(|| {
            anyhow::anyhow!("AVR 内核未创建（目标架构非 AVR）")
        })?;
        for (i, byte) in image.iter().enumerate() {
            let addr = base as usize + i;
            if addr >= avr::ROM_SIZE {
                anyhow::bail!("AVR 代码地址 0x{addr:x} 超出 64 KiB 代码空间");
            }
            core.rom[addr] = *byte;
        }
        core.pc = base;
        core.halted = false;
        Ok(())
    }

    /// Load a program image into the MSP430 core (flat 64 KiB memory).
    pub fn load_msp430(&mut self, base: u32, image: &[u8]) -> anyhow::Result<()> {
        let core = self.msp430.as_mut().ok_or_else(|| {
            anyhow::anyhow!("MSP430 内核未创建（目标架构非 MSP430）")
        })?;
        core.write_bytes(base, image);
        core.set_pc(base);
        core.halted = false;
        Ok(())
    }

    /// Load a program image into the V850 core (flat 1 MiB memory).
    pub fn load_v850(&mut self, base: u32, image: &[u8]) -> anyhow::Result<()> {
        let core = self.v850.as_mut().ok_or_else(|| {
            anyhow::anyhow!("V850 内核未创建（目标架构非 V850）")
        })?;
        core.write_bytes(base, image);
        core.set_pc(base);
        core.halted = false;
        Ok(())
    }
    /// Load a program image into the STM8 core (flat 16 MiB address space).
    /// `base` is the 24-bit load address; the core starts at `base`.
    pub fn load_stm8(&mut self, base: u32, image: &[u8]) -> anyhow::Result<()> {
        let core = self.stm8.as_mut().ok_or_else(|| {
            anyhow::anyhow!("STM8 内核未创建（目标架构非 STM8）")
        })?;
        core.load_image(base, image);
        Ok(())
    }
    /// Load a program image into the RX core (flat 64 MiB memory window).
    pub fn load_rx(&mut self, base: u32, image: &[u8]) -> anyhow::Result<()> {
        let core = self.rx.as_mut().ok_or_else(|| {
            anyhow::anyhow!("RX 内核未创建（目标架构非 RX）")
        })?;
        core.load_image(base, image);
        Ok(())
    }

    /// Load a program image into the RL78 core (1 MiB flat memory window).
    pub fn load_rl78(&mut self, base: u32, image: &[u8]) -> anyhow::Result<()> {
        let core = self.rl78.as_mut().ok_or_else(|| {
            anyhow::anyhow!("RL78 内核未创建（目标架构非 RL78）")
        })?;
        core.load_image(base, image);
        Ok(())
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
        // Verify the ELF's machine type matches the simulator's architecture,
        // so a Thumb/RISC-V artifact isn't silently decoded with the wrong ISA.
        let machine = read_u16(elf, 18, little);
        let Some(elf_arch) = Arch::from_elf_machine(machine) else {
            anyhow::bail!("不支持的 ELF 机器类型 e_machine={machine}");
        };
        if elf_arch != self.arch {
            anyhow::bail!(
                "ELF 架构 {} 与仿真器架构 {} 不匹配",
                elf_arch.label(),
                self.arch.label()
            );
        }
        let entry = read_u32(elf, 24, little);
        let phoff = read_u32(elf, 28, little);
        let phentsize = read_u16(elf, 42, little) as usize;
        let phnum = read_u16(elf, 44, little) as usize;
        // For ARM Cortex-M, the vector table lives at the lowest PT_LOAD
        // address (the flash base); record it so reset() reads SP/PC from it.
        let mut flash_base: Option<u32> = None;
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
            if self.arm.is_some() {
                flash_base = Some(flash_base.map_or(p_vaddr, |b| b.min(p_vaddr)));
            }
            let end = p_offset.saturating_add(p_filesz).min(elf.len());
            let data_slice = &elf[p_offset..end];
            self.write_bytes(p_vaddr, data_slice);
            // Zero-fill the .bss portion.
            if p_memsz > p_filesz {
                let bss_start = p_vaddr.wrapping_add(p_filesz as u32);
                self.zero_bytes(bss_start, p_memsz - p_filesz);
            }
        }
        if let Some(core) = self.arm.as_mut() {
            if let Some(base) = flash_base {
                core.vtor = base & !3;
                core.reset_from_vector_table(base & !3, entry);
            } else {
                core.reset_from_vector_table(entry & !3, entry);
            }
            self.pc = core.regs[arm::PC];
            self.halted = core.halted;
        } else {
            self.reset(entry);
        }
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

    /// Estimate the clock cycles the instruction at `pc` takes, for
    /// cycle-accurate modeling. Returns 1 for the common case and a higher
    /// value for multi-cycle ops (loads/stores, branches, multiplies, etc.).
    /// This is a conservative pipeline model, not a cycle-exact datasheet
    /// count; it exists so cycle-accurate simulation shows realistic timing
    /// rather than a flat 1-cycle-per-instruction assumption.
    pub fn cycles_for_instruction(&self, pc: u32) -> u32 {
        match self.arch {
            Arch::RiscV => {
                let insn = self.read_u32(pc);
                let opcode = insn & 0x7F;
                match opcode {
                    // Loads and stores: 2 cycles (address + data phase).
                    0x03 | 0x23 => 2,
                    // JAL/JALR and taken branches: 2 cycles.
                    0x6F | 0x67 | 0x63 => 2,
                    // M extension multiply/divide: 4 cycles.
                    0x33 => {
                        let funct3 = (insn >> 12) & 0x7;
                        let funct7 = insn >> 25;
                        if funct7 == 0x01 || (funct3 == 0x04 || funct3 == 0x05) && funct7 == 0x00 {
                            4
                        } else {
                            1
                        }
                    }
                    _ => 1,
                }
            }
            Arch::Arm => {
                let half = self.read_u8(pc) as u16 | (self.read_u8(pc + 1) as u16) << 8;
                match half & 0xF800 {
                    // LDR/STR (register/imm offset): 2 cycles.
                    0x6000 | 0x6800 | 0x7000 | 0x7800 => 2,
                    // PUSH/POP/STM/LDM: 2 + N (one per register, approx).
                    0xB400 | 0xBC00 | 0xC000 | 0xC800 => 2,
                    // B/BL: 2 cycles.
                    0xE000 => 2,
                    _ => 1,
                }
            }
            // Most 8/16-bit CISC cores use 1 cycle per instruction in the
            // model; multi-byte or memory ops are approximated as 1.
            _ => 1,
        }
    }


    /// Execute one instruction. Returns what happened.
    pub fn step(&mut self) -> StepResult {
        let pc_before = self.program_counter();
        let est = self.cycles_for_instruction(pc_before);
        if let Some(core) = self.mcs51.as_mut() {
            let bps: Vec<u16> = self.breakpoints.iter().map(|b| (*b & 0xFFFF) as u16).collect();
            let result = core.step(&bps);
            self.last_instruction_cycles = if self.cycle_accurate { est.max(1) } else { 1 };
            self.cycle_count += self.last_instruction_cycles as u64;
            let result = match result {
                mcs51::Mc51Step::Advanced => StepResult::Advanced,
                mcs51::Mc51Step::Breakpoint => StepResult::Breakpoint,
                mcs51::Mc51Step::Halted => StepResult::Halted,
            };
            self.tick_peripherals_for(self.last_instruction_cycles as u64);
            return result;
        }
        if let Some(core) = self.arm.as_mut() {
            let bps: Vec<u32> = self.breakpoints.clone();
            let result = core.step(&bps);
            self.pc = core.regs[arm::PC];
            self.last_instruction_cycles = if self.cycle_accurate { est.max(1) } else { 1 };
            self.cycle_count += self.last_instruction_cycles as u64;
            self.halted = core.halted;
            let result = match result {
                arm::ArmStep::Advanced => StepResult::Advanced,
                arm::ArmStep::Breakpoint => StepResult::Breakpoint,
                arm::ArmStep::Halted => StepResult::Halted,
            };
            self.tick_peripherals_for(self.last_instruction_cycles as u64);
            return result;
        }
        if let Some(core) = self.x51.as_mut() {
            let bps: Vec<u32> = self.breakpoints.clone();
            let result = core.step(&bps);
            self.pc = core.pc;
            self.last_instruction_cycles = if self.cycle_accurate { est.max(1) } else { 1 };
            self.cycle_count += self.last_instruction_cycles as u64;
            self.halted = core.halted;
            let result = match result {
                x51::X51Step::Advanced => StepResult::Advanced,
                x51::X51Step::Breakpoint => StepResult::Breakpoint,
                x51::X51Step::Halted => StepResult::Halted,
            };
            self.tick_peripherals_for(self.last_instruction_cycles as u64);
            return result;
        }
        if let Some(core) = self.c166.as_mut() {
            let bps: Vec<u16> = self.breakpoints.iter().map(|b| (*b & 0xFFFF) as u16).collect();
            let result = core.step(&bps);
            self.pc = core.pc as u32;
            self.last_instruction_cycles = if self.cycle_accurate { est.max(1) } else { 1 };
            self.cycle_count += self.last_instruction_cycles as u64;
            self.halted = core.halted;
            let result = match result {
                c166::C166Step::Advanced => StepResult::Advanced,
                c166::C166Step::Breakpoint => StepResult::Breakpoint,
                c166::C166Step::Halted => StepResult::Halted,
            };
            self.tick_peripherals_for(self.last_instruction_cycles as u64);
            return result;
        }
        if let Some(core) = self.xtensa.as_mut() {
            let bps: Vec<u32> = self.breakpoints.clone();
            let result = core.step(&bps);
            self.pc = core.pc;
            self.last_instruction_cycles = if self.cycle_accurate { est.max(1) } else { 1 };
            self.cycle_count += self.last_instruction_cycles as u64;
            self.halted = core.halted;
            let result = match result {
                xtensa::Lx6Step::Advanced => StepResult::Advanced,
                xtensa::Lx6Step::Breakpoint => StepResult::Breakpoint,
                xtensa::Lx6Step::Halted => StepResult::Halted,
            };
            self.tick_peripherals_for(self.last_instruction_cycles as u64);
            return result;
        }
        if let Some(core) = self.avr.as_mut() {
            let bps: Vec<u32> = self.breakpoints.clone();
            let result = core.step(&bps);
            self.pc = core.pc;
            self.last_instruction_cycles = if self.cycle_accurate { est.max(1) } else { 1 };
            self.cycle_count += self.last_instruction_cycles as u64;
            self.halted = core.halted;
            let result = match result {
                avr::AvrStep::Advanced => StepResult::Advanced,
                avr::AvrStep::Breakpoint => StepResult::Breakpoint,
                avr::AvrStep::Halted => StepResult::Halted,
            };
            self.tick_peripherals_for(self.last_instruction_cycles as u64);
            return result;
        }
        if let Some(core) = self.msp430.as_mut() {
            let bps: Vec<u32> = self.breakpoints.clone();
            let result = core.step(&bps);
            self.pc = core.pc;
            self.last_instruction_cycles = if self.cycle_accurate { est.max(1) } else { 1 };
            self.cycle_count += self.last_instruction_cycles as u64;
            self.halted = core.halted;
            let result = match result {
                msp430::Msp430Step::Advanced => StepResult::Advanced,
                msp430::Msp430Step::Breakpoint => StepResult::Breakpoint,
                msp430::Msp430Step::Halted => StepResult::Halted,
            };
            self.tick_peripherals_for(self.last_instruction_cycles as u64);
            return result;
        }
        if let Some(core) = self.v850.as_mut() {
            let bps: Vec<u32> = self.breakpoints.clone();
            let result = core.step(&bps);
            self.pc = core.pc;
            self.last_instruction_cycles = if self.cycle_accurate { est.max(1) } else { 1 };
            self.cycle_count += self.last_instruction_cycles as u64;
            self.halted = core.halted;
            let result = match result {
                v850::V850Step::Advanced => StepResult::Advanced,
                v850::V850Step::Breakpoint => StepResult::Breakpoint,
                v850::V850Step::Halted => StepResult::Halted,
            };
            self.tick_peripherals_for(self.last_instruction_cycles as u64);
            return result;
        }
        if let Some(core) = self.stm8.as_mut() {
            let bps: Vec<u32> = self.breakpoints.clone();
            let result = core.step(&bps);
            self.pc = core.pc;
            self.last_instruction_cycles = if self.cycle_accurate { est.max(1) } else { 1 };
            self.cycle_count += self.last_instruction_cycles as u64;
            self.halted = core.halted;
            let result = match result {
                stm8::Stm8Step::Advanced => StepResult::Advanced,
                stm8::Stm8Step::Breakpoint => StepResult::Breakpoint,
                stm8::Stm8Step::Halted => StepResult::Halted,
            };
            self.tick_peripherals_for(self.last_instruction_cycles as u64);
            return result;
        }
        if let Some(core) = self.rx.as_mut() {
            let bps: Vec<u32> = self.breakpoints.clone();
            let result = core.step(&bps);
            self.pc = core.pc;
            self.last_instruction_cycles = if self.cycle_accurate { est.max(1) } else { 1 };
            self.cycle_count += self.last_instruction_cycles as u64;
            self.halted = core.halted;
            let result = match result {
                rx::RxStep::Advanced => StepResult::Advanced,
                rx::RxStep::Breakpoint => StepResult::Breakpoint,
                rx::RxStep::Halted => StepResult::Halted,
            };
            self.tick_peripherals_for(self.last_instruction_cycles as u64);
            return result;
        }
        if let Some(core) = self.rl78.as_mut() {
            let bps: Vec<u32> = self.breakpoints.clone();
            let result = core.step(&bps);
            self.pc = core.pc;
            self.last_instruction_cycles = if self.cycle_accurate { est.max(1) } else { 1 };
            self.cycle_count += self.last_instruction_cycles as u64;
            self.halted = core.halted;
            return match result {
                rl78::Rl78Step::Advanced => StepResult::Advanced,
                rl78::Rl78Step::Breakpoint => StepResult::Breakpoint,
                rl78::Rl78Step::Halted => StepResult::Halted,
            };
        }
        if self.halted {
            return StepResult::Halted;
        }
        if self.breakpoints.contains(&self.pc) {
            return StepResult::Breakpoint;
        }
        // Take a pending, enabled interrupt before the next instruction: this
        // mirrors real hardware, which traps at an instruction boundary. The
        // vector entry then runs the ISR via the trap vector (mtvec).
        if self.take_interrupt() {
            self.last_instruction_cycles = if self.cycle_accurate { est.max(1) } else { 1 };
            self.cycle_count += self.last_instruction_cycles as u64;
            return StepResult::Advanced;
        }
        let insn = self.read_u32(self.pc);
        self.execute(insn);
            self.last_instruction_cycles = if self.cycle_accurate { est.max(1) } else { 1 };
            self.cycle_count += self.last_instruction_cycles as u64;
        self.tick_peripherals_for(self.last_instruction_cycles as u64);
        if self.halted {
            StepResult::Halted
        } else {
            StepResult::Advanced
        }
    }

    /// The current program counter, unified across architectures (8051 is a
    /// 16-bit PC; the RV32I core is 32-bit).
    pub fn program_counter(&self) -> u32 {
        if let Some(core) = &self.mcs51 {
            core.pc as u32
        } else if let Some(core) = &self.x51 {
            core.pc
        } else if let Some(core) = &self.c166 {
            core.pc as u32
        } else if let Some(core) = &self.avr {
            core.pc
        } else if let Some(core) = &self.msp430 {
            core.pc
        } else if let Some(core) = &self.v850 {
            core.pc
        } else if let Some(core) = &self.stm8 {
            core.pc
        } else if let Some(core) = &self.rx {
            core.pc
        } else if let Some(core) = &self.rl78 {
            core.pc
        } else {
            self.pc
        }
    }

    /// Whether the ARM-family core is in ARM (A32) state. Only meaningful when
    /// `arch == Arch::Arm`; returns `false` for Cortex-M (Thumb) and other
    /// architectures.
    pub fn arm_state(&self) -> bool {
        self.arm.as_ref().map(|core| core.arm_state).unwrap_or(false)
    }

    /// Set the program counter, routing to the active architecture core so the
    /// PC stays consistent (ARM/Xtensa keep their own PC register).
    pub fn set_program_counter(&mut self, pc: u32) {
        self.pc = pc;
        if let Some(core) = &mut self.mcs51 {
            core.pc = pc as u16;
        } else if let Some(core) = &mut self.x51 {
            core.pc = pc;
        } else if let Some(core) = &mut self.c166 {
            core.pc = pc as u16;
        } else if let Some(core) = &mut self.arm {
            core.set_pc(pc);
        } else if let Some(core) = &mut self.xtensa {
            core.set_pc(pc);
        } else if let Some(core) = &mut self.avr {
            core.set_pc(pc);
        } else if let Some(core) = &mut self.msp430 {
            core.set_pc(pc);
        } else if let Some(core) = &mut self.v850 {
            core.set_pc(pc);
        } else if let Some(core) = &mut self.stm8 {
            core.set_pc(pc);
        } else if let Some(core) = &mut self.rx {
            core.set_pc(pc);
        } else if let Some(core) = &mut self.rl78 {
            core.set_pc(pc);
        }
    }

    /// Advance cycle-based peripherals (GPIO sampling, timers) by one core
    /// cycle. Equivalent to [`Self::tick_peripherals_for`] with 1.
    pub fn tick_peripherals(&mut self) {
        self.tick_peripherals_for(1);
    }

    /// Advance cycle-based peripherals by `cycles` core cycles. In
    /// cycle-accurate mode each instruction advances `cycle_count` by its
    /// estimated latency, so peripherals must be ticked by that same amount or
    /// timers run slow relative to the CPU clock.
    pub fn tick_peripherals_for(&mut self, cycles: u64) {
        for timer in self.peripherals.timers.iter_mut() {
            let mut overflow = false;
            for _ in 0..cycles.max(1) {
                if timer.tick() > 0 {
                    overflow = true;
                }
            }
            if overflow && timer.irq_line < self.peripherals.irq_pending.len() {
                self.peripherals.irq_pending[timer.irq_line] = true;
            }
        }
        // Advance the watchdog. When it overflows, record a reset event (the
        // firmware failed to feed it in time); the IDE surfaces `reset_count`
        // so runaway firmware is visible instead of silently stalling.
        if let Some(watchdog) = self.peripherals.watchdog.as_mut() {
            if watchdog.enabled && watchdog.timeout_cycles > 0 {
                watchdog.elapsed = watchdog.elapsed.saturating_add(cycles.max(1));
                if watchdog.elapsed >= watchdog.timeout_cycles {
                    watchdog.reset_count = watchdog.reset_count.saturating_add(1);
                    watchdog.last_reset_pc = self.pc;
                    watchdog.elapsed = 0;
                }
            }
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

    /// Single-step out of the current function by running until a return
    /// instruction is executed (the caller's next instruction), or until a
    /// breakpoint/halt. Architecture-independent: it inspects the decoded
    /// mnemonic at the current PC for a return class (`ret`/`reti`/`rts`/
    /// `rti`/`rfe`/`rte`/`retf`) before stepping. A function that never
    /// returns runs to `max_steps`.
    pub fn step_out(&mut self, max_steps: u64) -> StepResult {
        let mut steps = 0u64;
        while steps < max_steps {
            let pc = self.pc;
            let arch = self.arch;
            let read = |addr: u32| self.read_u8(addr);
            let insn = disassemble_one(arch, pc, &read);
            if is_return_mnemonic(&insn) {
                // Execute the return so the PC lands on the caller's next
                // instruction; stop there.
                return self.step();
            }
            let result = self.step();
            if result != StepResult::Advanced {
                return result;
            }
            steps += 1;
        }
        StepResult::Advanced
    }


    pub fn read_reg(&self, index: usize) -> u32 {
        if let Some(core) = &self.arm {
            return core.regs[index.min(15)];
        }
        if let Some(core) = &self.mcs51 {
            return match index {
                0 => core.acc as u32,
                1 => core.b as u32,
                2 => core.psw as u32,
                3 => core.sp as u32,
                4 => core.dptr as u32,
                // R0-R7 of the active register bank (RS1:RS0).
                n if n >= 5 && n < 13 => {
                    let bank = ((core.psw >> 3) & 0x03) as usize;
                    core.idram[bank * 8 + (n - 5)] as u32
                }
                _ => 0,
            };
        }
        if let Some(core) = &self.x51 {
            return match index {
                0 => core.acc as u32,
                1 => core.b as u32,
                2 => core.psw as u32,
                3 => core.sp as u32,
                4 => core.dptr as u32,
                n if n >= 5 && n < 21 => core.regs[n - 5] as u32,
                _ => 0,
            };
        }
        if let Some(core) = &self.c166 {
            return core.regs[index.min(15)] as u32;
        }
        if let Some(core) = &self.avr {
            return core.regs[index.min(31)] as u32;
        }
        if let Some(core) = &self.msp430 {
            return core.read_reg(index);
        }
        if let Some(core) = &self.v850 {
            return core.read_reg(index);
        }
        if let Some(core) = &self.stm8 {
            return match index {
                0 => core.a as u32,
                1 => core.x as u32,
                2 => core.y as u32,
                3 => core.sp as u32,
                4 => core.cc as u32,
                _ => 0,
            };
        }
        if let Some(core) = &self.rx {
            return core.read_reg(index);
        }
        if let Some(core) = &self.rl78 {
            return core.read_reg(index);
        }
        if let Some(core) = &self.xtensa {
            return core.reg(index & 15);
        }
        self.registers[index.min(REG_COUNT - 1)]
    }

    /// Write a register value, routing to the active architecture core (the
    /// inverse of `read_reg`). x0 (RISC-V) is read-only and ignored.
    pub fn write_reg(&mut self, index: usize, value: u32) {
        if let Some(core) = &mut self.arm {
            core.regs[index.min(15)] = value;
            return;
        }
        if let Some(core) = &mut self.mcs51 {
            match index {
                0 => core.acc = value as u8,
                1 => core.b = value as u8,
                2 => core.psw = value as u8,
                3 => core.sp = value as u8,
                4 => core.dptr = value as u16,
                n if n >= 5 && n < 13 => {
                    let bank = ((core.psw >> 3) & 0x03) as usize;
                    core.idram[bank * 8 + (n - 5)] = value as u8;
                }
                _ => {}
            }
            return;
        }
        if let Some(core) = &mut self.x51 {
            match index {
                0 => core.acc = value as u16,
                1 => core.b = value as u8,
                2 => core.psw = value as u8,
                3 => core.sp = value as u8,
                4 => core.dptr = value,
                n if n >= 5 && n < 21 => core.regs[n - 5] = value as u16,
                _ => {}
            }
            return;
        }
        if let Some(core) = &mut self.c166 {
            core.regs[index.min(15)] = value as u16;
            return;
        }
        if let Some(core) = &mut self.avr {
            core.regs[index.min(31)] = value as u8;
            return;
        }
        if let Some(core) = &mut self.msp430 {
            core.write_reg(index, value);
            return;
        }
        if let Some(core) = &mut self.v850 {
            core.write_reg(index, value);
            return;
        }
        if let Some(core) = &mut self.stm8 {
            match index {
                0 => core.a = value as u8,
                1 => core.x = value as u16,
                2 => core.y = value as u16,
                3 => core.sp = value as u16,
                4 => core.cc = value as u8,
                _ => {}
            }
            return;
        }
        if let Some(core) = &mut self.rx {
            core.write_reg(index, value);
            return;
        }
        if let Some(core) = &mut self.rl78 {
            core.write_reg(index, value);
            return;
        }
        if let Some(core) = &mut self.xtensa {
            core.set_reg(index & 15, value);
            return;
        }
        if index != 0 && index < REG_COUNT {
            self.registers[index] = value;
        }
    }

    pub fn read_u32(&self, address: u32) -> u32 {
        if let Some(core) = &self.arm {
            return core.read_u32(address);
        }
        if let Some(core) = &self.xtensa {
            return core.read_u32(address);
        }
        if let Some(core) = &self.avr {
            let mut value = 0u32;
            for i in 0..4 {
                value |= (self.read_u8(address.wrapping_add(i as u32)) as u32) << (8 * i);
            }
            let _ = core;
            return value;
        }
        if let Some(core) = &self.msp430 {
            return core.read_u32(address);
        }
        if let Some(core) = &self.v850 {
            return core.read_u32(address);
        }
        if let Some(core) = &self.stm8 {
            return core.read_mem_u32(address);
        }
        if let Some(core) = &self.rx {
            return core.read_u32(address);
        }
        if let Some(core) = &self.rl78 {
            return core.read_u32(address);
        }
        let base = (address as usize) & !3;
        let mut value = 0u32;
        for i in 0..4 {
            value |= (self.memory[base.wrapping_add(i) % MEMORY_SIZE] as u32) << (8 * i);
        }
        value
    }

    pub fn read_u8(&self, address: u32) -> u8 {
        if let Some(core) = &self.arm {
            return core.read_u8(address);
        }
        if let Some(core) = &self.xtensa {
            return core.read_u8(address);
        }
        if let Some(core) = &self.avr {
            return match address {
                0..=0x1F => core.regs[address as usize],
                0x20..=0x5F => core.io[(address as usize) - 0x20],
                0x60.. => {
                    let idx = (address as usize) - 0x60;
                    if idx < core.sram.len() {
                        core.sram[idx]
                    } else {
                        0
                    }
                }
            };
        }
        if let Some(core) = &self.msp430 {
            return core.read_u8(address);
        }
        if let Some(core) = &self.v850 {
            return core.read_u8(address);
        }
        if let Some(core) = &self.stm8 {
            return core.read_mem_u8(address);
        }
        if let Some(core) = &self.rx {
            return core.read_u8(address);
        }
        if let Some(core) = &self.rl78 {
            return core.read_u8(address);
        }
        self.memory[(address as usize) % MEMORY_SIZE]
    }
    pub fn write_u32(&mut self, address: u32, value: u32) {
        if let Some(core) = self.arm.as_mut() {
            core.write_u32(address, value);
            return;
        }
        if let Some(core) = self.xtensa.as_mut() {
            core.write_u32(address, value);
            return;
        }
        if let Some(core) = self.avr.as_mut() {
            for i in 0..4 {
                let byte = (value >> (8 * i)) as u8;
                let addr = address.wrapping_add(i as u32);
                match addr {
                    0..=0x1F => core.regs[addr as usize] = byte,
                    0x20..=0x5F => core.io[(addr as usize) - 0x20] = byte,
                    0x60.. => {
                        let idx = (addr as usize) - 0x60;
                        if idx < core.sram.len() {
                            core.sram[idx] = byte;
                        }
                    }
                }
            }
            return;
        }
        if let Some(core) = self.msp430.as_mut() {
            core.write_u32(address, value);
            return;
        }
        if let Some(core) = self.v850.as_mut() {
            core.write_u32(address, value);
            return;
        }
        if let Some(core) = self.stm8.as_mut() {
            core.write_mem_u32(address, value);
            return;
        }
        if let Some(core) = self.rx.as_mut() {
            core.write_u32(address, value);
            return;
        }
        if let Some(core) = self.rl78.as_mut() {
            core.write_u32(address, value);
            return;
        }
        let base = (address as usize) & !3;
        for i in 0..4 {
            let idx = base.wrapping_add(i) % MEMORY_SIZE;
            self.memory[idx] = (value >> (8 * i)) as u8;
        }
    }

    pub fn write_bytes(&mut self, base: u32, bytes: &[u8]) {
        if let Some(core) = self.arm.as_mut() {
            core.write_bytes(base, bytes);
            return;
        }
        if let Some(core) = self.avr.as_mut() {
            core.load_image(base, bytes);
            return;
        }
        if let Some(core) = self.xtensa.as_mut() {
            core.write_bytes(base, bytes);
            return;
        }
        if let Some(core) = self.msp430.as_mut() {
            core.write_bytes(base, bytes);
            return;
        }
        if let Some(core) = self.v850.as_mut() {
            core.write_bytes(base, bytes);
            return;
        }
        if let Some(core) = self.stm8.as_mut() {
            core.write_mem_bytes(base, bytes);
            return;
        }
        if let Some(core) = self.rx.as_mut() {
            core.write_bytes(base, bytes);
            return;
        }
        if let Some(core) = self.rl78.as_mut() {
            core.write_bytes(base, bytes);
            return;
        }
        for (i, byte) in bytes.iter().enumerate() {
            let idx = (base as usize).wrapping_add(i) % MEMORY_SIZE;
            self.memory[idx] = *byte;
        }
    }


    fn zero_bytes(&mut self, base: u32, count: usize) {
        if let Some(core) = self.arm.as_mut() {
            for i in 0..count {
                core.write_u8(base.wrapping_add(i as u32), 0);
            }
            return;
        }
        if let Some(core) = self.avr.as_mut() {
            for i in 0..count {
                let addr = base as usize + i;
                if addr < core.rom.len() {
                    core.rom[addr] = 0;
                }
            }
            return;
        }
        if let Some(core) = self.xtensa.as_mut() {
            core.zero_bytes(base, count as u32);
            return;
        }
        if let Some(core) = self.msp430.as_mut() {
            core.zero_bytes(base, count as u32);
            return;
        }
        if let Some(core) = self.v850.as_mut() {
            core.zero_bytes(base, count as u32);
            return;
        }
        if let Some(core) = self.stm8.as_mut() {
            core.zero_mem_bytes(base, count as u32);
            return;
        }
        if let Some(core) = self.rx.as_mut() {
            for i in 0..count {
                core.write_u8(base.wrapping_add(i as u32), 0);
            }
            return;
        }
        if let Some(core) = self.rl78.as_mut() {
            core.zero_bytes(base, count as u32);
            return;
        }
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
            0x73 => {
                // SYSTEM: MRET, CSRRW/CSRRS/CSRRC, CSRRWI/CSRRSI/CSRRCI.
                let funct12 = (insn >> 20) & 0xfff;
                let funct3 = (insn >> 12) & 0x7;
                if funct12 == 0x302 && funct3 == 0 && rd == 0 && rs1 == 0 {
                    // MRET: return from machine trap.
                    let mepc = self.csr.mepc;
                    self.csr.mstatus |= 0x8; // MIE restored
                    self.pc = mepc & !1;
                    return;
                }
                let csr = funct12 as u16;
                let old = self.read_csr(csr);
                let new = match funct3 {
                    1 => {
                        // CSRRW: write rs1 into csr, return old.
                        self.registers[rs1]
                    }
                    2 => old | self.registers[rs1], // CSRRS
                    3 => old & !self.registers[rs1], // CSRRC
                    5 => rs1 as u32,                 // CSRRWI
                    6 => old | (rs1 as u32),         // CSRRSI
                    7 => old & !(rs1 as u32),        // CSRRCI
                    _ => old,
                };
                self.write_csr(csr, new);
                self.set_reg(rd, old);
                self.pc = self.pc.wrapping_add(4);
            }
            _ => {
                // Unknown opcode: halt to avoid runaway.
                self.halted = true;
            }
        }
    }
    /// Read a machine-mode CSR by its 12-bit address.
    fn read_csr(&self, csr: u16) -> u32 {
        match csr {
            0x300 => self.csr.mstatus,
            0x305 => self.csr.mtvec,
            0x341 => self.csr.mepc,
            0x342 => self.csr.mcause,
            0x304 => self.csr.mie,
            0xf14 => self.csr.mhartid,
            _ => 0,
        }
    }

    /// Write a machine-mode CSR by its 12-bit address. Read-only CSRs (e.g.
    /// `mhartid`) ignore the write, matching real hardware semantics.
    fn write_csr(&mut self, csr: u16, value: u32) {
        match csr {
            0x300 => self.csr.mstatus = value,
            0x305 => self.csr.mtvec = value,
            0x341 => self.csr.mepc = value,
            0x342 => self.csr.mcause = value,
            0x304 => self.csr.mie = value,
            _ => {}
        }
    }

    /// Take the lowest-numbered pending and enabled interrupt, if the global
    /// interrupt enable (`MIE` bit of `mstatus`) is set. Saves `mepc`/`mcause`
    /// and redirects PC to the trap vector: `mtvec` directly (mode 0) or
    /// `mtvec + 4*line` (vectored mode 1). Returns true if a trap was taken.
    pub fn take_interrupt(&mut self) -> bool {
        if self.csr.mstatus & 0x8 == 0 {
            return false; // MIE clear: interrupts are globally masked.
        }
        let pending = self
            .peripherals
            .irq_pending
            .iter()
            .enumerate()
            .find(|(line, p)| **p && (self.csr.mie >> line) & 1 == 1);
        let Some((line, _)) = pending else {
            return false;
        };
        self.csr.mepc = self.pc;
        self.csr.mcause = 0x8000_0000 | (line as u32);
        self.csr.mstatus &= !0x8; // MIE clear during trap
        let mode = self.csr.mtvec & 0x3;
        let base = self.csr.mtvec & !0x3;
        self.pc = if mode == 1 {
            base.wrapping_add((line as u32).wrapping_mul(4))
        } else {
            base
        };
        true
    }

    /// Whether any interrupt is currently pending and enabled (for UI display).
    pub fn any_pending(&self) -> bool {
        self.peripherals
            .irq_pending
            .iter()
            .enumerate()
            .any(|(line, p)| *p && (self.csr.mie >> line) & 1 == 1)
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
        self.watchdog_write(address);
    }

    /// Handle watchdog register writes: base+0x00 feed (0xAA55 resets the
    /// timer), base+0x04 enable/disable, base+0x08 timeout cycles.
    fn watchdog_write(&mut self, address: u32) {
        // Read the written value first (it borrows `self` immutably through
        // `read_u32`), then take the mutable watchdog borrow.
        let value = self.read_u32(address);
        let Some(watchdog) = self.peripherals.watchdog.as_mut() else {
            return;
        };
        let offset = address.wrapping_sub(watchdog.base);
        if offset >= 0x10 {
            return;
        }
        match offset {
            0x00 => {
                // Feed: write 0xAA55 to reload the countdown. Any other write
                // (e.g. 0x00) leaves the watchdog alone.
                if value & 0xFFFF == 0xAA55 {
                    watchdog.elapsed = 0;
                }
            }
            0x04 => watchdog.enabled = value & 1 != 0,
            0x08 => watchdog.timeout_cycles = value as u64,
            _ => {}
        }
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
                    self.peripherals.gpio_events.push(GpioEvent {
                        cycle: self.cycle_count,
                        pin,
                        value,
                        pc: self.pc,
                    });
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
        if let Some(watchdog) = &self.peripherals.watchdog {
            let offset = address.wrapping_sub(watchdog.base);
            if offset < 0x10 {
                return Some(match offset {
                    0x00 => watchdog.enabled as u32,
                    0x04 => watchdog.elapsed as u32,
                    0x08 => watchdog.reset_count as u32,
                    0x0c => watchdog.last_reset_pc,
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

/// Number of register-file entries the IDE should display for an
/// architecture. RISC-V shows 32 (x0-x31); ARM/Xtensa show 16 (r0-r15 /
/// a0-a15); 8051/80251/C166 show the special registers plus their bank.
pub fn reg_count_for(arch: Arch) -> usize {
    match arch {
        Arch::RiscV => REG_COUNT,
        Arch::Arm | Arch::Xtensa => 16,
        // 8051: ACC/B/PSW/SP/DPTR + R0-R7; 80251: ACC/B/PSW/SP/DPTR + R0-R15;
        // C166: R0-R15.
        Arch::Mcs51 => 13,
        Arch::Mcs251 => 21,
        Arch::C166 => 16,
        Arch::Avr => 32,
        Arch::Msp430 => 16,
        Arch::V850 => 32,
        Arch::Stm8 => 5,
        Arch::Rx => 16,
        Arch::Rl78 => 16,
    }
}

/// Human-readable register label for an architecture and index.
pub fn reg_label_for(arch: Arch, index: usize) -> String {
    match arch {
        Arch::RiscV => reg_name(index).to_string(),
        Arch::Arm => format!("r{}", index.min(15)),
        Arch::Xtensa => format!("a{}", index & 15),
        Arch::Mcs51 => match index {
            0 => "ACC".to_string(),
            1 => "B".to_string(),
            2 => "PSW".to_string(),
            3 => "SP".to_string(),
            4 => "DPTR".to_string(),
            n if n < 13 => format!("R{}", n - 5),
            _ => format!("r{}", index),
        },
        Arch::Mcs251 => match index {
            0 => "ACC".to_string(),
            1 => "B".to_string(),
            2 => "PSW".to_string(),
            3 => "SP".to_string(),
            4 => "DPTR".to_string(),
            n if n < 21 => format!("R{}", n - 5),
            _ => format!("r{}", index),
        },
        Arch::C166 => format!("R{}", index.min(15)),
        Arch::Avr => format!("R{}", index.min(31)),
        Arch::Msp430 => msp430::Msp430Core::register_label(index),
        Arch::V850 => v850::V850Core::register_label(index),
        Arch::Stm8 => match index {
            0 => "A".to_string(),
            1 => "X".to_string(),
            2 => "Y".to_string(),
            3 => "SP".to_string(),
            4 => "CC".to_string(),
            _ => format!("r{}", index),
        },
        Arch::Rx => rx::RxCore::register_label(index),
        Arch::Rl78 => rl78::Rl78Core::register_label(index),
    }

}

/// Disassemble a single instruction at the current PC for an architecture.
/// `read` is a closure returning the byte at an address (the simulator's
/// code space). Returns a mnemonic string like `add a0, a1, a2`.
pub fn disassemble_one(arch: Arch, pc: u32, read: &dyn Fn(u32) -> u8) -> String {
    match arch {
        Arch::RiscV => disassemble_riscv(pc, read),
        Arch::Arm => disassemble_arm_state(pc, read, false),
        Arch::Xtensa => disassemble_xtensa(pc, read),
        Arch::Mcs51 => disassemble_mcs51(pc, read),
        Arch::Mcs251 => disassemble_mcs251(pc, read),
        Arch::C166 => disassemble_c166(pc, read),
        Arch::Avr => avr::disassemble(pc, read),
        Arch::Msp430 => msp430::disassemble(pc, read),
        Arch::V850 => v850::disassemble(pc, read),
        Arch::Stm8 => stm8::disassemble(pc, read),
        Arch::Rx => rx::disassemble(pc, read),
        Arch::Rl78 => rl78::disassemble(pc, read),
    }
}

/// Whether a disassembled mnemonic is a return-class instruction. Used by
/// `step_out` to detect the function epilogue; matches the mnemonics emitted
/// across all supported architectures.
pub fn is_return_mnemonic(insn: &str) -> bool {
    let insn = insn.trim_start();
    for prefix in ["ret", "reti", "rts", "rti", "rfe", "rte", "retf", "mret"] {
        if insn == prefix
            || insn
                .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_whitespace() || c == ','))
        {
            return true;
        }
    }
    // ARM-family returns: `bx lr`/`blx lr` (or any `bx r14`) and
    // `pop {.., pc}` / `ldm/ldmia ... {.., pc}` restore the link register or
    // the program counter, i.e. they leave the current function.
    let body = insn
        .split_once(|c: char| c.is_ascii_whitespace() || c == ',')
        .map(|(m, _)| m)
        .unwrap_or(insn);
    if body == "bx" || body == "blx" {
        let target = insn
            .split_whitespace()
            .nth(1)
            .unwrap_or("")
            .trim_end_matches(',');
        return target == "lr" || target == "r14";
    }
    if body == "pop" || body == "ldm" || body == "ldmia" || body == "ldmfd" {
        return insn.contains("pc") || insn.contains("r15");
    }
    false
}

/// Disassemble one instruction at `pc`, choosing ARM (A32) or Thumb decoding
/// based on `arm_state`. Used by the IDE when the core is an ARM-family CPU
/// (Cortex-M always Thumb; ARM7/ARM9/Cortex-A run A32).
pub fn disassemble_one_state(
    arch: Arch,
    pc: u32,
    read: &dyn Fn(u32) -> u8,
    arm_state: bool,
) -> String {
    if arch == Arch::Arm {
        disassemble_arm_state(pc, read, arm_state)
    } else {
        disassemble_one(arch, pc, read)
    }
}

fn disassemble_riscv(pc: u32, read: &dyn Fn(u32) -> u8) -> String {
    let insn = read(pc) as u32
        | ((read(pc + 1) as u32) << 8)
        | ((read(pc + 2) as u32) << 16)
        | ((read(pc + 3) as u32) << 24);
    let opcode = insn & 0x7f;
    let rd = ((insn >> 7) & 0x1f) as usize;
    let rs1 = ((insn >> 15) & 0x1f) as usize;
    let rs2 = ((insn >> 20) & 0x1f) as usize;
    let funct3 = (insn >> 12) & 0x7;
    let funct7 = (insn >> 25) & 0x7f;
    let imm_i = ((insn as i32) >> 20) as i32;
    let imm_s = sign_extend(
        (((insn >> 7) & 0x1f) | (((insn >> 25) & 0x7f) << 5)) as u32,
        12,
    ) as i32;
    match opcode {
        0x37 => format!("lui {}, 0x{:x}", reg_name(rd), insn >> 12),
        0x17 => format!("auipc {}, 0x{:x}", reg_name(rd), insn >> 12),
        0x6f => {
            let imm = jal_immediate(insn);
            format!("jal {}, {:#x}", reg_name(rd), pc.wrapping_add(imm))
        }
        0x67 => {
            // Canonical `ret` / `jr` encodings: `jalr x0, ra, 0` → "ret",
            // `jalr x0, rs1, 0` → "jr rs1".
            if rd == 0 && imm_i == 0 {
                if rs1 == 1 {
                    "ret".to_string()
                } else {
                    format!("jr {}", reg_name(rs1))
                }
            } else {
                format!("jalr {}, {}, {}", reg_name(rd), reg_name(rs1), imm_i)
            }
        }
        0x63 => {
            let imm = branch_immediate(insn);
            let cond = match funct3 {
                0 => "beq",
                1 => "bne",
                4 => "blt",
                5 => "bge",
                6 => "bltu",
                7 => "bgeu",
                _ => "br",
            };
            format!("{cond} {}, {}, {:#x}", reg_name(rs1), reg_name(rs2), pc.wrapping_add(imm))
        }
        0x03 => {
            let name = match funct3 {
                0 => "lb",
                1 => "lh",
                2 => "lw",
                4 => "lbu",
                5 => "lhu",
                _ => "ld",
            };
            format!("{name} {}, {}({})", reg_name(rd), imm_i, reg_name(rs1))
        }
        0x23 => {
            let name = match funct3 {
                0 => "sb",
                1 => "sh",
                2 => "sw",
                _ => "sd",
            };
            format!("{name} {}, {}({})", reg_name(rs2), imm_s, reg_name(rs1))
        }
        0x13 => {
            let name = match funct3 {
                0 => "addi",
                2 => "slti",
                3 => "sltiu",
                4 => "xori",
                6 => "ori",
                7 => "andi",
                1 => "slli",
                5 if funct7 == 0x20 => "srli",
                5 => "srai",
                _ => "opimm",
            };
            format!("{name} {}, {}, {}", reg_name(rd), reg_name(rs1), imm_i)
        }
        0x33 => {
            let name = match funct3 {
                0 if funct7 == 0x20 => "sub",
                0 => "add",
                1 => "sll",
                2 => "slt",
                3 => "sltu",
                4 => "xor",
                5 if funct7 == 0x20 => "srl",
                5 => "sra",
                6 => "or",
                7 => "and",
                _ => "op",
            };
            format!("{name} {}, {}, {}", reg_name(rd), reg_name(rs1), reg_name(rs2))
        }
        0x0f => {
            if insn & 0xfff == 0x73 {
                "ecall".to_string()
            } else {
                "fence".to_string()
            }
        }
        0x73 => {
            let funct12 = (insn >> 20) & 0xfff;
            if funct12 == 0x302 && funct3 == 0 && rd == 0 && rs1 == 0 {
                // MRET: machine-mode interrupt return.
                return "mret".to_string();
            }
            let csr = ((insn >> 20) & 0xfff) as u16;
            let name = match funct3 {
                1 => "csrrw",
                2 => "csrrs",
                3 => "csrrc",
                5 => "csrrwi",
                6 => "csrrsi",
                7 => "csrrci",
                _ => "csr",
            };
            format!("{name} {}, 0x{:x}, {}", reg_name(rd), csr, reg_name(rs1))
        }
        _ => format!(".word 0x{insn:08x}"),
    }
}

fn disassemble_arm_state(pc: u32, read: &dyn Fn(u32) -> u8, arm_state: bool) -> String {
    if arm_state {
        return disassemble_a32(pc, read);
    }
    let half = read(pc) as u16 | ((read(pc + 1) as u16) << 8);
    let (insn, width) = if half & 0xE000 == 0xE000 {
        let second = read(pc + 2) as u16 | ((read(pc + 3) as u16) << 8);
        (((half as u32) << 16) | second as u32, 4)
    } else {
        (half as u32, 2)
    };
    let _ = width;
    // Thumb-1 16-bit decode (subset mirrored from ArmCore::step).
    match half & 0xF800 {
        0x2000 => format!("movs r{}, #{}", (half >> 8) & 7, half & 0xff),
        0x3000 => format!("adds r{}, #{}", (half >> 8) & 7, half & 0xff),
        // BX / BLX (register): 0x4700-0x477F. `bx lr` is a function return.
        _ if insn & 0xFF00 == 0x4700 => {
            let r = ((half >> 3) & 0x0f) | (((half >> 4) & 0x8));
            let mnem = if half & 0x80 != 0 { "blx" } else { "bx" };
            format!("{mnem} r{}", r)
        }
        // High-register ops / BX fallback (0x4400-0x47FF).
        0x4400 => format!("add r{}, r{}", (half >> 8) & 7, (half >> 3) & 0xf),
        // POP: 0xBC00-0xBDFF. A pop that restores PC leaves the function.
        _ if insn & 0xFE00 == 0xBC00 => {
            let mut regs: Vec<String> = (0..8)
                .filter(|i| half & (1 << i) != 0)
                .map(|i| format!("r{i}"))
                .collect();
            if half & 0x100 != 0 {
                regs.push("pc".to_string());
            }
            format!("pop {{{}}}", regs.join(", "))
        }
        0x6000 => format!("str r{}, [r{}, #{}]", (half >> 8) & 7, (half >> 3) & 7, ((half >> 6) & 0x1f) * 4),
        0x6800 => format!("ldr r{}, [r{}, #{}]", (half >> 8) & 7, (half >> 3) & 7, ((half >> 6) & 0x1f) * 4),
        0xD000 => format!("b{} {:#x}", cond_suffix(((half >> 8) & 0xf) as u8), pc.wrapping_add(4 + ((half as i8 as i32) * 2) as u32)),
        0xE000 => format!("b {:#x}", pc.wrapping_add(4 + (((half & 0x7ff) as i16) * 2) as u32)),
        // Format-2 data-processing (ALU) 0x4000-0x43FF. NOTE: `half & 0xF800`
        // (not `insn & 0xFFFF_8000`, which is always 0 for a 16-bit `insn`).
        _ if half & 0xF800 == 0x4000 => {
            let op = (half >> 6) & 0xf;
            let name = match op {
                0x0 => "ands", 0x1 => "eors", 0x2 => "lsls", 0x3 => "lsrs",
                0x4 => "asrs", 0x5 => "adcs", 0x6 => "sbcs", 0x7 => "rors",
                0x8 => "tst", 0x9 => "rsbs", 0xA => "cmp", 0xB => "cmn",
                0xC => "orrs", 0xD => "muls", 0xE => "bics", _ => "mvns",
            };
            // rd = bits 2-0, rm = bits 5-3 (not bits 10-8/6-3: that is the
            // Format-4 shift/imm encoding, not the ALU register fields).
            format!("{name} r{}, r{}", half & 7, (half >> 3) & 7)
        }
        _ => format!(".hword 0x{half:04x}"),
    }
}

/// Decode a single A32 (ARM-state) instruction for the disassembly view.
fn disassemble_a32(pc: u32, read: &dyn Fn(u32) -> u8) -> String {
    let insn = read(pc) as u32
        | ((read(pc + 1) as u32) << 8)
        | ((read(pc + 2) as u32) << 16)
        | ((read(pc + 3) as u32) << 24);
    let cond = (insn >> 28) as u8;
    let cc = cond_suffix(cond);
    let rn = ((insn >> 16) & 0x0F) as usize;
    let rd = ((insn >> 12) & 0x0F) as usize;
    let rm = (insn & 0x0F) as usize;
    let rname = |r: usize| format!("r{r}");
    let pc_base = pc.wrapping_add(8);
    // Branch / branch-with-link: 101x (cond != 0xF).
    if insn & 0x0E00_0000 == 0x0A00_0000 {
        let link = insn & (1 << 24) != 0;
        let offset = ((insn & 0x00FF_FFFF) << 8) as i32 >> 8;
        let target = pc_base.wrapping_add((offset << 2) as u32);
        let mnem = if link { "bl" } else { "b" };
        return format!("{mnem}{cc} {target:#x}");
    }
    // BX / BLX (register).
    if insn & 0x0FFF_FFF0 == 0x012F_FF10 || insn & 0x0FFF_FFF0 == 0x012F_FF30 {
        let link = insn & 0x20 != 0;
        let mnem = if link { "blx" } else { "bx" };
        return format!("{mnem}{cc} {}", rname(rm));
    }
    // Block data transfer: 100x.
    if insn & 0x0E00_0000 == 0x0800_0000 {
        let pre = insn & (1 << 24) != 0;
        let add = insn & (1 << 23) != 0;
        let writeback = insn & (1 << 21) != 0;
        let load = insn & (1 << 20) != 0;
        let list = insn & 0xFFFF;
        let regs: Vec<String> = (0..16).filter(|i| list & (1 << i) != 0).map(rname).collect();
        let mode = match (add, pre) {
            (true, false) => "ia",
            (true, true) => "ib",
            (false, false) => "da",
            (false, true) => "db",
        };
        let mnem = if load { "ldm" } else { "stm" };
        let wb = if writeback { "!" } else { "" };
        return format!("{mnem}{mode}{cc} {}{wb}, {{{}}}", rname(rn), regs.join(", "));
    }
    // Single data transfer (word/byte): 01xx.
    if insn & 0x0C00_0000 == 0x0400_0000 {
        let reg_off = insn & (1 << 25) != 0;
        let pre = insn & (1 << 24) != 0;
        let add = insn & (1 << 23) != 0;
        let byte = insn & (1 << 22) != 0;
        let writeback = insn & (1 << 21) != 0;
        let load = insn & (1 << 20) != 0;
        let mnem = match (load, byte) {
            (true, false) => "ldr",
            (true, true) => "ldrb",
            (false, false) => "str",
            (false, true) => "strb",
        };
        let offset = if reg_off {
            let shift_imm = (insn >> 7) & 0x1F;
            if shift_imm != 0 {
                format!("{}, lsl #{shift_imm}", rname(rm))
            } else {
                rname(rm)
            }
        } else {
            format!("#{}", insn & 0x0FFF)
        };
        let wb = if writeback { "!" } else { "" };
        // Pre-indexed: [rn, offset]; post-indexed: [rn], offset. Positive
        // offsets omit the `+` sign per ARM assembly convention.
        let off_str = if add { offset } else { format!("-{offset}") };
        if pre {
            return format!("{mnem}{cc} {}, [{}, {off_str}]{wb}", rname(rd), rname(rn));
        }
        return format!("{mnem}{cc} {}, [{}{wb}], {off_str}", rname(rd), rname(rn));
    }
    // MOVW / MOVT.
    if insn & 0x0FF0_0000 == 0x0300_0000 || insn & 0x0FF0_0000 == 0x0340_0000 {
        let movt = insn & 0x0040_0000 != 0;
        let imm16 = ((insn >> 4) & 0xF000) | (insn & 0x0FFF);
        return format!("{}{cc} {}, #{imm16:#x}", if movt { "movt" } else { "movw" }, rname(rd));
    }
    // MRS Rd, CPSR.
    if insn & 0x0FBF_0FFF == 0x010F_0000 {
        return format!("mrs{cc} {}, cpsr", rname(rd));
    }
    // MSR CPSR_<fields>, Rm / #imm.
    if insn & 0x0FB0_FFF0 == 0x0120_F000 {
        let fields = (insn >> 16) & 0x0F;
        return format!("msr{cc} cpsr_{fields:#x}, {}", rname(rm));
    }
    if insn & 0x0FB0_F000 == 0x0320_F000 {
        let fields = (insn >> 16) & 0x0F;
        let imm = (insn & 0xFF).rotate_right(((insn >> 8) & 0x0F) * 2);
        return format!("msr{cc} cpsr_{fields:#x}, #{imm:#x}");
    }
    // Multiply / multiply-accumulate.
    if insn & 0x0FC0_00F0 == 0x0000_0090 {
        let acc = insn & (1 << 21) != 0;
        let rs = ((insn >> 8) & 0x0F) as usize;
        if acc {
            return format!("mla{cc} {}, {}, {}, {}", rname(rd), rname(rm), rname(rs), rname(rn));
        }
        return format!("mul{cc} {}, {}, {}", rname(rd), rname(rm), rname(rs));
    }
    // Long multiply.
    if insn & 0x0F80_00F0 == 0x0080_0090 {
        let rdhi = ((insn >> 16) & 0x0F) as usize;
        let rdlo = ((insn >> 12) & 0x0F) as usize;
        let rs = ((insn >> 8) & 0x0F) as usize;
        let signed = insn & (1 << 22) != 0;
        let acc = insn & (1 << 21) != 0;
        let mnem = match (signed, acc) {
            (false, false) => "umull",
            (false, true) => "umlal",
            (true, false) => "smull",
            (true, true) => "smlal",
        };
        return format!("{mnem}{cc} {}, {}, {}, {}", rname(rdlo), rname(rdhi), rname(rm), rname(rs));
    }
    // Halfword / signed transfer.
    if insn & 0x0E00_0090 == 0x0000_0090 && insn & 0x60 != 0 {
        let load = insn & (1 << 20) != 0;
        let sh = (insn >> 5) & 0x03;
        let mnem = match (load, sh) {
            (true, 1) => "ldrh",
            (true, 2) => "ldrsb",
            (true, _) => "ldrsh",
            (false, _) => "strh",
        };
        let imm = ((insn >> 4) & 0xF0) | (insn & 0x0F);
        let pre = insn & (1 << 24) != 0;
        let add = insn & (1 << 23) != 0;
        // Positive offsets omit the `+` sign per ARM assembly convention.
        let off_str = if add { format!("#{imm}") } else { format!("#-{imm}") };
        if pre {
            return format!("{mnem}{cc} {}, [{}, {off_str}]", rname(rd), rname(rn));
        }
        return format!("{mnem}{cc} {}, [{}], {off_str}", rname(rd), rname(rn));
    }
    // CLZ.
    if insn & 0x0FFF_0FF0 == 0x016F_0F10 {
        return format!("clz{cc} {}, {}", rname(rd), rname(rm));
    }
    // SVC (formerly SWI).
    if insn & 0x0F00_0000 == 0x0F00_0000 {
        return format!("svc{cc} #{:#x}", insn & 0x00FF_FFFF);
    }
    // Data processing (register/immediate).
    if insn & 0x0C00_0000 == 0 {
        let opcode = (insn >> 21) & 0x0F;
        let set_flags = insn & (1 << 20) != 0;
        let s = if set_flags { "s" } else { "" };
        let name = match opcode {
            0x0 => "and", 0x1 => "eor", 0x2 => "sub", 0x3 => "rsb",
            0x4 => "add", 0x5 => "adc", 0x6 => "sbc", 0x7 => "rsc",
            0x8 => "tst", 0x9 => "teq", 0xA => "cmp", 0xB => "cmn",
            0xC => "orr", 0xD => "mov", 0xE => "bic", _ => "mvn",
        };
        let is_imm = insn & (1 << 25) != 0;
        let operand = if is_imm {
            let imm = insn & 0xFF;
            let rotate = ((insn >> 8) & 0x0F) * 2;
            format!("#{:#x}", imm.rotate_right(rotate))
        } else {
            let shift_type = (insn >> 5) & 0x03;
            let amount = (insn >> 7) & 0x1F;
            let shift = match (shift_type, amount) {
                (0, 0) => String::new(),
                (1, 0) => "lsr #32".to_string(),
                (2, 0) => "asr #32".to_string(),
                (3, 0) => "rrx".to_string(),
                (0, a) => format!("lsl #{a}"),
                (1, a) => format!("lsr #{a}"),
                (2, a) => format!("asr #{a}"),
                (_, a) => format!("ror #{a}"),
            };
            if shift.is_empty() {
                rname(rm)
            } else {
                format!("{}, {}", rname(rm), shift)
            }
        };
        if opcode >= 8 && opcode <= 11 {
            // Compare/test: no destination.
            return format!("{name}{cc} {}, {operand}", rname(rn));
        }
        let rd_s = if opcode == 0xD || opcode == 0xF {
            // MOV/MVN have no Rn.
            format!("{}", rname(rd))
        } else {
            format!("{}, {}", rname(rd), rname(rn))
        };
        return format!("{name}{s}{cc} {rd_s}, {operand}");
    }
    format!(".word 0x{insn:08x}")
}

fn cond_suffix(cond: u8) -> &'static str {
    match cond {
        0 => "eq", 1 => "ne", 2 => "cs", 3 => "cc", 4 => "mi", 5 => "pl",
        6 => "vs", 7 => "vc", 8 => "hi", 9 => "ls", 0xA => "ge", 0xB => "lt",
        0xC => "gt", 0xD => "le", _ => "",
    }
}

fn disassemble_xtensa(pc: u32, read: &dyn Fn(u32) -> u8) -> String {
    let b0 = read(pc) as u32;
    let b1 = read(pc + 1) as u32;
    let b2 = read(pc + 2) as u32;
    let insn = (b2 << 16) | (b1 << 8) | b0;
    let op0 = (insn & 0x0F) as u8;
    let op1 = ((insn >> 4) & 0x0F) as u8;
    let r = ((insn >> 8) & 0x0F) as usize;
    let s = ((insn >> 12) & 0x0F) as usize;
    let t = ((insn >> 16) & 0x0F) as usize;
    let op2 = ((insn >> 20) & 0x0F) as u8;
    match (op0, op1) {
        (0x0, 0x2) if insn == 0x20 => "nop".to_string(),
        (0x0, 0x8) => {
            let name = match op2 {
                0x8 => "add", 0x9 => "sub", 0xA => "and", 0xB => "or", 0xC => "xor",
                _ => "rrr",
            };
            format!("{name} a{t}, a{s}, a{r}")
        }
        (0x2, 0xA) => {
            let t = ((insn >> 8) & 0x0F) as usize;
            let imm = ((insn >> 12) & 0xFFF) as i32;
            let imm = ((imm << 20) >> 20) as i32;
            format!("movi a{t}, {imm}")
        }
        (0x2, 0x2) => format!("l32i a{t}, a{s}, {}", (op2 as u32).wrapping_mul(4)),
        (0x2, 0x6) => format!("s32i a{t}, a{s}, {}", (op2 as u32).wrapping_mul(4)),
        (0x2, 0x0) => format!("l8ui a{t}, a{s}, {op2}"),
        (0x2, 0x4) => format!("s8i a{t}, a{s}, {op2}"),
        (0x7, 0x0) => format!("beq a{r}, a{s}, {:#x}", pc.wrapping_add(4).wrapping_add((((insn >> 16) & 0xFF) as u32).wrapping_mul(4))),
        (0x7, 0x4) => format!("bne a{r}, a{s}, {:#x}", pc.wrapping_add(4).wrapping_add((((insn >> 16) & 0xFF) as u32).wrapping_mul(4))),
        (0x7, 0x8) => format!("blt a{r}, a{s}, {:#x}", pc.wrapping_add(4).wrapping_add((((insn >> 16) & 0xFF) as u32).wrapping_mul(4))),
        (0x7, 0xC) => format!("bge a{r}, a{s}, {:#x}", pc.wrapping_add(4).wrapping_add((((insn >> 16) & 0xFF) as u32).wrapping_mul(4))),
        (0x5, 0x0) => {
            let imm = ((insn >> 12) & 0xFFFFF) as u32;
            format!("call0 {:#x}", (pc & !3).wrapping_add(imm.wrapping_mul(4)))
        }
        (0x5, 0x4) => format!("callx0 a{s}"),
        (0x0, 0x0) if r == 0 && op2 == 0 => "ret".to_string(),
        (0x6, 0x0) => format!("mov a{t}, a{s}"),
        (0x4, 0x0) => format!("extui a{t}, a{s}, {}, {}", (insn >> 16) & 0x1f, (insn >> 24) & 0x0f),
        _ => format!(".byte 0x{insn:06x}"),
    }
}

fn disassemble_mcs51(pc: u32, read: &dyn Fn(u32) -> u8) -> String {
    let op = read(pc);
    match op {
        0x00 => "nop".to_string(),
        0x74 => format!("mov a, #0x{:02x}", read(pc + 1)),
        0x90 => format!("mov dptr, #0x{:02x}{:02x}", read(pc + 1), read(pc + 2)),
        0xE0 => "movx a, @dptr".to_string(),
        0xF0 => "movx @dptr, a".to_string(),
        0x04 => "inc a".to_string(),
        0x14 => "dec a".to_string(),
        0xA4 => "mul ab".to_string(),
        0x84 => "div ab".to_string(),
        0x22 => "ret".to_string(),
        0x32 => "reti".to_string(),
        0x02 => format!("ljmp 0x{:02x}{:02x}{:02x}", read(pc + 1), read(pc + 2), read(pc + 3)),
        0x12 => format!("lcall 0x{:02x}{:02x}{:02x}", read(pc + 1), read(pc + 2), read(pc + 3)),
        0x80 => format!("sjmp {:#x}", pc.wrapping_add(2).wrapping_add((read(pc + 1) as i8) as u32)),
        0x60 => format!("jz {:#x}", pc.wrapping_add(2).wrapping_add((read(pc + 1) as i8) as u32)),
        0x70 => format!("jnz {:#x}", pc.wrapping_add(2).wrapping_add((read(pc + 1) as i8) as u32)),
        0x40 => format!("jc {:#x}", pc.wrapping_add(2).wrapping_add((read(pc + 1) as i8) as u32)),
        0x50 => format!("jnc {:#x}", pc.wrapping_add(2).wrapping_add((read(pc + 1) as i8) as u32)),
        n if (0xE8..=0xEF).contains(&n) => format!("mov a, r{}", n & 7),
        n if (0xF8..=0xFF).contains(&n) => format!("mov r{}, a", n & 7),
        n if (0x28..=0x2F).contains(&n) => format!("add a, r{}", n & 7),
        n if (0x98..=0x9F).contains(&n) => format!("subb a, r{}", n & 7),
        n if (0x08..=0x0F).contains(&n) => format!("inc r{}", n & 7),
        n if (0x18..=0x1F).contains(&n) => format!("dec r{}", n & 7),
        n if (0x40..=0x4F).contains(&n) => format!("jc {:#x}", pc.wrapping_add(2).wrapping_add((read(pc + 1) as i8) as u32)),
        _ => format!(".byte 0x{op:02x}"),
    }
}

fn disassemble_mcs251(pc: u32, read: &dyn Fn(u32) -> u8) -> String {
    let op = read(pc);
    match op {
        0x00 => "nop".to_string(),
        n if (0x74..=0x7F).contains(&n) => {
            let hi = read(pc + 1) as u16;
            let lo = read(pc + 2) as u16;
            format!("mov r{}, #0x{:04x}", n & 0x0F, (hi << 8) | lo)
        }
        n if (0xE8..=0xEF).contains(&n) => format!("mov a, r{}", n & 7),
        n if (0xF8..=0xFF).contains(&n) => format!("mov r{}, a", n & 7),
        n if (0x28..=0x2F).contains(&n) => format!("add a, r{}", n & 7),
        n if (0x98..=0x9F).contains(&n) => format!("subb a, r{}", n & 7),
        0xA4 => "mul ab".to_string(),
        0x84 => "div ab".to_string(),
        0x90 => format!("mov dptr, #0x{:02x}{:02x}{:02x}", read(pc + 1), read(pc + 2), read(pc + 3)),
        0x02 => format!("ljmp 0x{:02x}{:02x}{:02x}", read(pc + 1), read(pc + 2), read(pc + 3)),
        0x12 => format!("lcall 0x{:02x}{:02x}{:02x}", read(pc + 1), read(pc + 2), read(pc + 3)),
        0x80 => format!("sjmp {:#x}", pc.wrapping_add(3).wrapping_add((read(pc + 1) as i8) as u32)),
        0x60 => format!("jz {:#x}", pc.wrapping_add(3).wrapping_add((read(pc + 1) as i8) as u32)),
        0x70 => format!("jnz {:#x}", pc.wrapping_add(3).wrapping_add((read(pc + 1) as i8) as u32)),
        0xE0 => "movx a, @dptr".to_string(),
        0xF0 => "movx @dptr, a".to_string(),
        _ => format!(".byte 0x{op:02x}"),
    }
}

fn disassemble_c166(pc: u32, read: &dyn Fn(u32) -> u8) -> String {
    let op = read(pc);
    match op {
        0x00 => "nop".to_string(),
        n if (0x10..=0x1F).contains(&n) => format!("mov r{}, r{}", n & 0x0F, read(pc + 1) & 0x0F),
        n if (0x20..=0x2F).contains(&n) => {
            let hi = read(pc + 1) as u16;
            let lo = read(pc + 2) as u16;
            format!("mov r{}, #0x{:04x}", n & 0x0F, (hi << 8) | lo)
        }
        n if (0x30..=0x3F).contains(&n) => format!("add r{}, r{}", n & 0x0F, read(pc + 1) & 0x0F),
        n if (0x40..=0x4F).contains(&n) => format!("sub r{}, r{}", n & 0x0F, read(pc + 1) & 0x0F),
        n if (0x60..=0x6F).contains(&n) => format!("mul r{}, r{}", n & 0x0F, read(pc + 1) & 0x0F),
        n if (0x70..=0x7F).contains(&n) => format!("div r{}, r{}", n & 0x0F, read(pc + 1) & 0x0F),
        n if (0x80..=0x8F).contains(&n) => format!("load r{}, [r{}]", n & 0x0F, read(pc + 1) & 0x0F),
        n if (0x90..=0x9F).contains(&n) => format!("store [r{}], r{}", n & 0x0F, read(pc + 1) & 0x0F),
        0xA0 => format!("jmp {:#x}", pc.wrapping_add(2).wrapping_add((read(pc + 1) as i8) as u32)),
        0xB0 => format!("jz {:#x}", pc.wrapping_add(2).wrapping_add((read(pc + 1) as i8) as u32)),
        0xB1 => format!("jnz {:#x}", pc.wrapping_add(2).wrapping_add((read(pc + 1) as i8) as u32)),
        0xC0 => format!("call 0x{:02x}{:02x}", read(pc + 1), read(pc + 2)),
        0xC1 => "ret".to_string(),
        _ => format!(".byte 0x{op:02x}"),
    }
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
mod arm_integration {
    use super::*;

    /// Build a minimal ARM ELF32 (EM_ARM=40) with one PT_LOAD segment
    /// carrying `code` bytes at `vaddr`, plus a vector table at `vaddr`.
    fn arm_elf(vaddr: u32, vector_sp: u32, vector_pc: u32, code: &[u8]) -> Vec<u8> {
        let mut elf = vec![0u8; 0x100];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 1; // ELF32
        elf[5] = 1; // little-endian
        elf[6] = 1;
        elf[18] = 40; // e_machine = EM_ARM
        elf[24..28].copy_from_slice(&vaddr.to_le_bytes()); // e_entry
        elf[28..32].copy_from_slice(&52u32.to_le_bytes()); // e_phoff
        elf[42..44].copy_from_slice(&52u16.to_le_bytes()); // e_phentsize
        elf[44..46].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        let ph = 52usize;
        elf[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        elf[ph + 4..ph + 8].copy_from_slice(&0x100u32.to_le_bytes()); // p_offset
        elf[ph + 8..ph + 12].copy_from_slice(&vaddr.to_le_bytes()); // p_vaddr
        elf[ph + 16..ph + 20].copy_from_slice(&(code.len() as u32).to_le_bytes());
        elf[ph + 20..ph + 24].copy_from_slice(&(code.len() as u32).to_le_bytes());
        elf.extend_from_slice(code);
        let _ = (vector_sp, vector_pc);
        elf
    }

    #[test]
    fn for_arch_arm_creates_core() {
        let sim = Simulator::for_arch(Arch::Arm).expect("Arm core");
        assert!(sim.arm.is_some());
        assert_eq!(sim.arch, Arch::Arm);
    }

    #[test]
    fn arm_elf_loads_and_steps() {
        // Segment image: [0]=initial SP, [4]=reset|1, then MOVS r0,#1; MOVS r1,#2.
        let mut img_bytes = vec![0u8; 8];
        img_bytes[0..4].copy_from_slice(&0x2000_0000u32.to_le_bytes());
        img_bytes[4..8].copy_from_slice(&0x0800_0009u32.to_le_bytes());
        img_bytes.extend_from_slice(&[0x01u8, 0x20, 0x02, 0x21]); // 0x2001, 0x2102
        let elf = arm_elf(0x0800_0000, 0x2000_0000, 0x0800_0009, &img_bytes);
        let mut sim = Simulator::for_arch(Arch::Arm).unwrap();
        let entry = sim.load_elf(&elf).unwrap();
        assert_eq!(entry, 0x0800_0000);
        assert_eq!(sim.program_counter(), 0x0800_0008);
        assert_eq!(sim.read_reg(13), 0x2000_0000); // SP from vector
        let r = sim.step();
        assert_eq!(r, StepResult::Advanced);
        assert_eq!(sim.read_reg(0), 1);
        let r = sim.step();
        assert_eq!(r, StepResult::Advanced);
        assert_eq!(sim.read_reg(1), 2);
    }

    #[test]
    fn arm_svc_sets_exception() {
        let mut img_bytes = vec![0u8; 8];
        img_bytes[0..4].copy_from_slice(&0x2000_0000u32.to_le_bytes());
        img_bytes[4..8].copy_from_slice(&0x0800_0009u32.to_le_bytes());
        img_bytes.extend_from_slice(&[0x00u8, 0xDF]); // SVC #0 (0xDF00)
        let elf = arm_elf(0x0800_0000, 0x2000_0000, 0x0800_0009, &img_bytes);
        let mut sim = Simulator::for_arch(Arch::Arm).unwrap();
        sim.load_elf(&elf).unwrap();
        let _ = sim.step();
        assert_eq!(sim.arm.as_ref().unwrap().exception_number(), 11);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_mnemonic_detection() {
        // Return-class mnemonics across architectures are recognized.
        for insn in ["ret", "reti", "rts", "rti", "rfe", "rte", "retf", "mret"] {
            assert!(is_return_mnemonic(insn), "{insn}");
            assert!(is_return_mnemonic(&format!("{insn} #0x10")), "{insn} with operand");
        }
        // ARM-family returns: `bx lr`/`bx r14` and `pop {.., pc}` / `ldm .. {.., pc}`.
        for insn in ["bx lr", "bx r14", "blx lr", "pop {r4, pc}", "pop {pc}", "ldm sp!, {r4, pc}"] {
            assert!(is_return_mnemonic(insn), "{insn}");
        }
        // Non-return instructions are rejected.
        for insn in [
            "add a0, a1",
            "lw a0, 0(a1)",
            "mov A, #0x2A",
            "reti_handler",
            "bx r0",
            "bx r1",
            "pop {r4, r5}",
            "ldr r0, [r1]",
        ] {
            assert!(!is_return_mnemonic(insn), "{insn}");
        }
    }

    /// `step_out` stops at the function epilogue across architectures: RISC-V
    /// `ret`/`mret` (interrupt return) and ARM `bx lr` / `pop {.., pc}`.
    #[test]
    fn step_out_stops_at_returns() {
        // RISC-V: a function that ends with `ret` (jalr x0, ra, 0).
        let mut sim = Simulator::for_arch(Arch::RiscV).expect("RISC-V core");
        sim.load_image(0x8000_0000, &[
            0x13, 0x03, 0x00, 0x00, // addi x6, x0, 0   (0x80000000)
            0x67, 0x00, 0x00, 0x00, // ret              (0x80000004)
        ]);
        sim.set_program_counter(0x8000_0000);
        let result = sim.step_out(64);
        assert_eq!(result, StepResult::Advanced);
        // After the ret executes, PC is the caller's next instruction (ra).
        assert_eq!(sim.program_counter(), 0);

        // ARM Thumb: `bx lr` (0x4770) is a return.
        let mut sim = Simulator::for_arch(Arch::Arm).expect("ARM core");
        // Write the bytes directly (no `load_image`: that would reset the core
        // and treat the code as a vector table, flipping the core to ARM state).
        sim.arm.as_mut().unwrap().set_arm_state(false);
        sim.arm.as_mut().unwrap().write_bytes(0x0800_0000, &[0x70, 0x47]);
        sim.set_program_counter(0x0800_0000);
        sim.write_reg(14, 0x0800_0010); // LR = return address
        let result = sim.step_out(64);
        assert_eq!(result, StepResult::Advanced);
        assert_eq!(sim.program_counter(), 0x0800_0010);
    }

    #[test]
    fn step_out_stops_at_mret_after_interrupt() {
        let mut sim = Simulator::for_arch(Arch::RiscV).expect("RISC-V core");
        // Main loop at 0x80000000; an ISR at 0x80000200 that ends with mret.
        sim.load_image(0x8000_0000, &[0x13, 0x01, 0x00, 0x00]); // nop-ish addi
        sim.load_image(0x8000_0200, &[0x73, 0x00, 0x20, 0x30]); // mret (0x30200073)
        sim.set_program_counter(0x8000_0200);
        sim.csr.mepc = 0x8000_0004;
        // Stepping out of the ISR stops after mret, back in the main loop.
        let result = sim.step_out(64);
        assert_eq!(result, StepResult::Advanced);
        assert_eq!(sim.program_counter(), 0x8000_0004);
    }

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

    /// A timer whose counter is several auto-reloads ahead must fire the exact
    /// number of update events in one `tick()` (not a single bool), so the
    /// `update_count` matches the real overflow count across high-speed runs.
    #[test]
    fn timer_tick_counts_exact_overflows() {
        let mut timer = Timer::new(0x4000_1000, 3);
        timer.enabled = true;
        timer.prescaler = 1;
        timer.auto_reload = 4;
        // Counter already 10 reloads ahead: one tick fires 10 overflows.
        timer.counter = 40;
        let overflows = timer.tick();
        assert_eq!(overflows, 10);
        assert_eq!(timer.update_count, 10);
        assert_eq!(timer.counter, 0);

        // Below the reload: a normal prescaler tick increments the counter.
        let mut timer = Timer::new(0x4000_1000, 3);
        timer.enabled = true;
        timer.prescaler = 1;
        timer.auto_reload = 4;
        timer.counter = 2;
        assert_eq!(timer.tick(), 0);
        assert_eq!(timer.counter, 3);
        assert_eq!(timer.update_count, 0);

        // Exactly at the reload: one overflow, counter returns to 0.
        let mut timer = Timer::new(0x4000_1000, 3);
        timer.enabled = true;
        timer.prescaler = 1;
        timer.auto_reload = 4;
        timer.counter = 4;
        assert_eq!(timer.tick(), 1);
        assert_eq!(timer.counter, 0);
        assert_eq!(timer.update_count, 1);

        // A disabled timer never advances.
        let mut timer = Timer::new(0x4000_1000, 3);
        timer.enabled = false;
        timer.counter = 4;
        assert_eq!(timer.tick(), 0);
        assert_eq!(timer.counter, 4);
        assert_eq!(timer.update_count, 0);
    }

    /// `tick_peripherals_for(n)` must advance timers by exactly `n` core
    /// cycles — cycle-accurate mode advances `cycle_count` by the instruction's
    /// latency, so peripherals must follow the same clock or timers run slow.
    #[test]
    fn tick_peripherals_for_advances_timer_by_cycles() {
        let mut sim = Simulator::new();
        let mut timer = Timer::new(0x4000_1000, 2);
        timer.enabled = true;
        timer.prescaler = 1;
        timer.auto_reload = 4;
        timer.counter = 0;
        sim.peripherals.timers.push(timer);

        // 4 cycles: counter 0→4, no overflow yet.
        sim.tick_peripherals_for(4);
        assert_eq!(sim.peripherals.timers[0].counter, 4);
        assert_eq!(sim.peripherals.timers[0].update_count, 0);
        assert!(!sim.peripherals.irq_pending[2]);

        // One more cycle crosses the reload → overflow + IRQ.
        sim.tick_peripherals_for(1);
        assert_eq!(sim.peripherals.timers[0].update_count, 1);
        assert!(sim.peripherals.irq_pending[2]);
    }

    /// A non-RISC-V architecture (ARM Cortex-M) must advance peripherals on
    /// every `step` — previously the ARM/8051/… branches returned before
    /// `tick_peripherals`, so timers never ran on those targets.
    #[test]
    fn arm_step_advances_peripherals() {
        let mut sim = Simulator::for_arch(Arch::Arm).unwrap();
        let mut timer = Timer::new(0x4000_1000, 1);
        timer.enabled = true;
        timer.prescaler = 1;
        timer.auto_reload = 1;
        timer.counter = 0;
        sim.peripherals.timers.push(timer);

        // Force Thumb state so the two 0x46C0 words decode as Thumb NOPs.
        // Use `write_bytes` (not `load_image`, which resets the vector table
        // and clears the Thumb bit) so PC/state stay under test control.
        sim.arm.as_mut().unwrap().xpsr |= 0x2000_0000;
        sim.write_bytes(0, &[0xC0, 0x46, 0xC0, 0x46]);
        sim.set_program_counter(0);
        let _ = sim.step();
        let _ = sim.step();
        assert!(
            sim.peripherals.timers[0].update_count >= 1,
            "ARM steps must tick peripherals; got {}",
            sim.peripherals.timers[0].update_count
        );
        assert!(sim.peripherals.irq_pending[1]);
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
        // The event must carry the PC of the store that changed the pin.
        let event = sim.peripherals.gpio_events.last().unwrap();
        assert_eq!(event.pc, sim.pc, "event PC must match the current program counter");
        assert_eq!(event.cycle, sim.cycle_count);
        assert_eq!(event.pin, 0);
        assert_eq!(event.value, 1);
        // Clear pin 0.
        sim.write_u32(base + 0x08, 0x0000_0001);
        sim.gpio_write(base + 0x08);
        assert_eq!(sim.peripherals.gpio_out & 1, 0);
    }

    /// The watchdog counts core cycles when enabled; feeding it (0xAA55)
    /// reloads the countdown, and an un-fed watchdog fires a reset whose
    /// `reset_count`/`last_reset_pc` the IDE can surface.
    #[test]
    fn watchdog_fires_when_unfed_and_resets_on_feed() {
        let mut sim = Simulator::new();
        let wd_base = 0x4000_3000;
        sim.peripherals.watchdog = Some(Watchdog::new(wd_base));
        let wd = sim.peripherals.watchdog.as_mut().unwrap();
        assert!(!wd.enabled);

        // Enable and set a 10-cycle timeout.
        sim.write_u32(wd_base + 0x08, 10);
        sim.watchdog_write(wd_base + 0x08);
        sim.write_u32(wd_base + 0x04, 1);
        sim.watchdog_write(wd_base + 0x04);
        assert!(sim.peripherals.watchdog.as_ref().unwrap().enabled);

        // 9 cycles: not yet fired.
        sim.tick_peripherals_for(9);
        assert_eq!(sim.peripherals.watchdog.as_ref().unwrap().reset_count, 0);

        // 1 more cycle crosses the timeout → fires a reset at the current PC.
        sim.pc = 0x1000;
        sim.tick_peripherals_for(1);
        let wd = sim.peripherals.watchdog.as_ref().unwrap();
        assert_eq!(wd.reset_count, 1);
        assert_eq!(wd.last_reset_pc, 0x1000);

        // Feeding (0xAA55) reloads the countdown before the next timeout.
        sim.write_u32(wd_base + 0x00, 0xAA55);
        sim.watchdog_write(wd_base + 0x00);
        sim.tick_peripherals_for(5);
        assert_eq!(sim.peripherals.watchdog.as_ref().unwrap().reset_count, 1);

        // A non-feed write (e.g. 0x00) must not reload the countdown.
        sim.write_u32(wd_base + 0x00, 0x0000);
        sim.watchdog_write(wd_base + 0x00);
        sim.tick_peripherals_for(5);
        assert_eq!(sim.peripherals.watchdog.as_ref().unwrap().reset_count, 2);
    }

    #[test]
    fn timer_reads_back_registers() {
        let mut sim = Simulator::new();
        // Enable and configure a timer at its base.
        sim.peripherals.timers.push(Timer::new(0x4000_1000, 0));
        let base = 0x4000_1000;
        sim.write_u32(base + 0x00, 1);
        sim.timer_write(base + 0x00);
        sim.write_u32(base + 0x04, 4);
        sim.timer_write(base + 0x04);
        sim.write_u32(base + 0x0c, 10);
        sim.timer_write(base + 0x0c);
        sim.tick_peripherals_for(1);
        // The SR (offset 0x10) reflects the timer's update flag.
        assert_eq!(sim.peripheral_read(base + 0x10), Some(0));
        assert_eq!(sim.peripheral_read(base + 0x04), Some(4));
        assert_eq!(sim.peripheral_read(base + 0x0c), Some(10));
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

    #[test]
    fn csr_read_write_and_mret() {
        let mut sim = Simulator::new();
        // CSRRW x5, mtvec, x6 — write 0x1000 to mtvec.
        sim.registers[6] = 0x1000;
        sim.execute(0x73 | (5 << 7) | (0x305 << 20) | (6 << 15) | (1 << 12));
        assert_eq!(sim.csr.mtvec, 0x1000);
        assert_eq!(sim.registers[5], 0, "CSRRW returns old value (0)");
        // CSRRSI x0, mie, 4 — set mie bit 2 (line 2).
        sim.execute(0x73 | (0x304 << 20) | (4 << 15) | (6 << 12));
        assert_eq!(sim.csr.mie, 4);
        // MRET returns to saved mepc.
        sim.csr.mepc = 0x2000;
        sim.csr.mstatus &= !0x8;
        sim.execute(0x3020_0073);
        assert_eq!(sim.pc, 0x2000);
        assert_eq!(sim.csr.mstatus & 0x8, 0x8, "MRET restores MIE");
    }

    #[test]
    fn interrupt_vectors_to_isr() {
        let mut sim = Simulator::new();
        sim.csr.mtvec = 0x8000_0100; // direct mode
        sim.csr.mstatus |= 0x8; // MIE on
        sim.csr.mie = 1 << 3;
        sim.peripherals.irq_pending[3] = true;
        let pc_before = sim.pc;
        assert!(sim.take_interrupt());
        assert_eq!(sim.pc, 0x8000_0100, "jump to trap vector");
        assert_eq!(sim.csr.mepc, pc_before, "save interrupted PC");
        assert_eq!(sim.csr.mcause, 0x8000_0003, "interrupt cause line 3");
        assert_eq!(sim.csr.mstatus & 0x8, 0, "MIE cleared during trap");
    }

    #[test]
    fn interrupt_masked_when_disabled_or_unmatched() {
        let mut sim = Simulator::new();
        sim.csr.mtvec = 0x8000_0100;
        sim.csr.mstatus |= 0x8;
        sim.csr.mie = 1 << 3;
        sim.peripherals.irq_pending[3] = true;
        // MIE clear: no trap.
        sim.csr.mstatus &= !0x8;
        assert!(!sim.take_interrupt());
        assert_eq!(sim.pc, ENTRY_POINT, "no trap when MIE is clear");
        // MIE set but line 3 not enabled in mie: no trap.
        sim.csr.mstatus |= 0x8;
        sim.csr.mie = 1 << 2;
        assert!(!sim.take_interrupt());
        assert_eq!(sim.pc, ENTRY_POINT, "no trap when line not enabled");
    }

    #[test]
    fn vectored_mode_offsets_by_line() {
        let mut sim = Simulator::new();
        sim.csr.mtvec = 0x8000_0100 | 1; // vectored mode
        sim.csr.mstatus |= 0x8;
        sim.csr.mie = 1 << 5;
        sim.peripherals.irq_pending[5] = true;
        assert!(sim.take_interrupt());
        assert_eq!(sim.pc, 0x8000_0100 + 5 * 4, "vectored: base + 4*line");
    }

    #[test]
    fn timer_irq_enters_and_returns() {
        let mut sim = Simulator::new();
        sim.csr.mtvec = 0x8000_0200;
        sim.csr.mstatus |= 0x8;
        sim.csr.mie = 1 << 3;
        sim.peripherals.timers.push(Timer::new(0x4000_1000, 3));
        let base = 0x4000_1000;
        sim.write_u32(base + 0x04, 1);
        sim.timer_write(base + 0x04);
        sim.write_u32(base + 0x0c, 4);
        sim.timer_write(base + 0x0c);
        sim.write_u32(base + 0x00, 1);
        sim.timer_write(base + 0x00);
        // Main loop: an infinite `jal x0, 0` at the entry point, so the core
        // spins in place while the timer advances.
        sim.write_u32(ENTRY_POINT, 0x0000_006f);
        sim.write_u32(0x8000_0200, 0x3020_0073); // ISR: mret
        for _ in 0..7 {
            sim.step();
        }
        // The timer fired, the core trapped into the ISR (mcause line 3), and
        // the ISR returned to the main loop via mret.
        assert!(sim.peripherals.irq_pending[3], "timer IRQ latched");
        assert_eq!(sim.csr.mcause, 0x8000_0003, "entered ISR for line 3");
        assert_eq!(sim.csr.mepc, ENTRY_POINT, "interrupted the main loop PC");
}

    #[test]
    fn arch_maps_from_elf_machine_and_target() {
        assert_eq!(Arch::from_elf_machine(243), Some(Arch::RiscV));
        assert_eq!(Arch::from_elf_machine(40), Some(Arch::Arm));
        assert_eq!(Arch::from_elf_machine(94), Some(Arch::Xtensa));
        assert_eq!(Arch::from_elf_machine(999), None);

        assert_eq!(Arch::from_target("esp32c3"), Arch::RiscV);
        assert_eq!(Arch::from_target("riscv32"), Arch::RiscV);
        assert_eq!(Arch::from_target("stm32f103c8"), Arch::Arm);
        assert_eq!(Arch::from_target("nrf52840"), Arch::Arm);
        assert_eq!(Arch::from_target("rp2040"), Arch::Arm);
        assert_eq!(Arch::from_target("esp32s3"), Arch::Xtensa);
        assert_eq!(Arch::from_target("esp32"), Arch::Xtensa);
        assert_eq!(Arch::from_target("8051"), Arch::Mcs51);
        assert_eq!(Arch::from_target("mcs51"), Arch::Mcs51);
        assert_eq!(Arch::from_target("80251"), Arch::Mcs251);
        assert_eq!(Arch::from_target("mcs251"), Arch::Mcs251);
        assert_eq!(Arch::from_target("c166"), Arch::C166);
        assert_eq!(Arch::from_target("xc166"), Arch::C166);
        assert_eq!(Arch::from_target("xc2000"), Arch::C166);
        assert_eq!(Arch::from_target("lpc2148"), Arch::Arm, "ARM7 maps to Arm");
        assert_eq!(Arch::from_target("s3c2440"), Arch::Arm, "ARM9 maps to Arm");
        assert_eq!(Arch::from_target("at91sam7"), Arch::Arm, "AT91 ARM7 maps to Arm");
        assert_eq!(Arch::from_target("imx6"), Arch::Arm, "i.MX Cortex-A maps to Arm");
        assert_eq!(Arch::from_target("arm926ej-s"), Arch::Arm, "ARM926 maps to Arm");
        // Unknown targets default to the only implemented core.
        assert_eq!(Arch::from_target("custom-riscv"), Arch::RiscV);
    }

    #[test]
    fn for_arch_supports_implemented_cores() {
        assert!(Simulator::for_arch(Arch::RiscV).is_ok());
        assert!(Simulator::for_arch(Arch::Mcs51).is_ok(), "8051 is implemented");
        assert!(Simulator::for_arch(Arch::Arm).is_ok(), "ARM is implemented");
        assert!(
            Simulator::for_arch(Arch::Mcs251).is_ok(),
            "80251 is implemented"
        );
        assert!(Simulator::for_arch(Arch::C166).is_ok(), "C166 is implemented");
        assert!(
            Simulator::for_arch(Arch::Xtensa).is_ok(),
            "Xtensa is implemented"
        );
        assert!(Simulator::for_arch(Arch::Avr).is_ok(), "AVR is implemented");
        assert!(
            Simulator::for_arch(Arch::Msp430).is_ok(),
            "MSP430 is implemented"
        );
        assert!(Simulator::for_arch(Arch::V850).is_ok(), "V850 is implemented");
        assert!(Simulator::for_arch(Arch::Stm8).is_ok(), "STM8 is implemented");
        assert!(Simulator::for_arch(Arch::Rx).is_ok(), "RX is implemented");
        assert!(Simulator::for_arch(Arch::Rl78).is_ok(), "RL78 is implemented");
    }

    #[test]
    fn load_elf_checks_machine_match() {
        // Minimal ELF32 header: e_machine = 243 (RISC-V), class=1, little-endian.
        let mut elf = vec![0u8; 52];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 1; // ELFCLASS32
        elf[5] = 1; // little-endian
        elf[18] = 243; // e_machine lo
        // Same arch loads fine (no program headers, entry 0).
        let mut sim = Simulator::new();
        assert!(sim.load_elf(&elf).is_ok());
        // ARM machine mismatches the RISC-V simulator.
        elf[18] = 40;
        let mut sim = Simulator::new();
        let Err(error) = sim.load_elf(&elf) else {
            panic!("expected machine mismatch error");
        };
        assert!(error.to_string().contains("不匹配"));
    }

    /// Every device-catalog key must map through `from_target` to the
    /// architecture its core belongs to, so a project targeting any catalogued
    /// chip starts the right simulator core.
    #[test]
    fn from_target_maps_every_catalog_key() {
        // (catalog key, expected Arch).
        let pairs = [
            ("stm32f103c8", Arch::Arm),
            ("stm32f407vg", Arch::Arm),
            ("nrf52840", Arch::Arm),
            ("rp2040", Arch::Arm),
            ("stm32g431rb", Arch::Arm),
            ("lpc2148", Arch::Arm),
            ("s3c2440", Arch::Arm),
            ("esp32c3", Arch::RiscV),
            ("esp32", Arch::Xtensa),
            ("esp32s3", Arch::Xtensa),
            ("mcs51", Arch::Mcs51),
            ("80251", Arch::Mcs251),
            ("c166", Arch::C166),
            ("xc166", Arch::C166),
            ("xc2000", Arch::C166),
            ("st10", Arch::C166),
            ("atmega328p", Arch::Avr),
            ("attiny85", Arch::Avr),
            ("at90s8515", Arch::Avr),
            ("msp430g2553", Arch::Msp430),
            ("v850es", Arch::V850),
            ("stm8s003f3", Arch::Stm8),
            ("rx62n", Arch::Rx),
            ("rl78g14", Arch::Rl78),
        ];
        for (key, want) in pairs {
            assert_eq!(
                Arch::from_target(key),
                want,
                "catalog key {key} must map to {want:?}"
            );
        }
    }

    #[test]
    fn x51_core_loads_and_steps_via_simulator() {
        let mut sim = Simulator::for_arch(Arch::Mcs251).unwrap();
        // MOV R4, #0x1234 (0x74, 0x12, 0x34); then NOP.
        let image = [0x74, 0x12, 0x34, 0x00];
        sim.load_x51(0, &image).unwrap();
        assert_eq!(sim.program_counter(), 0);
        let r = sim.step();
        assert_eq!(r, StepResult::Advanced);
        assert_eq!(sim.read_reg(9), 0x1234); // regs[4] is index 9 (0-3 special, 4=dptr)
        assert_eq!(sim.program_counter(), 3);
    }

    #[test]
    fn x51_breakpoint_stops_before_execute() {
        let mut sim = Simulator::for_arch(Arch::Mcs251).unwrap();
        sim.load_x51(0, &[0x00, 0x00]).unwrap();
        sim.breakpoints.push(0);
        let r = sim.step();
        assert_eq!(r, StepResult::Breakpoint);
        assert_eq!(sim.program_counter(), 0, "breakpoint stops before opcode");
    }

    #[test]
    fn c166_core_loads_and_steps_via_simulator() {
        let mut sim = Simulator::for_arch(Arch::C166).unwrap();
        // MOV R1, #0xABCD (0x21, 0xCD, 0xAB); then NOP.
        let image = [0x21, 0xCD, 0xAB, 0x00];
        sim.load_c166(0, &image).unwrap();
        assert_eq!(sim.program_counter(), 0);
        let r = sim.step();
        assert_eq!(r, StepResult::Advanced);
        assert_eq!(sim.read_reg(1), 0xABCD);
        assert_eq!(sim.program_counter(), 3);
    }

    #[test]
    fn c166_breakpoint_stops_before_execute() {
        let mut sim = Simulator::for_arch(Arch::C166).unwrap();
        sim.load_c166(0, &[0x00, 0x00]).unwrap();
        sim.breakpoints.push(0);
        let r = sim.step();
        assert_eq!(r, StepResult::Breakpoint);
        assert_eq!(sim.program_counter(), 0, "breakpoint stops before opcode");
    }

    #[test]
    fn xtensa_core_loads_and_steps_via_simulator() {
        let mut sim = Simulator::for_arch(Arch::Xtensa).unwrap();
        // MOVI a2, 5 (op0=2, op1=A, t=2 bits 8-11, imm=5 bits 12-23).
        let word = 0x2u32 | (0xAu32 << 4) | (2u32 << 8) | (5u32 << 12);
        let image = [word as u8, (word >> 8) as u8, (word >> 16) as u8];
        sim.load_image(0, &image);
        assert_eq!(sim.program_counter(), 0);
        let r = sim.step();
        assert_eq!(r, StepResult::Advanced);
        assert_eq!(sim.read_reg(2), 5);
        assert_eq!(sim.program_counter(), 3);
    }

    #[test]
    fn xtensa_breakpoint_stops_before_execute() {
        let mut sim = Simulator::for_arch(Arch::Xtensa).unwrap();
        sim.load_image(0, &[0x00, 0x00, 0x00]);
        sim.breakpoints.push(0);
        let r = sim.step();
        assert_eq!(r, StepResult::Breakpoint);
        assert_eq!(sim.program_counter(), 0, "breakpoint stops before opcode");
    }

    #[test]
    fn xtensa_load_elf_accepts_machine_94() {
        // Minimal ELF32 header with e_machine = 94 (EM_XTENSA).
        let mut elf = vec![0u8; 52];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 1; // ELFCLASS32
        elf[5] = 1; // little-endian
        elf[18] = 94; // e_machine lo
        let mut sim = Simulator::for_arch(Arch::Xtensa).unwrap();
        assert!(sim.load_elf(&elf).is_ok(), "EM_XTENSA ELF loads");
    }

    #[test]
    fn disassembles_riscv_instructions() {
        // add a0, a1, a2 (opcode 0x33, funct3=0, rd=10, rs1=11, rs2=12).
        let insn = 0x00C58533u32;
        let bytes = [insn as u8, (insn >> 8) as u8, (insn >> 16) as u8, (insn >> 24) as u8];
        let read = |addr: u32| {
            let i = addr as usize;
            if i < 4 { bytes[i] } else { 0 }
        };
        let text = disassemble_one(Arch::RiscV, 0, &read);
        assert_eq!(text, "add a0, a1, a2");
        // addi a0, a1, 5 (opcode 0x13, funct3=0, rd=10, rs1=11, imm=5).
        let insn = 0x00558513u32;
        let bytes = [insn as u8, (insn >> 8) as u8, (insn >> 16) as u8, (insn >> 24) as u8];
        let read = |addr: u32| {
            let i = addr as usize;
            if i < 4 { bytes[i] } else { 0 }
        };
        assert_eq!(disassemble_one(Arch::RiscV, 0, &read), "addi a0, a1, 5");
    }

    #[test]
    fn disassembles_xtensa_and_8051() {
        // Xtensa MOVI a2, 5.
        let word = 0x2u32 | (0xAu32 << 4) | (2u32 << 8) | (5u32 << 12);
        let bytes = [word as u8, (word >> 8) as u8, (word >> 16) as u8];
        let read = |addr: u32| {
            let i = addr as usize;
            if i < 3 { bytes[i] } else { 0 }
        };
        assert_eq!(disassemble_one(Arch::Xtensa, 0, &read), "movi a2, 5");
        // 8051 NOP.
        let read = |addr: u32| if addr == 0 { 0x00 } else { 0 };
        assert_eq!(disassemble_one(Arch::Mcs51, 0, &read), "nop");
        // 8051 MOV A, R0.
        let read = |addr: u32| if addr == 0 { 0xE8 } else { 0 };
        assert_eq!(disassemble_one(Arch::Mcs51, 0, &read), "mov a, r0");
        // C166 MOV R0, R2.
        let bytes = [0x10, 0x02];
        let read = |addr: u32| {
            let i = addr as usize;
            if i < 2 { bytes[i] } else { 0 }
        };
        assert_eq!(disassemble_one(Arch::C166, 0, &read), "mov r0, r2");
    }

    #[test]
    fn disassembles_a32_instructions() {
        // MOV r0, #0x1234 (MOVW r0, #0x1234 => 0xE3010234) disassembles in
        // ARM state but not in Thumb state.
        let bytes = [0x34, 0x02, 0x01, 0xE3];
        let read = |addr: u32| {
            let i = addr as usize;
            if i < 4 { bytes[i] } else { 0 }
        };
        let a32 = disassemble_one_state(Arch::Arm, 0, &read, true);
        assert_eq!(a32, "movw r0, #0x1234");
        // The same bytes in Thumb state decode differently (Thumb-1 group).
        let thumb = disassemble_one_state(Arch::Arm, 0, &read, false);
        assert_ne!(thumb, a32, "ARM state changes the decode");
        // ADD r0, r1, r2 (0xE0810002).
        let insn = 0xE081_0002u32;
        let bytes = [insn as u8, (insn >> 8) as u8, (insn >> 16) as u8, (insn >> 24) as u8];
        let read = |addr: u32| {
            let i = addr as usize;
            if i < 4 { bytes[i] } else { 0 }
        };
        assert_eq!(
            disassemble_one_state(Arch::Arm, 0, &read, true),
            "add r0, r1, r2"
        );
        // LDR r0, [r1, #4] (0xE5910004).
        let insn = 0xE591_0004u32;
        let bytes = [insn as u8, (insn >> 8) as u8, (insn >> 16) as u8, (insn >> 24) as u8];
        let read = |addr: u32| {
            let i = addr as usize;
            if i < 4 { bytes[i] } else { 0 }
        };
        assert_eq!(
            disassemble_one_state(Arch::Arm, 0, &read, true),
            "ldr r0, [r1, #4]"
        );
        // B +0 (0xEA000000) branches to pc+8 = 0x8.
        let insn = 0xEA00_0000u32;
        let bytes = [insn as u8, (insn >> 8) as u8, (insn >> 16) as u8, (insn >> 24) as u8];
        let read = |addr: u32| {
            let i = addr as usize;
            if i < 4 { bytes[i] } else { 0 }
        };
        assert_eq!(disassemble_one_state(Arch::Arm, 0, &read, true), "b 0x8");
    }
    #[test]
    fn disassembles_thumb_alu_instructions() {
        // Regression: the Thumb-1 format-2 ALU guard was `insn & 0xFFFF_8000
        // == 0x4000`, always false for a 16-bit `insn`, so every ALU op fell
        // through to `.hword`. Guard must mask with 0xF800 on `half`.
        let cases: &[(u16, &str)] = &[
            (0x4000, "ands r0, r0"),
            (0x4011, "ands r1, r2"),
            (0x4091, "lsls r1, r2"),
            (0x4100, "asrs r0, r0"),
            (0x4200, "tst r0, r0"),
            (0x4291, "cmp r1, r2"),
            (0x4351, "muls r1, r2"),
            (0x43d1, "mvns r1, r2"),
        ];
        for (insn, expected) in cases {
            let bytes = [*insn as u8, (*insn >> 8) as u8];
            let read = |addr: u32| {
                let i = addr as usize;
                if i < 2 { bytes[i] } else { 0 }
            };
            assert_eq!(
                disassemble_one_state(Arch::Arm, 0, &read, false),
                *expected,
                "insn {insn:#06x}"
            );
        }
    }


    #[test]
    fn register_metadata_per_architecture() {
        assert_eq!(reg_count_for(Arch::RiscV), 32);
        assert_eq!(reg_count_for(Arch::Arm), 16);
        assert_eq!(reg_count_for(Arch::Xtensa), 16);
        assert_eq!(reg_count_for(Arch::Mcs51), 13);
        assert_eq!(reg_count_for(Arch::Mcs251), 21);
        assert_eq!(reg_count_for(Arch::C166), 16);
        assert_eq!(reg_label_for(Arch::Mcs51, 0), "ACC");
        assert_eq!(reg_label_for(Arch::Mcs51, 5), "R0");
        assert_eq!(reg_label_for(Arch::Mcs251, 5), "R0");
        assert_eq!(reg_label_for(Arch::C166, 3), "R3");
        assert_eq!(reg_label_for(Arch::Xtensa, 2), "a2");
        assert_eq!(reg_label_for(Arch::Arm, 3), "r3");
        // Newer Keil/IAR architectures.
        assert_eq!(reg_count_for(Arch::Avr), 32);
        assert_eq!(reg_count_for(Arch::Msp430), 16);
        assert_eq!(reg_count_for(Arch::V850), 32);
        assert_eq!(reg_count_for(Arch::Stm8), 5);
        assert_eq!(reg_count_for(Arch::Rx), 16);
        assert_eq!(reg_count_for(Arch::Rl78), 16);
        assert_eq!(reg_label_for(Arch::Avr, 3), "R3");
        assert_eq!(reg_label_for(Arch::Msp430, 1), "SP");
        assert_eq!(reg_label_for(Arch::V850, 3), "sp");
        assert_eq!(reg_label_for(Arch::Stm8, 0), "A");
        assert_eq!(reg_label_for(Arch::Rx, 0), "r0");
        assert_eq!(reg_label_for(Arch::Rl78, 0), "X");
    }

    #[test]
    fn write_reg_routes_per_architecture() {
        let mut sim = Simulator::for_arch(Arch::RiscV).unwrap();
        sim.write_reg(5, 0x1234);
        assert_eq!(sim.read_reg(5), 0x1234, "RISC-V register write");
        sim.write_reg(0, 0xdead);
        assert_eq!(sim.read_reg(0), 0, "RISC-V x0 is read-only");

        let mut sim = Simulator::for_arch(Arch::Arm).unwrap();
        sim.write_reg(3, 0xabcd);
        assert_eq!(sim.read_reg(3), 0xabcd, "ARM register write");

        let mut sim = Simulator::for_arch(Arch::Xtensa).unwrap();
        sim.write_reg(2, 0x99);
        assert_eq!(sim.read_reg(2), 0x99, "Xtensa register write");

        let mut sim = Simulator::for_arch(Arch::C166).unwrap();
        sim.write_reg(4, 0x77);
        assert_eq!(sim.read_reg(4), 0x77, "C166 register write");
    }

    #[test]
    fn write_reg_mcs51_and_x51_special_registers() {
        let mut sim = Simulator::for_arch(Arch::Mcs51).unwrap();
        sim.write_reg(0, 0x55); // ACC
        assert_eq!(sim.read_reg(0), 0x55, "mcs51 ACC write");
        sim.write_reg(5, 0x12); // R0 (bank 0)
        assert_eq!(sim.read_reg(5), 0x12, "mcs51 R0 write");

        let mut sim = Simulator::for_arch(Arch::Mcs251).unwrap();
        sim.write_reg(2, 0x30); // PSW
        assert_eq!(sim.read_reg(2), 0x30, "80251 PSW write");
    }

    #[test]
    fn set_program_counter_routes_per_architecture() {
        let mut sim = Simulator::for_arch(Arch::RiscV).unwrap();
        sim.set_program_counter(0x8000);
        assert_eq!(sim.program_counter(), 0x8000, "RISC-V PC set");

        let mut sim = Simulator::for_arch(Arch::Arm).unwrap();
        sim.set_program_counter(0x0800_1234);
        assert_eq!(sim.program_counter(), 0x0800_1234, "ARM PC set");

        let mut sim = Simulator::for_arch(Arch::Mcs51).unwrap();
        sim.set_program_counter(0x0400);
        assert_eq!(sim.program_counter(), 0x0400, "8051 PC set");

        let mut sim = Simulator::for_arch(Arch::Xtensa).unwrap();
        sim.set_program_counter(0x4000_0100);
        assert_eq!(sim.program_counter(), 0x4000_0100, "Xtensa PC set");
    }

    #[test]
    fn avr_core_loads_and_steps_via_simulator() {
        let mut sim = Simulator::for_arch(Arch::Avr).unwrap();
        // AVR NOP (0x0000) is a single 16-bit word.
        sim.load_avr(0, &[0x00, 0x00]).unwrap();
        let result = sim.step();
        assert_eq!(result, StepResult::Advanced);
        assert_eq!(sim.program_counter(), 2, "AVR PC advances by 2 bytes");
    }

    #[test]
    fn avr_breakpoint_stops_before_execute() {
        let mut sim = Simulator::for_arch(Arch::Avr).unwrap();
        sim.load_avr(0, &[0x00, 0x00]).unwrap();
        sim.set_breakpoint(0);
        let result = sim.step();
        assert_eq!(result, StepResult::Breakpoint);
        assert_eq!(sim.program_counter(), 0, "breakpoint at PC 0");
    }

    #[test]
    fn msp430_core_loads_and_steps_via_simulator() {
        let mut sim = Simulator::for_arch(Arch::Msp430).unwrap();
        // MSP430 NOP is 0x4303 (MOV #0, R3) — actually MOV r3, r3 = 0x4303.
        sim.load_msp430(0, &[0x03, 0x43]).unwrap();
        let result = sim.step();
        assert_eq!(result, StepResult::Advanced);
        assert_eq!(sim.program_counter(), 2, "MSP430 PC advances by 2");
    }

    #[test]
    fn v850_core_loads_and_steps_via_simulator() {
        let mut sim = Simulator::for_arch(Arch::V850).unwrap();
        // V850 NOP (0x0000).
        sim.load_v850(0, &[0x00, 0x00]).unwrap();
        let result = sim.step();
        assert_eq!(result, StepResult::Advanced);
        assert_eq!(sim.program_counter(), 2, "V850 PC advances by 2");
    }

    #[test]
    fn avr_msp430_v850_registers_and_disassembly() {
        let mut sim = Simulator::for_arch(Arch::Avr).unwrap();
        sim.write_reg(5, 0xAB);
        assert_eq!(sim.read_reg(5), 0xAB, "AVR register write");

        let mut sim = Simulator::for_arch(Arch::Msp430).unwrap();
        sim.write_reg(4, 0x1234);
        assert_eq!(sim.read_reg(4), 0x1234, "MSP430 register write");

        let mut sim = Simulator::for_arch(Arch::V850).unwrap();
        sim.write_reg(4, 0xDEAD);
        assert_eq!(sim.read_reg(4), 0xDEAD, "V850 register write");
        // V850 NOP disassembles.
        let bytes = [0x00, 0x00];
        let read = |addr: u32| {
            let i = addr as usize;
            if i < 2 { bytes[i] } else { 0 }
        };
        assert_eq!(disassemble_one(Arch::V850, 0, &read), "nop");
    }

    #[test]
    fn new_archs_supported_by_for_arch_and_labels() {
        for arch in [Arch::Avr, Arch::Msp430, Arch::V850, Arch::Stm8] {
            assert!(Simulator::for_arch(arch).is_ok(), "{arch:?} for_arch");
        }
        assert_eq!(reg_count_for(Arch::Avr), 32);
        assert_eq!(reg_count_for(Arch::Msp430), 16);
        assert_eq!(reg_count_for(Arch::V850), 32);
        assert_eq!(reg_count_for(Arch::Stm8), 5);
        assert_eq!(reg_label_for(Arch::Avr, 3), "R3");
        assert_eq!(reg_label_for(Arch::V850, 3), "sp");
        assert_eq!(reg_label_for(Arch::Stm8, 0), "A");
    }

    #[test]
    fn from_elf_machine_maps_new_architectures() {
        assert_eq!(Arch::from_elf_machine(83), Some(Arch::Avr)); // EM_AVR
        assert_eq!(Arch::from_elf_machine(167), Some(Arch::Msp430)); // EM_MSP430_2
        assert_eq!(Arch::from_elf_machine(87), Some(Arch::V850)); // EM_V850
        assert_eq!(Arch::from_elf_machine(186), Some(Arch::Stm8)); // EM_STM8
    }

    #[test]
    fn stm8_core_loads_and_steps_via_simulator() {
        let mut sim = Simulator::for_arch(Arch::Stm8).unwrap();
        // LD A, #0x55 (0xA6, 0x55); then NOP (0x9D).
        let image = [0xA6, 0x55, 0x9D];
        sim.load_stm8(0x8000, &image).unwrap();
        assert_eq!(sim.program_counter(), 0x8000);
        let result = sim.step();
        assert_eq!(result, StepResult::Advanced);
        assert_eq!(sim.read_reg(0), 0x55, "STM8 A register");
        assert_eq!(sim.program_counter(), 0x8002);
    }

    #[test]
    fn stm8_breakpoint_stops_before_execute() {
        let mut sim = Simulator::for_arch(Arch::Stm8).unwrap();
        sim.load_stm8(0x8000, &[0x9D]).unwrap();
        sim.set_breakpoint(0x8000);
        let result = sim.step();
        assert_eq!(result, StepResult::Breakpoint);
        assert_eq!(sim.program_counter(), 0x8000, "breakpoint at entry");
    }

    #[test]
    fn stm8_alu_add_and_store() {
        let mut sim = Simulator::for_arch(Arch::Stm8).unwrap();
        // LD A, #10 (A6 0A); ADD A, #5 (AB 05); LD 0x2000, A (C7 00 20).
        let image = [0xA6, 0x0A, 0xAB, 0x05, 0xC7, 0x00, 0x20];
        sim.load_stm8(0x8000, &image).unwrap();
        for _ in 0..3 {
            assert_eq!(sim.step(), StepResult::Advanced);
        }
        assert_eq!(sim.read_reg(0), 15, "A = 10 + 5");
        assert_eq!(sim.read_u8(0x2000), 15, "store to direct address");
    }

    /// Cycle-accurate modeling must advance `cycle_count` by the instruction's
    /// estimated latency (loads/stores/branches > 1), while the default mode
    /// stays at 1 cycle per instruction.
    #[test]
    fn cycle_accurate_accounts_instruction_latency() {
        let mut sim = Simulator::new();
        sim.memory[0] = 0x13; // ADDI x1, x0, 1 (1 cycle)
        sim.memory[4] = 0x23; // SW x1, 0(x2) (store, 2 cycles)
        sim.pc = 0;
        // Default: 1 cycle per instruction regardless of type.
        assert_eq!(sim.step(), StepResult::Advanced);
        assert_eq!(sim.cycle_count, 1);
        assert_eq!(sim.last_instruction_cycles, 1);
        sim.pc = 4;
        assert_eq!(sim.step(), StepResult::Advanced);
        assert_eq!(sim.cycle_count, 2);
        // Cycle-accurate: the store costs 2 cycles.
        let mut sim = Simulator::new();
        sim.cycle_accurate = true;
        sim.memory[0] = 0x13;
        sim.memory[4] = 0x23;
        sim.pc = 0;
        assert_eq!(sim.step(), StepResult::Advanced);
        assert_eq!(sim.cycle_count, 1);
        assert_eq!(sim.last_instruction_cycles, 1);
        sim.pc = 4;
        assert_eq!(sim.step(), StepResult::Advanced);
        assert_eq!(sim.cycle_count, 3, "store adds 2 cycles");
        assert_eq!(sim.last_instruction_cycles, 2);
    }
}
