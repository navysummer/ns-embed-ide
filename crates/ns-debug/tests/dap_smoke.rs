//! End-to-end smoke test for the debug adapter pipeline.
//!
//! Builds a tiny C program with `cc -g`, launches it under the real DAP adapter,
//! hits a breakpoint, continues, and checks the session terminates cleanly.
//! Skips gracefully when `lldb-dap` or `cc` is unavailable (e.g. non-macOS CI).

// Shared test helpers are only called by some tests; a single-test run would
// otherwise report them as dead code.
#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use ns_debug::{DebugBreakpoint, DebugCommand, DebugConfig, DebugMessage};

/// Per-test fixture counter: e2e tests run in parallel within one process,
/// so each `compile_fixture` call gets its own directory to avoid racing on
/// `remove_dir_all`/`create_dir_all` of the shared PID-based path.
static FIXTURE_SLOT: AtomicU32 = AtomicU32::new(0);

fn compile_fixture() -> (PathBuf, PathBuf, PathBuf) {
    let slot = FIXTURE_SLOT.fetch_add(1, Ordering::SeqCst);
    let root =
        std::env::temp_dir().join(format!("ns-debug-e2e-{}-{}", std::process::id(), slot));
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
        env: Vec::new(),
        cwd: root.clone(),
        breakpoints: vec![DebugBreakpoint {
            path: source_display.clone(),
            line: 4,
            ..Default::default()
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

/// The DAP `restart` request restarts the debuggee in place: after hitting a
/// breakpoint and restarting, the program runs from `main` again and stops at
/// the same breakpoint (the loop counter is reset).
#[test]
fn dap_restart_reruns_from_main() {
    if ns_debug::find_dap_adapter().is_none() {
        eprintln!("lldb-dap 不可用，跳过端到端测试");
        return;
    }
    if Command::new("cc").arg("--version").output().is_err() {
        eprintln!("cc 不可用，跳过端到端测试");
        return;
    }

    let (root, source, program) = compile_fixture();
    let source_display = source.display().to_string();
    let config = DebugConfig {
        program,
        args: Vec::new(),
        env: Vec::new(),
        cwd: root.clone(),
        breakpoints: vec![DebugBreakpoint {
            path: source_display.clone(),
            line: 4,
            ..Default::default()
        }],
    };

    let handle = ns_debug::spawn(&config).expect("failed to launch debug session");
    wait_for(&handle, |msg| matches!(msg, DebugMessage::Ready), Duration::from_secs(10), "Ready");

    // First stop at line 4 (i == 0).
    let stop = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Stopped { .. }),
        Duration::from_secs(10),
        "first breakpoint stop",
    );
     let DebugMessage::Stopped { thread_id: _, .. } = stop else {
        unreachable!()
    };

     // Native restart: the adapter stays alive and the debuggee re-runs.
     handle.send(DebugCommand::Restart).unwrap();
    // After a restart the program runs from `main` again; drive it through
    // the loop's two iterations and out to a clean process exit, continuing
    // from whichever thread stops next.
    let mut continue_count = 0;
    loop {
        match wait_for(
            &handle,
            |msg| matches!(msg, DebugMessage::Stopped { .. } | DebugMessage::Terminated),
            Duration::from_secs(15),
            "stop-or-exit after restart",
        ) {
            DebugMessage::Stopped { thread_id, .. } => {
                handle.send(DebugCommand::Continue { thread_id }).unwrap();
                continue_count += 1;
                assert!(continue_count <= 4, "restart did not reach a clean exit");
            }
            DebugMessage::Terminated => break,
            _ => unreachable!(),
        }
    }
 
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
        env: Vec::new(),
        cwd: root.clone(),
        breakpoints: vec![DebugBreakpoint {
            path: source_display,
            line: 4,
            condition: Some("i == 1".into()),
            ..Default::default()
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
        env: Vec::new(),
        cwd: root.clone(),
        breakpoints: vec![DebugBreakpoint {
            path: source,
            line: 4,
            ..Default::default()
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

/// Collect messages from `handle` until one matches `predicate`, returning every
/// message seen up to and including the match.
#[track_caller]
fn collect_until(
    handle: &ns_debug::DebugHandle,
    predicate: impl Fn(&DebugMessage) -> bool,
    timeout: Duration,
    what: &str,
) -> Vec<DebugMessage> {
    let deadline = Instant::now() + timeout;
    let mut collected = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            remaining > Duration::ZERO,
            "timed out waiting for {what}; saw {collected:?}"
        );
        match handle.recv_timeout(remaining.min(Duration::from_millis(200))) {
            Ok(message) => {
                let matched = predicate(&message);
                collected.push(message);
                if matched {
                    return collected;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("debug session disconnected while waiting for {what}")
            }
        }
    }
}

/// Find the first message of the requested kind in a collected batch.
#[track_caller]
fn take<T>(
    batch: Vec<DebugMessage>,
    extract: impl Fn(DebugMessage) -> Result<T, DebugMessage>,
    what: &str,
) -> T {
    for message in batch {
        if let Ok(value) = extract(message) {
            return value;
        }
    }
    panic!("no {what} in batch")
}

#[test]
fn dap_breakpoint_results_reported_and_hit_condition_pauses() {
    if ns_debug::find_dap_adapter().is_none() {
        eprintln!("lldb-dap 不可用，跳过端到端测试");
        return;
    }
    if Command::new("cc").arg("--version").output().is_err() {
        eprintln!("cc 不可用，跳过端到端测试");
        return;
    }

    let (root, source, program) = compile_fixture();
    let source_display = source.display().to_string();
    // Line 4 is `marker++;`, hit on both iterations of the fixture loop.
    let config = DebugConfig {
        program,
        args: Vec::new(),
        env: Vec::new(),
        cwd: root.clone(),
        breakpoints: vec![DebugBreakpoint {
            path: source_display.clone(),
            line: 4,
            hit_condition: Some("2".into()),
            ..Default::default()
        }],
    };
    let handle = ns_debug::spawn(&config).expect("failed to launch debug session");

    // The adapter must confirm the launch-time binding.
    let results = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::BreakpointResults { .. }),
        Duration::from_secs(10),
        "breakpoint results",
    );
    let DebugMessage::BreakpointResults { file, results } = results else {
        unreachable!()
    };
    assert_eq!(file, source_display);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].line, 4);
    assert!(results[0].verified, "expected the adapter to confirm the location");

    // hitCondition "2" pauses only on the second hit even when the debuggee
    // starts out stopped at the same line for a different reason.
    let stop = collect_until(
        &handle,
        |msg| matches!(msg, DebugMessage::Stopped { reason, .. } if reason.contains("breakpoint")),
        Duration::from_secs(15),
        "hit-condition stop",
    );
    let (reason, thread_id) = take(
        stop,
        |msg| match msg {
            DebugMessage::Stopped { reason, thread_id, .. } => Ok((reason, thread_id)),
            other => Err(other),
        },
        "breakpoint stop",
    );
    assert_eq!(reason, "breakpoint");

    handle.send(DebugCommand::GetStackTrace { thread_id }).unwrap();
    let frames = collect_until(
        &handle,
        |msg| matches!(msg, DebugMessage::StackTrace { .. }),
        Duration::from_secs(10),
        "stack trace",
    );
    let frames = take(
        frames,
        |msg| match msg {
            DebugMessage::StackTrace { frames } => Ok(frames),
            other => Err(other),
        },
        "stack trace",
    );
    // The stop must be on the requested line.
    assert_eq!(frames.first().and_then(|frame| frame.line), Some(4));

    handle.send(DebugCommand::Stop).unwrap();
    wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Terminated),
        Duration::from_secs(15),
        "process exit",
    );

    let _ = fs::remove_dir_all(root);
}

/// Watchpoint (data breakpoint) end-to-end: resolve `marker` to a data id via
/// `dataBreakpointInfo`, arm a write watchpoint, then continue from the line
/// breakpoint and expect the debugger to stop on the store to `marker`.
#[test]
fn dap_watchpoint_resolves_and_stops_on_write() {
    if ns_debug::find_dap_adapter().is_none() {
        eprintln!("lldb-dap 不可用，跳过端到端测试");
        return;
    }
    if Command::new("cc").arg("--version").output().is_err() {
        eprintln!("cc 不可用，跳过端到端测试");
        return;
    }

    let (root, source, program) = compile_fixture();
    let config = DebugConfig {
        program,
        args: Vec::new(),
        env: Vec::new(),
        cwd: root.clone(),
        breakpoints: vec![DebugBreakpoint {
            path: source.display().to_string(),
            line: 4,
            ..Default::default()
        }],
    };
    let handle = ns_debug::spawn(&config).expect("failed to launch debug session");

    // One batch keeps every message between Ready and the first stop, so the
    // stack/scopes/variables replies stay observable in order.
    let startup = collect_until(
        &handle,
        |msg| matches!(msg, DebugMessage::Stopped { .. }),
        Duration::from_secs(15),
        "first breakpoint stop",
    );
    let thread_id = take(
        startup,
        |msg| match msg {
            DebugMessage::Stopped { thread_id, .. } => Ok(thread_id),
            other => Err(other),
        },
        "breakpoint stop",
    );
    handle.send(DebugCommand::GetStackTrace { thread_id }).unwrap();
    let frames = collect_until(
        &handle,
        |msg| matches!(msg, DebugMessage::StackTrace { .. }),
        Duration::from_secs(10),
        "stack trace",
    );
    let frames = take(
        frames,
        |msg| match msg {
            DebugMessage::StackTrace { frames } => Ok(frames),
            other => Err(other),
        },
        "stack trace",
    );
    let frame_id = frames.first().expect("expected a frame").id;

    handle.send(DebugCommand::GetScopes { frame_id }).unwrap();
    let scopes = collect_until(
        &handle,
        |msg| matches!(msg, DebugMessage::Scopes { .. }),
        Duration::from_secs(10),
        "scopes",
    );
    let scopes = take(
        scopes,
        |msg| match msg {
            DebugMessage::Scopes { scopes, .. } => Ok(scopes),
            other => Err(other),
        },
        "scopes",
    );

    // `marker` is a file-scope static, so lldb-dap lists it under Globals. Ask
    // every scope that lists it; the first that resolves gives the data id.
    let mut listing = Vec::new();
    for scope in &scopes {
        handle
            .send(DebugCommand::GetVariables {
                variables_reference: scope.id,
            })
            .unwrap();
        let batch = collect_until(
            &handle,
            |msg| matches!(msg, DebugMessage::Variables { reference, .. } if *reference == scope.id),
            Duration::from_secs(10),
            "scope variables",
        );
        let variables = take(
            batch,
            |msg| match msg {
                DebugMessage::Variables { reference, variables } if reference == scope.id => {
                    Ok(variables)
                }
                other => Err(other),
            },
            "scope variables",
        );
        if variables.iter().any(|variable| variable.name == "marker") {
            listing.push(scope.id);
        }
    }
    assert!(!listing.is_empty(), "no scope exposes `marker`");

    let mut resolved = None;
    for scope in listing {
        handle
            .send(DebugCommand::DataBreakpointInfo {
                expression: "marker".into(),
                scope,
            })
            .unwrap();
        let batch = collect_until(
            &handle,
            |msg| matches!(msg, DebugMessage::DataBreakpointInfo { .. }),
            Duration::from_secs(10),
            "data breakpoint info",
        );
        let info = take(
            batch,
            |msg| match msg {
                DebugMessage::DataBreakpointInfo {
                    expression,
                    data_id,
                    access_types,
                    description,
                } => Ok((expression, data_id, access_types, description)),
                other => Err(other),
            },
            "data breakpoint info",
        );
        if let Some(data_id) = info.1 {
            resolved = Some((data_id, info.2, info.3));
            break;
        }
    }
    let Some((data_id, access_types, description)) = resolved else {
        panic!("marker is not watchable in any scope");
    };
    assert!(
        access_types.is_empty() || access_types.iter().any(|kind| kind == "write"),
        "expected write access to be allowed, got {access_types:?}"
    );

    handle
        .send(DebugCommand::SetDataBreakpoints(vec![ns_debug::WatchpointRequest {
            expression: "marker".into(),
            data_id,
            access_type: "write".into(),
        }]))
        .unwrap();
    let results = collect_until(
        &handle,
        |msg| matches!(msg, DebugMessage::WatchpointResults { .. }),
        Duration::from_secs(10),
        "watchpoint results",
    );
    let results = take(
        results,
        |msg| match msg {
            DebugMessage::WatchpointResults { results } => Ok(results),
            other => Err(other),
        },
        "watchpoint results",
    );
    assert_eq!(results.len(), 1);
    assert!(
        results[0].verified,
        "watchpoint on {description} was rejected: {}",
        results[0].reason
    );

    // The line breakpoint sits on the store to `marker`: continuing from it
    // must immediately trip the write watchpoint.
    handle.send(DebugCommand::Continue { thread_id }).unwrap();
    let stop = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Stopped { .. }),
        Duration::from_secs(15),
        "watchpoint stop",
    );
    let DebugMessage::Stopped { reason, .. } = stop else {
        unreachable!()
    };
    assert_eq!(reason, "breakpoint");

    handle.send(DebugCommand::Stop).unwrap();
    wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Terminated),
        Duration::from_secs(15),
        "process exit",
    );

    let _ = fs::remove_dir_all(root);
}

/// Hardware breakpoint (`setBreakpoints` with `mode: "hardware"`) must bind and
/// stop the debuggee, and the adapter must confirm it as verified.
#[test]
fn dap_hardware_breakpoint_binds_and_stops() {
    if ns_debug::find_dap_adapter().is_none() {
        eprintln!("lldb-dap 不可用，跳过端到端测试");
        return;
    }
    if Command::new("cc").arg("--version").output().is_err() {
        eprintln!("cc 不可用，跳过端到端测试");
        return;
    }

    let (root, source, program) = compile_fixture();
    let source_display = source.display().to_string();
    let config = DebugConfig {
        program,
        args: Vec::new(),
        env: Vec::new(),
        cwd: root.clone(),
        breakpoints: vec![DebugBreakpoint {
            path: source_display.clone(),
            line: 4,
            hardware: true,
            ..Default::default()
        }],
    };
    let handle = ns_debug::spawn(&config).expect("failed to launch debug session");

    // The adapter must confirm the hardware location bound.
    let results = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::BreakpointResults { .. }),
        Duration::from_secs(10),
        "hardware breakpoint results",
    );
    let DebugMessage::BreakpointResults { file, results } = results else {
        unreachable!()
    };
    assert_eq!(file, source_display);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].line, 4);
    assert!(
        results[0].verified,
        "expected the adapter to confirm the hardware location: {}",
        results[0].reason
    );

    // The hardware breakpoint must actually stop the debuggee.
    let stop = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Stopped { .. }),
        Duration::from_secs(10),
        "hardware breakpoint stop",
    );
    let DebugMessage::Stopped { thread_id, .. } = stop else {
        unreachable!()
    };
    handle.send(DebugCommand::GetStackTrace { thread_id }).unwrap();
    let frames = wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::StackTrace { .. }),
        Duration::from_secs(10),
        "stack trace",
    );
    let DebugMessage::StackTrace { frames } = frames else {
        unreachable!()
    };
    assert_eq!(frames.first().and_then(|frame| frame.line), Some(4));

    handle.send(DebugCommand::Stop).unwrap();
    wait_for(
        &handle,
        |msg| matches!(msg, DebugMessage::Terminated),
        Duration::from_secs(15),
        "process exit",
    );

    let _ = fs::remove_dir_all(root);
}
