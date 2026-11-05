//! End-to-end smoke test for the debug adapter pipeline.
//!
//! Builds a tiny C program with `cc -g`, launches it under the real DAP adapter,
//! hits a breakpoint, continues, and checks the session terminates cleanly.
//! Skips gracefully when `lldb-dap` or `cc` is unavailable (e.g. non-macOS CI).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use ns_debug::{DebugBreakpoint, DebugCommand, DebugConfig, DebugMessage};

fn compile_fixture() -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("ns-debug-e2e-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let source = root.join("e2e.c");
    let program = root.join("e2e");
    fs::write(
        &source,
        "static volatile int marker = 0;\n\
         int main(void) {\n\
             for (int i = 0; i < 2; i++) {\n\
                 marker++;\n\
             }\n\
             return marker;\n\
         }\n",
    )
    .unwrap();
    let status = Command::new("cc")
        .arg("-g")
        .arg("-O0")
        .arg(&source)
        .arg("-o")
        .arg(&program)
        .status()
        .unwrap();
    assert!(status.success(), "failed to compile fixture");
    (root, source, program)
}

fn wait_for(
    handle: &ns_debug::DebugHandle,
    predicate: impl Fn(&DebugMessage) -> bool,
    timeout: Duration,
    what: &str,
) -> DebugMessage {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(remaining > Duration::ZERO, "timed out waiting for {what}");
        match handle.recv_timeout(remaining.min(Duration::from_millis(500))) {
            Ok(message) => {
                if predicate(&message) {
                    return message;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("debug session disconnected while waiting for {what}")
            }
        }
    }
}

#[test]
fn dap_launch_hit_continue_terminate() {
    if ns_debug::find_dap_adapter().is_none() {
        eprintln!("lldb-dap 不可用，跳过端到端测试");
        return;
    }
    if Command::new("cc").arg("--version").output().is_err() {
        eprintln!("cc 不可用，跳过端到端测试");
        return;
    }

    let (root, source, program) = compile_fixture();
    // NOTE: the debugger matches breakpoints by the path recorded in DWARF, which is the
    // path passed to the compiler. Do NOT canonicalize (that would resolve /tmp -> /private/tmp).
    let source_display = source.display().to_string();
    // Line 4 is `marker++;`
    let config = DebugConfig {
        program,
        args: Vec::new(),
        cwd: root.clone(),
        breakpoints: vec![DebugBreakpoint {
            path: source_display.clone(),
            line: 4,
            condition: None,
        }],
    };

    let handle = ns_debug::spawn(&config).expect("failed to launch debug session");

    wait_for(&handle, |msg| matches!(msg, DebugMessage::Ready), Duration::from_secs(10), "Ready");

    // First hit: i == 0
    let stop = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Stopped { .. }),
        Duration::from_secs(10),
        "first breakpoint stop",
    );
    let DebugMessage::Stopped { thread_id, .. } = stop else {
        unreachable!()
    };

    // Stack trace must contain our fixture file and line.
    handle.send(DebugCommand::GetStackTrace { thread_id }).unwrap();
    let trace = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::StackTrace { .. }),
        Duration::from_secs(10),
        "stack trace",
    );
    let DebugMessage::StackTrace { frames } = trace else {
        unreachable!()
    };
    let frame = frames
        .iter()
        .find(|frame| frame.file.as_deref() == Some(source_display.as_str()))
        .expect("no frame in the fixture file");
    assert_eq!(frame.line, Some(4));

    // Continue -> stops again at i == 1 (line 4), then continue -> process exits.
    handle.send(DebugCommand::Continue { thread_id }).unwrap();
    wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Stopped { .. }),
        Duration::from_secs(10),
        "second breakpoint stop",
    );

    handle.send(DebugCommand::Continue { thread_id }).unwrap();
    wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Terminated),
        Duration::from_secs(15),
        "process exit",
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn dap_conditional_breakpoint_fires_only_when_true() {
    if ns_debug::find_dap_adapter().is_none() {
        eprintln!("lldb-dap 不可用，跳过端到端测试");
        return;
    }
    if Command::new("cc").arg("--version").output().is_err() {
        eprintln!("cc 不可用，跳过端到端测试");
        return;
    }

    let (root, source, program) = compile_fixture();
    // Same path-form consideration as the other fixture: use the compiler-side path for breakpoints.
    let source_display = source.display().to_string();
    let config = DebugConfig {
        program,
        args: Vec::new(),
        cwd: root.clone(),
        breakpoints: vec![DebugBreakpoint {
            path: source_display,
            line: 4,
            condition: Some("i == 1".into()),
        }],
    };

    let handle = ns_debug::spawn(&config).expect("failed to launch debug session");
    wait_for(&handle, |msg| matches!(msg, DebugMessage::Ready), Duration::from_secs(10), "Ready");

    // With `i == 1`, the first stop happens on the second iteration, not the first.
    let stop = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Stopped { .. }),
        Duration::from_secs(10),
        "conditional breakpoint stop",
    );
    let DebugMessage::Stopped { thread_id, .. } = stop else {
        unreachable!()
    };

    // Ask for the stack to confirm the frame; then disconnect.
    handle.send(DebugCommand::GetStackTrace { thread_id }).unwrap();
    let _ = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::StackTrace { .. }),
        Duration::from_secs(10),
        "stack trace",
    );
    handle.send(DebugCommand::Stop).unwrap();
    wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Terminated),
        Duration::from_secs(10),
        "disconnect termination",
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn dap_registers_memory_disassembly() {
    if ns_debug::find_dap_adapter().is_none() {
        eprintln!("lldb-dap 不可用，跳过端到端测试");
        return;
    }
    if Command::new("cc").arg("--version").output().is_err() {
        eprintln!("cc 不可用，跳过端到端测试");
        return;
    }

    let (root, source, program) = compile_fixture();
    let source = source.display().to_string();
    let config = DebugConfig {
        program,
        args: Vec::new(),
        cwd: root.clone(),
        breakpoints: vec![DebugBreakpoint {
            path: source,
            line: 4,
            condition: None,
        }],
    };
    let handle = ns_debug::spawn(&config).expect("failed to launch debug session");
    wait_for(&handle, |msg| matches!(msg, DebugMessage::Ready), Duration::from_secs(10), "Ready");
    let stop = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Stopped { .. }),
        Duration::from_secs(10),
        "breakpoint stop",
    );
    let DebugMessage::Stopped { thread_id, .. } = stop else {
        unreachable!()
    };

    // Stack trace -> pick frame 0 -> pc.
    handle.send(DebugCommand::GetStackTrace { thread_id }).unwrap();
    let trace = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::StackTrace { .. }),
        Duration::from_secs(10),
        "stack trace",
    );
    let DebugMessage::StackTrace { frames } = trace else {
        unreachable!()
    };
    let pc = frames[0].pc.expect("lldb-dap should report the instruction pointer");

    // Scopes -> find the Registers scope.
    handle.send(DebugCommand::GetScopes { frame_id: frames[0].id }).unwrap();
    let scopes_message = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Scopes { .. }),
        Duration::from_secs(10),
        "scopes",
    );
    let DebugMessage::Scopes { scopes, .. } = scopes_message else {
        unreachable!()
    };
    let registers_scope = scopes
        .iter()
        .find(|scope| scope.name.eq_ignore_ascii_case("Registers"))
        .expect("lldb-dap should expose a Registers scope");

    // Expand Registers -> groups (General Purpose / Floating Point / Exception State).
    handle.send(DebugCommand::GetVariables { variables_reference: registers_scope.id }).unwrap();
    let groups = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Variables { reference, .. } if reference == &registers_scope.id),
        Duration::from_secs(10),
        "register groups",
    );
    let DebugMessage::Variables { variables: groups, .. } = groups else {
        unreachable!()
    };
    let general = groups
        .iter()
        .find(|group| group.name.contains("General Purpose"))
        .expect("Registers scope should include General Purpose Registers");

    // Expand General Purpose -> actual registers, non-empty.
    handle.send(DebugCommand::GetVariables { variables_reference: general.variables_reference }).unwrap();
    let registers = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Variables { reference, .. } if reference == &general.variables_reference),
        Duration::from_secs(10),
        "general purpose registers",
    );
    let DebugMessage::Variables { variables: registers, .. } = registers else {
        unreachable!()
    };
    assert!(!registers.is_empty(), "expected at least one register");
    assert!(registers.iter().any(|register| !register.value.is_empty()));

    // Read 32 bytes at the pc.
    handle.send(DebugCommand::ReadMemory { reference: format!("0x{pc:x}"), count: 32 }).unwrap();
    let memory = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Memory { .. }),
        Duration::from_secs(10),
        "read memory",
    );
    let DebugMessage::Memory { address, data } = memory else {
        unreachable!()
    };
    assert_eq!(address, pc);
    assert_eq!(data.len(), 32);

    // Disassemble at the pc.
    handle.send(DebugCommand::Disassemble { reference: format!("0x{pc:x}"), instruction_count: 4 }).unwrap();
    let disassembly = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Disassembly { .. }),
        Duration::from_secs(10),
        "disassemble",
    );
    let DebugMessage::Disassembly { instructions } = disassembly else {
        unreachable!()
    };
    assert!(instructions.len() >= 3, "expected at least 3 instructions");
    assert_eq!(instructions[0].address, pc);
    assert!(instructions[0].instruction.len() > 0);

    handle.send(DebugCommand::Stop).unwrap();
    wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Terminated),
        Duration::from_secs(10),
        "disconnect termination",
    );

    let _ = fs::remove_dir_all(root);
}
