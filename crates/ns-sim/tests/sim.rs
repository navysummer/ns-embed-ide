use ns_sim::{Simulator, StepResult, ENTRY_POINT, reg_name};

/// Assemble a minimal RV32I instruction.
fn r_type(funct7: u32, rs2: u32, rs1: u32, funct3: u32, rd: u32, opcode: u32) -> u32 {
    (funct7 << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode
}

fn i_type(imm: u32, rs1: u32, funct3: u32, rd: u32, opcode: u32) -> u32 {
    ((imm & 0xfff) << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode
}

fn jal_imm(imm: u32, rd: u32) -> u32 {
    // JAL with a small positive offset.
    let imm20 = (imm >> 20) & 1;
    let imm10_1 = (imm >> 1) & 0x3ff;
    let imm11 = (imm >> 11) & 1;
    let imm19_12 = (imm >> 12) & 0xff;
    (imm20 << 31)
        | (imm10_1 << 21)
        | (imm11 << 20)
        | (imm19_12 << 12)
        | (rd << 7)
        | 0x6f
}

fn beq(rs1: u32, rs2: u32, byte_offset: i32, funct3: u32) -> u32 {
    // B-type: imm[12:1] = byte_offset >> 1 (bit 0 always 0).
    // In the 12-bit imm field: imm12=bit11, imm11=bit10, imm10:5=bits9:4, imm4:1=bits3:0.
    let imm = (byte_offset >> 1) as u32 & 0xfff;
    let imm12 = (imm >> 11) & 1;
    let imm10_5 = (imm >> 4) & 0x3f;
    let imm4_1 = imm & 0xf;
    let imm11 = (imm >> 10) & 1;
    (imm12 << 31)
        | (imm10_5 << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (funct3 << 12)
        | (imm4_1 << 8)
        | (imm11 << 7)
        | 0x63
}
fn sw(rs2: u32, rs1: u32, imm: u32) -> u32 {
    // S-type: imm[11:5] -> insn[31:25], imm[4:0] -> insn[11:7].
    let imm11_5 = (imm >> 5) & 0x7f;
    let imm4_0 = imm & 0x1f;
    (imm11_5 << 25) | (rs2 << 20) | (rs1 << 15) | (2 << 12) | (imm4_0 << 7) | 0x23
}
fn lw(rs1: u32, imm: u32, rd: u32) -> u32 {
    i_type(imm, rs1, 2, rd, 0x03)
}

#[test]
fn executes_add_and_sub() {
    let mut sim = Simulator::new();
    // x1 = 5; x2 = 3; x3 = x1 + x2; x4 = x1 - x2;
    let prog = [
        i_type(5, 0, 0, 1, 0x13), // addi x1, x0, 5
        i_type(3, 0, 0, 2, 0x13), // addi x2, x0, 3
        r_type(0, 2, 1, 0, 3, 0x33), // add x3, x1, x2
        r_type(0x20, 2, 1, 0, 4, 0x33), // sub x4, x1, x2
    ];
    let bytes: Vec<u8> = prog.iter().flat_map(|i| i.to_le_bytes()).collect();
    sim.load_image(ENTRY_POINT, &bytes);
    sim.run(10);
    assert_eq!(sim.read_reg(1), 5);
    assert_eq!(sim.read_reg(2), 3);
    assert_eq!(sim.read_reg(3), 8);
    assert_eq!(sim.read_reg(4), 2);
}

#[test]
fn executes_loop_summing_to_n() {
    let mut sim = Simulator::new();
    // Compute sum of 1..=10 into x5 using a loop.
    // x1 = 10 (limit), x2 = 1 (i), x3 = 0 (sum), x5 = 0 (acc)
    // loop: sum += i; i += 1; if i <= limit goto loop
    let prog = [
        i_type(10, 0, 0, 1, 0x13),   // addi x1, x0, 10
        i_type(1, 0, 0, 2, 0x13),    // addi x2, x0, 1
        i_type(0, 0, 0, 3, 0x13),    // addi x3, x0, 0
        r_type(0, 2, 3, 0, 3, 0x33), // add x3, x3, x2   (sum += i)
        i_type(1, 2, 0, 2, 0x13),    // addi x2, x2, 1   (i += 1)
        beq(1, 2, -8, 5),            // bge x1, x2, loop (8 bytes back)
    ];
    let bytes: Vec<u8> = prog.iter().flat_map(|i| i.to_le_bytes()).collect();
    sim.load_image(ENTRY_POINT, &bytes);
    sim.run(1000);
    // Sum 1..=10 = 55.
    assert_eq!(sim.read_reg(3), 55, "expected sum 55, got {}", sim.read_reg(3));
}

#[test]
fn stops_at_breakpoint() {
    let mut sim = Simulator::new();
    let prog = [
        i_type(1, 0, 0, 1, 0x13),
        i_type(2, 0, 0, 2, 0x13),
        i_type(3, 0, 0, 3, 0x13),
    ];
    let bytes: Vec<u8> = prog.iter().flat_map(|i| i.to_le_bytes()).collect();
    sim.load_image(ENTRY_POINT, &bytes);
    let bp = ENTRY_POINT + 8;
    sim.set_breakpoint(bp);
    let result = sim.run(100);
    assert_eq!(result, StepResult::Breakpoint);
    assert_eq!(sim.pc, bp);
    // x1 and x2 executed; x3 not yet.
    assert_eq!(sim.read_reg(1), 1);
    assert_eq!(sim.read_reg(2), 2);
    assert_eq!(sim.read_reg(3), 0);
}

#[test]
fn loads_stores_memory() {
    let mut sim = Simulator::new();
    let prog = [
        i_type(0x234, 0, 0, 1, 0x13), // addi x1, x0, 0x234 (fits in 12-bit signed)
        i_type(0x200, 0, 0, 2, 0x13), // addi x2, x0, 0x200
        sw(1, 2, 0),                  // sw x1, 0(x2)
        lw(2, 0, 3),                  // lw x3, 0(x2)
    ];
    let bytes: Vec<u8> = prog.iter().flat_map(|i| i.to_le_bytes()).collect();
    sim.load_image(ENTRY_POINT, &bytes);
    sim.run(10);
    assert_eq!(sim.read_reg(3), 0x234);
}

#[test]
fn halts_on_ecall() {
    let mut sim = Simulator::new();
    // ecall with a7=0 halts.
    let prog = [
        r_type(0, 0, 0, 0, 17, 0x33), // add x17, x0, x0  (a7 = 0)
        0x0000_0073,                  // ecall
    ];
    let bytes: Vec<u8> = prog.iter().flat_map(|i| i.to_le_bytes()).collect();
    sim.load_image(ENTRY_POINT, &bytes);
    let result = sim.run(100);
    assert_eq!(result, StepResult::Halted);
    assert!(sim.halted);
}

#[test]
fn register_names_are_stable() {
    assert_eq!(reg_name(0), "zero");
    assert_eq!(reg_name(1), "ra");
    assert_eq!(reg_name(10), "a0");
    assert_eq!(reg_name(31), "t6");
}

#[test]
fn snapshot_reports_state() {
    let mut sim = Simulator::new();
    sim.load_image(ENTRY_POINT, &[0x13; 8]);
    sim.step();
    let state = sim.snapshot();
    assert_eq!(state.pc, ENTRY_POINT + 4);
    assert_eq!(state.cycle_count, 1);
    assert!(!state.halted);
    assert!(state.registers.contains_key("pc") || state.registers.contains_key("zero"));
}

#[test]
fn executes_jal_jumps_and_links() {
    let mut sim = Simulator::new();
    // Entry: JAL ra, +8  (skips the next instruction, links PC+4 into x1).
    //        (skipped: an addi that must not execute)
    // target: addi x5, x0, 42
    let prog = [
        jal_imm(8, 1),                // jal ra, +8 -> target
        i_type(99, 0, 0, 2, 0x13),    // addi x2, x0, 99  (skipped)
        i_type(42, 0, 0, 5, 0x13),    // addi x5, x0, 42  (target)
    ];
    let bytes: Vec<u8> = prog.iter().flat_map(|i| i.to_le_bytes()).collect();
    sim.load_image(ENTRY_POINT, &bytes);
    sim.run(10);
    // x1 = PC+4 of the JAL (return address), x5 = 42, x2 untouched.
    assert_eq!(sim.read_reg(1), ENTRY_POINT + 4, "ra links to the next instruction");
    assert_eq!(sim.read_reg(5), 42, "target executed");
    assert_eq!(sim.read_reg(2), 0, "skipped instruction did not execute");
}

#[test]
fn rl78_loads_and_executes() {
    use ns_sim::Arch;
    let mut sim = Simulator::for_arch(Arch::Rl78).unwrap();
    // A tiny RL78 program at 0x1000: MOV A, #0x2A (0x51 0x2A); ADD A, #0x01
    // (0x0C 0x01); then a second MOV to A. The core advances PC per byte.
    let prog = [0x51, 0x2A, 0x0C, 0x01];
    sim.load_image(0x1000, &prog);
    sim.run(4);
    // Register A (index 1) should be 0x2B.
    assert_eq!(sim.read_reg(1), 0x2B, "rl78 A register after add");
    // PC should have advanced past the 4 bytes.
    assert!(sim.read_reg(10) >= 0x1004, "rl78 pc advanced");
    // The architecture is recognized.
    assert!(matches!(sim.arch, Arch::Rl78), "rl78 arch set");
}

/// `step_out` runs until a return instruction is executed, landing on the
/// caller's next instruction. Uses a RISC-V call/ret pair.
#[test]
fn step_out_returns_to_caller() {
    let mut sim = Simulator::new();
    // ENTRY: jal ra, +16 -> sub (return address = ENTRY+4)
    // +4:    addi x2, x0, 99   (caller's next instruction — landing target)
    // +8:    addi x3, x0, 1    (unused padding)
    // +12:   addi x3, x0, 2
    // +16:   addi x3, x0, 7    (sub body)
    // +20:   ret (jalr x0, 0(x1))
    let ret = i_type(0, 1, 0, 0, 0x67);
    let prog = [
        jal_imm(16, 1),               // jal ra, +16 -> sub
        i_type(99, 0, 0, 2, 0x13),    // addi x2, x0, 99
        i_type(1, 0, 0, 3, 0x13),     // addi x3, x0, 1
        i_type(2, 0, 0, 3, 0x13),     // addi x3, x0, 2
        i_type(7, 0, 0, 3, 0x13),     // addi x3, x0, 7 (sub body)
        ret,                          // ret
    ];
    let bytes: Vec<u8> = prog.iter().flat_map(|i| i.to_le_bytes()).collect();
    sim.load_image(ENTRY_POINT, &bytes);
    // Execute the call: PC now points at the sub body, ra holds the return addr.
    assert_eq!(sim.step(), StepResult::Advanced);
    assert_eq!(sim.read_reg(1), ENTRY_POINT + 4, "ra = return address");
    assert_eq!(sim.pc, ENTRY_POINT + 16, "PC at sub body");
    // Step out: runs through the sub body + ret, landing on the caller's next
    // instruction without executing it.
    let result = sim.step_out(100);
    assert_eq!(result, StepResult::Advanced);
    assert_eq!(sim.pc, ENTRY_POINT + 4, "landed on caller's next instruction");
    assert_eq!(sim.read_reg(3), 7, "sub body executed");
    assert_eq!(sim.read_reg(2), 0, "caller's instruction not executed yet");
}
