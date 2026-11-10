//! Debug session abstraction for ns-embed-ide.
//!
//! Every debug session is driven through the Debug Adapter Protocol (DAP).
//! The default adapter is `lldb-dap` (part of Xcode / Command Line Tools on macOS);
//! a different adapter can be selected with the `NS_DEBUG_ADAPTER` environment variable.
//!
//! The crate spawns the adapter in a child process and exposes two channels:
//! one for UI-bound messages ([`DebugMessage`]) and one for UI-issued commands
//! ([`DebugCommand`]). All request/response plumbing is handled internally.

pub mod dap;

use std::io::BufReader;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

pub mod swvd;
pub mod embedded;

pub use dap::{read_message, wait_for_response, write_message};
pub use embedded::{
    EmbeddedConfig, EmbeddedDebugHandle, ProbeKind, find_gdb, parse_elf_symbols,
    server_command, spawn_embedded_debug, start_server,
};
pub use swvd::{Probe, TraceConfig, TraceMode, find_probe, package_support};

/// One breakpoint to be applied in the debuggee.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DebugBreakpoint {
    /// Absolute path to the source file.
    pub path: String,
    /// 1-based line.
    pub line: usize,
    /// Optional condition expression, for example `count == 10`.
    pub condition: Option<String>,
    /// Optional hit-count limit: pause on the N-th hit (`N` or `N ==`);
    /// mapped to the DAP `hitCondition` field.
    pub hit_condition: Option<String>,
    /// Optional log message printed instead of pausing (`{expr}` placeholders
    /// allowed); mapped to the DAP `logMessage` field.
    pub log_message: Option<String>,
    /// Use a hardware breakpoint (DAP `mode: "hardware"`) instead of the
    /// default software breakpoint. Hardware breakpoints consume a limited
    /// on-target resource; the adapter reports a pending breakpoint when the
    /// target cannot bind one.
    pub hardware: bool,
}
/// One breakpoint location inside a source file, with its DAP properties.
/// Used as the per-line payload in [`group_breakpoints`] and
/// [`DebugCommand::ResyncBreakpoints`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DebugBreakpointLocation {
    /// 1-based line.
    pub line: usize,
    /// Optional condition expression.
    pub condition: Option<String>,
    /// Optional DAP `hitCondition` value (pause on the N-th hit).
    pub hit_condition: Option<String>,
    /// Optional DAP `logMessage` value (log instead of pause).
    pub log_message: Option<String>,
    /// Use a hardware breakpoint (DAP `mode: "hardware"`).
    pub hardware: bool,
}

/// Outcome of applying one breakpoint, as confirmed by the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugBreakpointResult {
    /// 1-based line of the requested location.
    pub line: usize,
    /// Whether the adapter confirmed the location is bound in the debuggee.
    pub verified: bool,
    /// Why the location is pending, when unverified.
    pub reason: String,
}

impl DebugBreakpoint {
    /// DAP location for this breakpoint: line plus its optional properties.
    pub fn location(&self) -> DebugBreakpointLocation {
        DebugBreakpointLocation {
            line: self.line,
            condition: self.condition.clone(),
            hit_condition: self.hit_condition.clone(),
            log_message: self.log_message.clone(),
            hardware: self.hardware,
        }
    }
}

/// One resolved data breakpoint (watchpoint) to send to the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchpointRequest {
    /// User-facing expression this entry came from, used to map the response.
    pub expression: String,
    /// Adapter-provided `dataId` (address + size), from `dataBreakpointInfo`.
    pub data_id: String,
    /// DAP `accessType`: `read`, `write`, or `readWrite`.
    pub access_type: String,
}

/// Outcome of applying one watchpoint, as confirmed by `setDataBreakpoints`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchpointResult {
    pub expression: String,
    pub verified: bool,
    pub reason: String,
    /// Adapter breakpoint id, when the adapter reports one.
    pub adapter_id: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct DebugConfig {
    /// The executable to debug.
    pub program: PathBuf,
    /// Arguments passed to the debuggee.
    pub args: Vec<String>,
    /// Environment variables as `KEY=value` applied on top of the inherited
    /// environment when the debuggee starts.
    pub env: Vec<String>,
    /// Working directory for the debuggee.
    pub cwd: PathBuf,
    /// Breakpoints to apply before the session starts.
    pub breakpoints: Vec<DebugBreakpoint>,
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub id: u64,
    pub name: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    /// Instruction pointer, when the adapter reports it.
    pub pc: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Scope {
    pub id: u64,
    pub name: String,
    pub expensive: bool,
}

#[derive(Debug, Clone)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub variables_reference: u64,
}

/// One thread reported by the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadInfo {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Instruction {
    /// Absolute address of the instruction.
    pub address: u64,
    /// Assembly text, e.g. `movl 0xc12(%rip), %eax ; marker`.
    pub instruction: String,
    /// Raw bytes, e.g. `8b 05 12 0c 00 00`.
    pub bytes: String,
    pub line: Option<usize>,
    /// Path of the source file, when available.
    pub file: Option<String>,
}

/// UI-bound messages produced by the debug session.
#[derive(Debug, Clone)]
pub enum DebugMessage {
    /// Adapter finished initialization and the program is ready to run.
    Ready,
    /// The debuggee paused (breakpoint hit, step completed, pause requested, …).
    Stopped { reason: String, thread_id: u64, description: Option<String> },
    /// The debuggee resumed.
    Continued,
    /// Full call stack for the most recent stop.
    StackTrace { frames: Vec<Frame> },
    /// Scopes for the selected frame of the most recent stop.
    Scopes { frame_id: u64, scopes: Vec<Scope> },
    /// Variables for the most recently requested scope or expandable variable.
    Variables { reference: u64, variables: Vec<Variable> },
    /// Memory bytes starting at `address` (result of `readMemory`).
    Memory { address: u64, data: Vec<u8> },
    /// A `writeMemory` completed successfully at `address`; the caller should
    /// re-read the region to refresh its view.
    MemoryWritten { address: u64, bytes: usize },
    /// Disassembly instructions starting at the given address.
    Disassembly { instructions: Vec<Instruction> },
    /// Output from the debuggee or the adapter.
    Output { category: String, text: String },
    /// Whether each requested breakpoint bound in the debuggee, as confirmed
    /// by a `setBreakpoints` response. Delivered for the initial set on
    /// launch and for every [`DebugCommand::ResyncBreakpoints`].
    BreakpointResults {
        /// Absolute source file the set belongs to.
        file: String,
        /// Per-line outcomes, in the adapter's order.
        results: Vec<DebugBreakpointResult>,
    },
    /// Outcome of evaluating one watchpoint expression via `dataBreakpointInfo`.
    /// `data_id` is `None` when the expression can't be watched at this stop.
    DataBreakpointInfo {
        expression: String,
        /// Adapter `dataId` to use with [`DebugCommand::SetDataBreakpoints`].
        data_id: Option<String>,
        /// `accessTypes` the adapter allows (empty = default to write).
        access_types: Vec<String>,
        description: String,
    },
    /// Outcome of `setDataBreakpoints`, in the requested order.
    WatchpointResults { results: Vec<WatchpointResult> },
    /// The most recent stop is a data breakpoint; `description` is the
    /// adapter's text (lldb: `watchpoint 1`) used to identify which watchpoint.
    WatchpointHit { description: String },
    /// Threads the adapter knows about (result of [`DebugCommand::Threads`]).
    Threads { threads: Vec<ThreadInfo> },
    /// Variables of a scope whose `variablesReference` can be reused to
    /// evaluate expressions in [`DebugCommand::Evaluate`].
    ScopeVariables {
        scope: String,
        reference: u64,
        variables: Vec<Variable>,
    },
    /// The debuggee's current source location changed without a full stop
    /// (lldb-dap `caret` / `frameStoppedLocation`); update the highlight only.
    Caret {
        path: Option<String>,
        line: usize,
    },
    /// Every trace register write succeeded; the window is now armed.
    TraceConfigured { config: TraceConfig },
    /// Decoded SWO trace packets streamed from the probe (SWO/SWV window).
    /// Delivered as the reader thread decodes them.
    TracePackets(Vec<swvd::Packet>),
    /// Result of [`DebugCommand::Evaluate`].
    Evaluated {
        expression: String,
        result: String,
        type_name: Option<String>,
    },
    /// Result of [`DebugCommand::SetVariable`]: the new value and the
    /// (possibly refreshed) variable the adapter reports.
    VariableSet {
        variables_reference: u64,
        name: String,
        value: String,
        type_name: Option<String>,
    },
    Terminated,
    /// Anything that went wrong.
    Error(String),
}

/// Commands the UI can send to a running debug session.
#[derive(Debug, Clone)]
pub enum DebugCommand {
    Continue { thread_id: u64 },
    /// Run until the given source location (DAP `continueTo`).
    ContinueTo { thread_id: u64, path: String, line: usize },
    StepOver { thread_id: u64 },
    StepIn { thread_id: u64 },
    StepOut { thread_id: u64 },
    Pause { thread_id: u64 },
    /// Set the value of a variable (DAP `setVariable`). The answer arrives as
    /// [`DebugMessage::VariableSet`] so the caller can refresh the panel.
    SetVariable { variables_reference: u64, name: String, value: String },
    /// Detach and terminate the debuggee.
    Stop,
    GetStackTrace { thread_id: u64 },
    /// Fetch the scopes for a frame; the answer arrives as [`DebugMessage::Scopes`].
    GetScopes { frame_id: u64 },
    /// Fetch memory bytes starting at `reference` (a hex address like `0x1000003E8`).
    /// Fetch memory bytes starting at `reference` (a hex address like `0x1000003E8`).
    ReadMemory { reference: String, count: u64 },
    /// Write `data` bytes at `reference` (a hex address). The answer arrives as
    /// [`DebugMessage::MemoryWritten`] so the caller can re-read the region.
    WriteMemory { reference: String, data: Vec<u8> },
    /// Fetch disassembly instructions starting at `reference` (a hex address).
    Disassemble { reference: String, instruction_count: u64 },
    /// Fetch the variables for a scope or expandable variable; the answer arrives
    /// as [`DebugMessage::Variables`].
    GetVariables { variables_reference: u64 },
    /// Replace all breakpoints of the listed files. Files whose list is empty have
    /// their breakpoints cleared. Files missing from the vector are left untouched.
    ResyncBreakpoints(Vec<(String, Vec<DebugBreakpointLocation>)>),
    /// Resolve a watchpoint expression to a data id. `scope` is the
    /// `variablesReference` of the scope the expression was found in (`0` when
    /// unknown). The answer arrives as [`DebugMessage::DataBreakpointInfo`].
    DataBreakpointInfo { expression: String, scope: u64 },
    /// Replace the full data-breakpoint set; the answer arrives as
    /// [`DebugMessage::WatchpointResults`]. An empty vector clears all.
    SetDataBreakpoints(Vec<WatchpointRequest>),
    /// List threads; the answer arrives as [`DebugMessage::Threads`].
    Threads,
    /// Configure and start the trace window (AWT/DWT) on cores that support it.
    ConfigureTrace(swvd::TraceConfig),
    /// Stop tracing and drop the accumulated trace data.
    StopTrace,
    /// Start a background SWO stream reader on the probe's data port. The
    /// decoded packets arrive as [`DebugMessage::TracePackets`]. `port` is the
    /// probe's SWO TCP port; `itm_port` is the armed stimulus port.
    StartTraceStream { port: u16, itm_port: u8 },
    /// Stop the background SWO stream reader started by
    /// [`DebugCommand::StartTraceStream`].
    StopTraceStream,
    /// Fetch the variables of a scope by its name (`LocalScope`/`GlobalScope`).
    /// The answer arrives as [`DebugMessage::ScopeVariables`].
    ScopeVariables { scope: String },
    /// Evaluate an expression in the context of a scope.
    Evaluate {
        expression: String,
        scope_name: String,
    },
    // Internal: used to shut the command thread down when the adapter exits.
    #[doc(hidden)]
    InternalExit,
}

/// Requests whose responses need to be attributed back to their call site.
#[derive(Debug, Clone)]
enum PendingRequest {
    Scopes(u64),
    Variables(u64),
    /// Path of the source file a `setBreakpoints` response belongs to.
    /// Per-line outcomes are read from the response body itself.
    Breakpoints(String),
    /// Watchpoint expression a `dataBreakpointInfo` response belongs to.
    DataBreakpointInfo(String),
    /// Expressions of a `setDataBreakpoints` request, in request order.
    DataBreakpoints(Vec<String>),
    /// Scope name of a `variables` request issued for evaluation reuse.
    ScopeVariables { scope: String },
    /// Expression and scope of an `evaluate` request.
    Evaluate { expression: String, scope: String },
    /// A `setVariable` request: the (reference, name) so the response can be
    /// attributed back to the caller.
    VariableSet { variables_reference: u64, name: String },

    /// A `writeMemory` request: the target address so the response can be
    /// attributed back to the caller (to re-read the region).
    MemoryWrite { address: u64 },
    /// Configuration whose setup was completed by a `writeMemory` response.
    TraceConfigured(TraceConfig),
}

/// Handle to a running debug session.
#[derive(Debug)]
pub struct DebugHandle {
    messages: Receiver<DebugMessage>,
    commands: Sender<DebugCommand>,
}

impl DebugHandle {
    /// Receive the next message, blocking up to `timeout`.
    pub fn recv_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<DebugMessage, mpsc::RecvTimeoutError> {
        self.messages.recv_timeout(timeout)
    }

    /// Try to receive the next pending message without blocking.
    pub fn try_recv(&self) -> Result<DebugMessage, mpsc::TryRecvError> {
        self.messages.try_recv()
    }

    /// Send a command to the debug session.
    pub fn send(&self, command: DebugCommand) -> Result<(), mpsc::SendError<DebugCommand>> {
        self.commands.send(command)
    }

    /// Split the handle into its two channels.
    pub fn into_parts(self) -> (Receiver<DebugMessage>, Sender<DebugCommand>) {
        (self.messages, self.commands)
    }

}

/// Locate the debug adapter executable.
pub fn find_dap_adapter() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("NS_DEBUG_ADAPTER") {
        let candidate = PathBuf::from(path);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for directory in path_var.split(':') {
            let candidate = Path::new(directory).join("lldb-dap");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = Command::new("xcrun").args(["--find", "lldb-dap"]).output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let candidate = PathBuf::from(path);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        for candidate in [
            "/usr/bin/lldb-dap",
            "/Library/Developer/CommandLineTools/usr/bin/lldb-dap",
            "/Applications/Xcode.app/Contents/Developer/usr/bin/lldb-dap",
        ] {
            let path = PathBuf::from(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// Group breakpoints by (absolute) source file. Files with no enabled
/// breakpoints are represented by an empty location list so callers can
/// clear previously synced breakpoints explicitly.
pub fn group_breakpoints(
    breakpoints: &[DebugBreakpoint],
) -> Vec<(String, Vec<DebugBreakpointLocation>)> {
    let mut files: Vec<(String, Vec<DebugBreakpointLocation>)> = Vec::new();
    for breakpoint in breakpoints {
        let location = breakpoint.location();
        match files.iter_mut().find(|(file, _)| file == &breakpoint.path) {
            Some((_, locations)) => locations.push(location),
            None => files.push((breakpoint.path.clone(), vec![location])),
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    for (_, locations) in &mut files {
        locations.sort_by_key(|location| location.line);
        locations.dedup();
    }
    files
}

pub fn spawn(config: &DebugConfig) -> Result<DebugHandle> {
    let adapter = find_dap_adapter().ok_or_else(|| {
        anyhow!("未找到 lldb-dap（LLDB 调试适配器）。请安装 Xcode Command Line Tools，或通过 NS_DEBUG_ADAPTER 环境变量指定适配器路径。")
    })?;

    // lldb-dap can fail to attach to the debuggee under system load (for
    // example when several sessions launch at once, as the e2e tests do in
    // parallel). Retry the whole handshake a bounded number of times so a
    // transient attach failure does not abort the session.
    const MAX_ATTEMPTS: u32 = 3;
    let mut attempt = 0;
    let (mut child, mut stdin, mut stdout, launch_breakpoint_results, configuration_seq) = loop {
        attempt += 1;
        match perform_handshake(config, &adapter) {
            Ok(handshake) => break handshake,
            Err(_error) if attempt < MAX_ATTEMPTS => {
                std::thread::sleep(std::time::Duration::from_millis(150 * attempt as u64));
            }
            Err(error) => return Err(error),
        }
    };

    // ---- Handshake complete; create the channels and start the pump threads ----
    let (messages, message_rx) = mpsc::channel::<DebugMessage>();
    let (commands, command_rx) = mpsc::channel::<DebugCommand>();
    let _ = messages.send(DebugMessage::Ready);
    for (file, results) in launch_breakpoint_results {
        let _ = messages.send(DebugMessage::BreakpointResults { file, results });
    }

    // Tracks the few commands whose responses must carry context from the request.
    let pending: Arc<Mutex<HashMap<u64, PendingRequest>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // ---- Command thread: UI commands -> adapter requests ----
    let command_messages = messages.clone();
    let command_pending = Arc::clone(&pending);
    thread::Builder::new()
        .name("ns-debug-commands".into())
        .spawn(move || {
            let mut sequence = configuration_seq + 1;
            // Background SWO stream reader, started/stopped by the trace window.
            let mut trace_stream: Option<swvd::TraceStream> = None;
            while let Ok(command) = command_rx.recv() {
                match command {
                    DebugCommand::Continue { thread_id } => {
                        request(&mut stdin, sequence, "continue", json!({ "threadId": thread_id }));
                        sequence += 1;
                    }
                    DebugCommand::ContinueTo { thread_id, path, line } => {
                        request(
                            &mut stdin,
                            sequence,
                            "continueTo",
                            json!({
                                "threadId": thread_id,
                                "location": { "path": path, "line": line },
                            }),
                        );
                        sequence += 1;
                    }
                    DebugCommand::SetVariable {
                        variables_reference,
                        name,
                        value,
                    } => {
                        let seq = sequence;
                        sequence += 1;
                        command_pending
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .insert(
                                seq,
                                PendingRequest::VariableSet {
                                    variables_reference,
                                    name: name.clone(),
                                },
                            );
                        request(
                            &mut stdin,
                            seq,
                            "setVariable",
                            json!({
                                "variablesReference": variables_reference,
                                "name": name,
                                "value": value,
                            }),
                        );
                    }
                    DebugCommand::StepOver { thread_id } => {
                        request(&mut stdin, sequence, "next", json!({ "threadId": thread_id }));
                        sequence += 1;
                    }
                    DebugCommand::StepIn { thread_id } => {
                        request(&mut stdin, sequence, "stepIn", json!({ "threadId": thread_id }));
                        sequence += 1;
                    }
                    DebugCommand::StepOut { thread_id } => {
                        request(&mut stdin, sequence, "stepOut", json!({ "threadId": thread_id }));
                        sequence += 1;
                    }
                    DebugCommand::Pause { thread_id } => {
                        request(&mut stdin, sequence, "pause", json!({ "threadId": thread_id }));
                        sequence += 1;
                    }
                    DebugCommand::Stop => {
                        request(
                            &mut stdin,
                            sequence,
                            "disconnect",
                            json!({ "terminateDebuggee": true, "restart": false }),
                        );
                        break;
                    }
                    DebugCommand::GetStackTrace { thread_id } => {
                        request(
                            &mut stdin,
                            sequence,
                            "stackTrace",
                            json!({ "threadId": thread_id, "startFrame": 0, "levels": 64 }),
                        );
                        sequence += 1;
                    }
                    DebugCommand::GetScopes { frame_id } => {
                        let seq = sequence;
                        sequence += 1;
                        command_pending
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .insert(seq, PendingRequest::Scopes(frame_id));
                        request(&mut stdin, seq, "scopes", json!({ "frameId": frame_id }));
                    }
                    DebugCommand::GetVariables { variables_reference } => {
                        let seq = sequence;
                        sequence += 1;
                        command_pending
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .insert(seq, PendingRequest::Variables(variables_reference));
                        request(
                            &mut stdin,
                            seq,
                            "variables",
                            json!({ "variablesReference": variables_reference }),
                        );
                    }
                    DebugCommand::ReadMemory { reference, count } => {
                        request(
                            &mut stdin,
                            sequence,
                            "readMemory",
                            json!({ "memoryReference": reference, "count": count }),
                        );
                        sequence += 1;
                    }
                    DebugCommand::WriteMemory { reference, data } => {
                        // DAP `writeMemory` takes hex-encoded bytes in `data`.
                        let encoded: String = data.iter().map(|byte| format!("{byte:02x}")).collect();
                        let address = parse_hex_address(&reference).unwrap_or(0);
                        let seq = sequence;
                        sequence += 1;
                        command_pending
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .insert(seq, PendingRequest::MemoryWrite { address });
                        request(
                            &mut stdin,
                            seq,
                            "writeMemory",
                            json!({
                                "memoryReference": reference,
                                "data": encoded,
                            }),
                        );
                    }
                    DebugCommand::Disassemble {
                        reference,
                        instruction_count,
                    } => {
                        request(
                            &mut stdin,
                            sequence,
                            "disassemble",
                            json!({
                                "memoryReference": reference,
                                "instructionOffset": 0,
                                "instructionCount": instruction_count,
                                "resolveSymbols": true,
                            }),
                        );
                        sequence += 1;
                    }
                    DebugCommand::SetDataBreakpoints(watchpoints) => {
                        let seq = sequence;
                        sequence += 1;
                        command_pending
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .insert(
                                seq,
                                PendingRequest::DataBreakpoints(
                                    watchpoints
                                        .iter()
                                        .map(|watchpoint| watchpoint.expression.clone())
                                        .collect(),
                                ),
                            );
                        request(&mut stdin, seq, "setDataBreakpoints", data_breakpoints_payload(&watchpoints));
                    }
                    DebugCommand::DataBreakpointInfo { expression, scope } => {
                        let seq = sequence;
                        sequence += 1;
                        command_pending
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .insert(
                                seq,
                                PendingRequest::DataBreakpointInfo(expression.clone()),
                            );
                        // Only `variablesReference` is sent: lldb-dap resolves the
                        // variable via the scope that actually contains it, so the
                        // caller tells us which scope listed the expression.
                        request(
                            &mut stdin,
                            seq,
                            "dataBreakpointInfo",
                            json!({
                                "name": expression,
                                "variablesReference": scope,
                            }),
                        );
                    }
                    DebugCommand::Threads => {
                        request(&mut stdin, sequence, "threads", json!({}));
                        sequence += 1;
                    }
                    DebugCommand::ConfigureTrace(trace) => {
                        if let Some(reason) = trace.validate().err() {
                            let _ = command_messages.send(DebugMessage::Error(format!(
                                "trace 配置无效，未下发: {reason}"
                            )));
                            continue;
                        }
                        // Trace configuration is heartbeat-loop work on target
                        // hardware: send the register writes and the final
                        // command, letting the UI know when the last one lands.
                        let requests = trace.requests();
                        let total = requests.len();
                        for (index, (command, arguments)) in requests.into_iter().enumerate() {
                            let seq = sequence;
                            sequence += 1;
                            if index + 1 == total {
                                command_pending
                                    .lock()
                                    .unwrap_or_else(|error| error.into_inner())
                                    .insert(
                                        seq,
                                        PendingRequest::TraceConfigured(trace.clone()),
                                    );
                            }
                            request(&mut stdin, seq, &command, arguments);
                        }
                    }
                    DebugCommand::StopTrace => {
                        for (command, arguments) in swvd::stop_requests() {
                            request(&mut stdin, sequence, &command, arguments);
                            sequence += 1;
                        }
                    }
                    DebugCommand::StartTraceStream { port, itm_port } => {
                        // Stop any previous reader before starting a new one.
                        if let Some(mut previous) = trace_stream.take() {
                            previous.stop();
                        }
                        let (packet_tx, packet_rx) = mpsc::channel::<Vec<swvd::Packet>>();
                        let forward = command_messages.clone();
                        // Forward decoded packets to the UI as trace messages.
                        let forwarder = std::thread::spawn(move || {
                            while let Ok(packets) = packet_rx.recv() {
                                if forward.send(DebugMessage::TracePackets(packets)).is_err() {
                                    break; // UI went away.
                                }
                            }
                        });
                        match swvd::TraceStream::spawn(
                            std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                            itm_port,
                            packet_tx,
                        ) {
                            Ok(stream) => {
                                let _ = forwarder;
                                let _ = command_messages.send(DebugMessage::Output {
                                    category: "console".into(),
                                    text: format!("SWO 流已连接(端口 {port})。"),
                                });
                                trace_stream = Some(stream);
                            }
                            Err(error) => {
                                let _ = command_messages.send(DebugMessage::Error(format!(
                                    "无法连接 SWO 数据流(端口 {port}): {error}"
                                )));
                            }
                        }
                    }
                    DebugCommand::StopTraceStream => {
                        if let Some(mut stream) = trace_stream.take() {
                            stream.stop();
                            let _ = command_messages.send(DebugMessage::Output {
                                category: "console".into(),
                                text: "SWO 数据流已停止。".into(),
                            });
                        }
                    }
                    DebugCommand::ScopeVariables { scope } => {
                        let seq = sequence;
                        sequence += 1;
                        command_pending
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .insert(
                                seq,
                                PendingRequest::ScopeVariables {
                                    scope: scope.clone(),
                                },
                            );
                        request(
                            &mut stdin,
                            seq,
                            "variables",
                            json!({
                                "variablesReference": swvd::scope_reference(&scope),
                            }),
                        );
                    }
                    DebugCommand::Evaluate {
                        expression,
                        scope_name,
                    } => {
                        let seq = sequence;
                        sequence += 1;
                        command_pending
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .insert(
                                seq,
                                PendingRequest::Evaluate {
                                    expression: expression.clone(),
                                    scope: scope_name.clone(),
                                },
                            );
                        request(
                            &mut stdin,
                            seq,
                            "evaluate",
                            json!({
                                "expression": expression,
                                // The C++ DAP extension uses `variablesReference`
                                // to look the expression up in that scope first.
                                "variablesReference": swvd::scope_reference(&scope_name),
                                "frameId": 0,
                                "context": "repl",
                            }),
                        );
                    }
                    DebugCommand::ResyncBreakpoints(files) => {
                        for (file, locations) in files {
                            sequence += 1;
                            if locations.is_empty() {
                                let _ = send_breakpoint_set(&mut stdin, sequence, &file, &locations);
                                continue;
                            }
                            command_pending
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .insert(sequence, PendingRequest::Breakpoints(file.clone()));
                            if send_breakpoint_set(&mut stdin, sequence, &file, &locations).is_err() {
                                let _ = command_messages.send(
                                    DebugMessage::Error(format!("重新设置断点失败: {file}")),
                                );
                            }
                        }
                    }
                    DebugCommand::InternalExit => break,
                }
            }
        })
        .map_err(|error| anyhow!("创建调试命令线程失败: {error}"))?;

    // ---- Reader thread: adapter traffic -> UI messages ----
    let reader_commands = commands.clone();
    let reader_pending = Arc::clone(&pending);
    thread::Builder::new()
        .name("ns-debug-reader".into())
        .spawn(move || {
            pump_events(
                &mut stdout,
                &mut child,
                &messages,
                &reader_commands,
                &reader_pending,
            );
        })
        .map_err(|error| anyhow!("创建调试消息线程失败: {error}"))?;

    Ok(DebugHandle {
        messages: message_rx,
        commands,
    })
}

/// Run the DAP handshake (`initialize` → `launch` → `setBreakpoints` →
/// `configurationDone`) against a fresh adapter child process. On failure the
/// child is killed and dropped so a retry can start a clean adapter.
#[allow(clippy::type_complexity)]
fn perform_handshake(
    config: &DebugConfig,
    adapter: &Path,
) -> Result<(
    Child,
    ChildStdin,
    BufReader<std::process::ChildStdout>,
    Vec<(String, Vec<DebugBreakpointResult>)>,
    u64,
)> {
    let mut child = Command::new(adapter)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| anyhow!("启动调试适配器 {} 失败: {error}", adapter.display()))?;
    let result = (|| {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("无法获取适配器 stdin"))?;
        let mut stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| anyhow!("无法获取适配器 stdout"))?,
        );

        // ---- Handshake: initialize -> launch -> setBreakpoints -> configurationDone ----
        let mut sequence = 1u64;
        let initialize_seq = sequence;
        request_and_wait(
            &mut stdin,
            &mut stdout,
            initialize_seq,
            "initialize",
            json!({
                "adapterID": "lldb",
                "pathFormat": "path",
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "supportsVariableType": true,
            }),
        )?;
        sequence += 1;

        let launch_seq = sequence;
        request_and_wait(
            &mut stdin,
            &mut stdout,
            launch_seq,
            "launch",
            json!({
                "program": config.program.display().to_string(),
                "args": config.args,
                "env": config.env,
                "cwd": config.cwd.display().to_string(),
                "stopOnEntry": false,
            }),
        )?;
        sequence += 1;

        let mut launch_breakpoint_results: Vec<(String, Vec<DebugBreakpointResult>)> = Vec::new();
        for (file, locations) in group_breakpoints(&config.breakpoints) {
            if locations.is_empty() {
                continue;
            }
            sequence += 1;
            send_breakpoint_set(&mut stdin, sequence, &file, &locations)?;
            let response = wait_for_response(&mut stdout, sequence)
                .map_err(|error| anyhow!("读取 setBreakpoints 响应失败: {error}"))?;
            launch_breakpoint_results
                .push((file, parse_breakpoint_results(&response)));
        }

        let configuration_seq = sequence + 1;
        request_and_wait(
            &mut stdin,
            &mut stdout,
            configuration_seq,
            "configurationDone",
            json!({}),
        )?;

        Ok((stdin, stdout, launch_breakpoint_results, configuration_seq))
    })();

    match result {
        Ok((stdin, stdout, launch_breakpoint_results, configuration_seq)) => Ok((
            child,
            stdin,
            stdout,
            launch_breakpoint_results,
            configuration_seq,
        )),
        Err(error) => {
            let _ = child.kill();
            Err(error)
        }
    }
}

fn request_and_wait(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<std::process::ChildStdout>,
    sequence: u64,
    command: &str,
    arguments: Value,
) -> Result<()> {
    dap::write_message(stdin, &dap::request(command, sequence, arguments))
        .map_err(|error| anyhow!("发送 {command} 请求失败: {error}"))?;
    wait_for_response(stdout, sequence).map(|_| ())
}

fn send_breakpoint_set(
    stdin: &mut ChildStdin,
    sequence: u64,
    file: &str,
    locations: &[DebugBreakpointLocation],
) -> std::io::Result<()> {
    let breakpoints: Vec<Value> = locations
        .iter()
        .map(|location| {
            let mut breakpoint = json!({ "line": location.line });
            if location.hardware {
                breakpoint["mode"] = Value::String("hardware".into());
            }
            if let Some(condition) = &location.condition {
                if !condition.trim().is_empty() {
                    breakpoint["condition"] = Value::String(condition.clone());
                }
            }
            if let Some(hit) = &location.hit_condition {
                if !hit.trim().is_empty() {
                    breakpoint["hitCondition"] = Value::String(hit.clone());
                }
            }
            if let Some(log_message) = &location.log_message {
                if !log_message.trim().is_empty() {
                    breakpoint["logMessage"] = Value::String(log_message.clone());
                }
            }
            breakpoint
        })
        .collect();
    let payload = dap::request(
        "setBreakpoints",
        sequence,
        json!({
            "source": { "path": file },
            "breakpoints": breakpoints,
        }),
    );
    dap::write_message(stdin, &payload)
}

/// Parse the `setBreakpoints` response body into per-line outcomes.
///
/// The DAP `body.breakpoints` array is aligned with the request in order; each
/// entry carries `verified` (whether the location bound) and, when it could
/// not, a `reason`. Missing fields are treated as unverified.
fn parse_breakpoint_results(response: &Value) -> Vec<DebugBreakpointResult> {
    let Some(entries) = response
        .get("body")
        .and_then(|body| body.get("breakpoints"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    entries
        .iter()
        .map(|entry| {
            let line = entry.get("line").and_then(Value::as_u64).unwrap_or(0) as usize;
            let verified = entry
                .get("verified")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let reason = if verified {
                String::new()
            } else {
                entry
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("未知原因")
                    .to_string()
            };
            DebugBreakpointResult { line, verified, reason }
        })
        .collect()
}

/// Build the `setDataBreakpoints` request body. Watchpoints without a resolved
/// `dataId` are still sent so the adapter reports them as unverified (keeping
/// the response aligned with the requested expressions).
fn data_breakpoints_payload(watchpoints: &[WatchpointRequest]) -> Value {
    let breakpoints: Vec<Value> = watchpoints
        .iter()
        .map(|watchpoint| {
            json!({
                "dataId": watchpoint.data_id,
                "accessType": watchpoint.access_type,
            })
        })
        .collect();
    json!({ "breakpoints": breakpoints })
}

/// Map a `dataBreakpointInfo` response body to [`DebugMessage::DataBreakpointInfo`].
fn parse_data_breakpoint_info(
    expression: String,
    response: &Value,
    success: bool,
) -> DebugMessage {
    if !success {
        let reason = response
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("未知错误")
            .to_string();
        return DebugMessage::DataBreakpointInfo {
            expression,
            data_id: None,
            access_types: Vec::new(),
            description: format!("查询数据断点失败: {reason}"),
        };
    }
    let body = &response["body"];
    let data_id = body.get("dataId").and_then(Value::as_str).map(str::to_string);
    let access_types = body
        .get("accessTypes")
        .and_then(Value::as_array)
        .map(|types| {
            types
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let description = body
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    DebugMessage::DataBreakpointInfo {
        expression,
        data_id,
        access_types,
        description,
    }
}

/// Map a `setDataBreakpoints` response to per-expression results. The body's
/// `breakpoints` array follows the request order.
fn parse_watchpoint_results(expressions: &[String], response: &Value) -> Vec<WatchpointResult> {
    let entries = response["body"]["breakpoints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    expressions
        .iter()
        .enumerate()
        .map(|(index, expression)| {
            let entry = entries.get(index);
            let verified = entry
                .and_then(|entry| entry.get("verified"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let reason = if verified {
                String::new()
            } else {
                entry
                    .and_then(|entry| entry.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("适配器未能绑定该数据断点")
                    .to_string()
            };
            let adapter_id = entry
                .and_then(|entry| entry.get("id"))
                .and_then(Value::as_u64);
            WatchpointResult {
                expression: expression.clone(),
                verified,
                reason,
                adapter_id,
            }
        })
        .collect()
}

fn request(stdin: &mut ChildStdin, sequence: u64, command: &str, arguments: Value) {
    let _ = dap::write_message(stdin, &dap::request(command, sequence, arguments));
}

fn pump_events(
    reader: &mut BufReader<std::process::ChildStdout>,
    child: &mut Child,
    messages: &Sender<DebugMessage>,
    commands: &Sender<DebugCommand>,
    pending: &Mutex<HashMap<u64, PendingRequest>>,
) {
    let mut terminated = false;
    loop {
        let message = match dap::read_message(reader) {
            Ok(message) => message,
            Err(_) => {
                mark_terminated(messages, &mut terminated);
                break;
            }
        };
        translate_message(&message, messages, &mut terminated, pending);
        if terminated {
            break;
        }
    }
    let _ = commands.send(DebugCommand::InternalExit);
    let _ = child.wait();
}

fn mark_terminated(messages: &Sender<DebugMessage>, terminated: &mut bool) {
    if !*terminated {
        *terminated = true;
        let _ = messages.send(DebugMessage::Terminated);
    }
}

fn translate_message(
    message: &Value,
    messages: &Sender<DebugMessage>,
    terminated: &mut bool,
    pending: &Mutex<HashMap<u64, PendingRequest>>,
) {
    let message_type = message
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match message_type {
        "event" => translate_event(message, messages, terminated),
        "response" => translate_response(message, messages, pending),
        _ => {}
    }
}

fn translate_event(event: &Value, messages: &Sender<DebugMessage>, terminated: &mut bool) {
    match event
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "stopped" => {
            let body = &event["body"];
            let reason = body
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("stopped")
                .to_string();
            let thread_id = body.get("threadId").and_then(Value::as_u64).unwrap_or(0);
            // lldb-dap identifies watchpoint stops only via `description`
            // ("watchpoint 1"); forward it so the UI can flag the right one.
            if reason == "data breakpoint"
                && let Some(description) = body.get("description").and_then(Value::as_str)
            {
                let _ = messages.send(DebugMessage::WatchpointHit {
                    description: description.to_string(),
                });
            }
            // Capture the adapter's descriptive text (e.g. "EXC_BAD_ACCESS",
            // "SIGSEGV", "hardware exception") so the UI can classify the stop.
            let description = body
                .get("description")
                .and_then(Value::as_str)
                .or_else(|| body.get("text").and_then(Value::as_str))
                .map(str::to_string);
            let _ = messages.send(DebugMessage::Stopped {
                reason,
                thread_id,
                description,
            });
        }
        "continued" => {
            let _ = messages.send(DebugMessage::Continued);
        }
        "caret" => {
            let body = &event["body"];
            if let Some(line) = body.get("line").and_then(Value::as_u64) {
                let _ = messages.send(DebugMessage::Caret {
                    path: body
                        .get("source")
                        .and_then(|source| source.get("path"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    line: line as usize,
                });
            }
        }
        "frameStoppedLocation" => {
            if let Some((path, line, _thread_id)) = parse_frame_location(&event["body"]) {
                let _ = messages.send(DebugMessage::Caret {
                    path: Some(path),
                    line,
                });
            }
        }
        "terminated" | "exited" => mark_terminated(messages, terminated),
        "output" => {
            let body = &event["body"];
            let text = body
                .get("output")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let category = body
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or("console")
                .to_string();
            if !text.is_empty() {
                let _ = messages.send(DebugMessage::Output { category, text });
            }
        }
        _ => {}
    }
}

fn parse_hex_address(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u64::from_str_radix(hex, 16).ok()
}

fn pending_take(
    pending: &Mutex<HashMap<u64, PendingRequest>>,
    sequence: Option<u64>,
) -> Option<PendingRequest> {
    sequence.and_then(|seq| pending.lock().ok()?.remove(&seq))
}

/// Parse the `location` object of a `frameStoppedLocation` event.
fn parse_frame_location(body: &Value) -> Option<(String, usize, u64)> {
    let location = body.get("frame")?.get("location")?;
    let path = location.get("path")?.as_str()?.to_string();
    let line = location.get("line")?.as_u64()? as usize;
    let thread_id = body.get("threadId").and_then(Value::as_u64).unwrap_or(0);
    Some((path, line, thread_id))
}

/// Parse a `variables` response body into [`Variable`]s.
fn parse_variables(response: &Value) -> Vec<Variable> {
    response["body"]["variables"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|variable| {
            Some(Variable {
                name: variable.get("name")?.as_str()?.to_string(),
                value: variable
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or("<未知>")
                    .to_string(),
                variables_reference: variable
                    .get("variablesReference")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            })
        })
        .collect()
}

fn parse_instruction(instruction: &Value) -> Option<Instruction> {
    Some(Instruction {
        address: parse_hex_address(instruction.get("address")?.as_str()?)?,
        instruction: instruction.get("instruction")?.as_str()?.to_string(),
        bytes: instruction
            .get("instructionBytes")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        line: instruction
            .get("line")
            .and_then(Value::as_u64)
            .map(|line| line as usize),
        file: instruction
            .get("location")
            .and_then(|location| location.get("path"))
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn translate_response(
    response: &Value,
    messages: &Sender<DebugMessage>,
    pending: &Mutex<HashMap<u64, PendingRequest>>,
) {
    let command = response
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let request_seq = response.get("request_seq").and_then(Value::as_u64);
    let success = response
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let error = || {
        response
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("未知错误")
            .to_string()
    };
    match command {
        "stackTrace" => {
            if !success {
                let _ = messages.send(DebugMessage::Error(format!("获取调用栈失败: {}", error())));
                return;
            }
            let frames = response["body"]["stackFrames"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|frame| {
                    Some(Frame {
                        id: frame.get("id")?.as_u64()?,
                        name: frame.get("name")?.as_str()?.to_string(),
                        file: frame
                            .get("source")
                            .and_then(|source| source.get("path"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        line: frame
                            .get("line")
                            .and_then(Value::as_u64)
                            .map(|line| line as usize),
                        column: frame
                            .get("column")
                            .and_then(Value::as_u64)
                            .map(|column| column as usize),
                        pc: frame
                            .get("instructionPointerReference")
                            .and_then(Value::as_str)
                            .and_then(parse_hex_address),
                    })
                })
                .collect();
            let _ = messages.send(DebugMessage::StackTrace { frames });
        }
        "scopes" => {
            if !success {
                let _ = messages.send(DebugMessage::Error(format!("获取作用域失败: {}", error())));
                return;
            }
            let frame_id = match pending_take(pending, request_seq) {
                Some(PendingRequest::Scopes(frame_id)) => frame_id,
                _ => 0,
            };
            let scopes = response["body"]["scopes"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|scope| {
                    Some(Scope {
                        id: scope.get("variablesReference")?.as_u64()?,
                        name: scope.get("name")?.as_str()?.to_string(),
                        expensive: scope
                            .get("expensive")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect();
            let _ = messages.send(DebugMessage::Scopes { frame_id, scopes });
        }
        "variables" => {
            if !success {
                let _ = messages.send(DebugMessage::Error(format!("获取变量失败: {}", error())));
                return;
            }
            let pending_value = pending_take(pending, request_seq);
            match pending_value {
                Some(PendingRequest::ScopeVariables { scope }) => {
                    let reference = swvd::scope_reference(&scope);
                    let variables = parse_variables(&response);
                    let _ = messages.send(DebugMessage::ScopeVariables {
                        scope,
                        reference,
                        variables,
                    });
                    return;
                }
                _ => {}
            }
            let reference = match pending_value {
                Some(PendingRequest::Variables(reference)) => reference,
                _ => 0,
            };
            let variables = parse_variables(response);
            let _ = messages.send(DebugMessage::Variables { reference, variables });
        }
        "readMemory" => {
            if !success {
                let _ = messages.send(DebugMessage::Error(format!("读取内存失败: {}", error())));
                return;
            }
            let address = response["body"]["address"]
                .as_str()
                .and_then(parse_hex_address)
                .unwrap_or(0);
            let data = response["body"]["data"]
                .as_str()
                .and_then(|encoded| {
                    use base64::{Engine, engine::general_purpose::STANDARD};
                    STANDARD.decode(encoded).ok()
                })
                .unwrap_or_default();
            let _ = messages.send(DebugMessage::Memory { address, data });
        }
        "disassemble" => {
            if !success {
                let _ = messages.send(DebugMessage::Error(format!("反汇编失败: {}", error())));
                return;
            }
            let instructions = response["body"]["instructions"]
                .as_array()
                .map(|instructions| instructions.iter().filter_map(parse_instruction).collect())
                .unwrap_or_default();
            let _ = messages.send(DebugMessage::Disassembly { instructions });
        }
        "setBreakpoints" => {
            if !success {
                let _ = messages.send(DebugMessage::Error(format!("设置断点失败: {}", error())));
            }
            match pending_take(pending, request_seq) {
                Some(PendingRequest::Breakpoints(file)) => {
                    let results = parse_breakpoint_results(response);
                    if !results.is_empty() {
                        let _ = messages.send(DebugMessage::BreakpointResults { file, results });
                    }
                }
                _ => {}
            }
        }
        "threads" => {
            if !success {
                let _ = messages.send(DebugMessage::Error(format!("获取线程列表失败: {}", error())));
                return;
            }
            let threads = response["body"]["threads"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|thread| {
                    Some(ThreadInfo {
                        id: thread.get("id")?.as_u64()?,
                        name: thread
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect();
            let _ = messages.send(DebugMessage::Threads { threads });
        }
        "evaluate" => {
            let context = match pending_take(pending, request_seq) {
                Some(PendingRequest::Evaluate { expression, scope }) => (expression, scope),
                _ => return,
            };
            let (expression, scope) = context;
            if !success {
                let _ = messages.send(DebugMessage::Error(format!(
                    "表达式 `{expression}` 求值失败: {}",
                    error()
                )));
                return;
            }
            let body = &response["body"];
            let result = body
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or("<无结果>")
                .to_string();
            let type_name = body
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string);
            // `scope` is only identification for the UI; the evaluated message
            // carries the expression so the caller can match it.
            let _ = scope;
            let _ = messages.send(DebugMessage::Evaluated {
                expression,
                result,
                type_name,
            });
        }
        "setVariable" => {
            let context = match pending_take(pending, request_seq) {
                Some(PendingRequest::VariableSet {
                    variables_reference,
                    name,
                }) => (variables_reference, name),
                _ => return,
            };
            let (variables_reference, name) = context;
            if !success {
                let _ = messages.send(DebugMessage::Error(format!(
                    "设置变量 `{name}` 失败: {}",
                    error()
                )));
                return;
            }
            let body = &response["body"];
            let value = body
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let type_name = body
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string);
            let _ = messages.send(DebugMessage::VariableSet {
                variables_reference,
                name,
                value,
                type_name,
            });
        }
        "writeMemory" => {
            match pending_take(pending, request_seq) {
                Some(PendingRequest::TraceConfigured(trace)) => {
                    if !success {
                        let _ = messages.send(DebugMessage::Error(format!(
                            "trace 寄存器写入失败: {}",
                            error()
                        )));
                    } else {
                        let _ = messages.send(DebugMessage::TraceConfigured {
                            config: trace,
                        });
                    }
                }
                Some(PendingRequest::MemoryWrite { address }) => {
                    if success {
                        let bytes = response
                            .get("body")
                            .and_then(|body| body.get("bytesWritten"))
                            .and_then(|value| value.as_u64())
                            .unwrap_or(0);
                        let _ = messages.send(DebugMessage::MemoryWritten {
                            address,
                            bytes: bytes as usize,
                        });
                    } else {
                        let _ = messages.send(DebugMessage::Error(format!(
                            "内存写入失败: {}",
                            error()
                        )));
                    }
                }
                _ => {}
            }
        }
        "disconnect" => {
            // Termination is reported via the "terminated"/"exited" event.
        }
        "dataBreakpointInfo" => {
            let expression = match pending_take(pending, request_seq) {
                Some(PendingRequest::DataBreakpointInfo(expression)) => expression,
                _ => return,
            };
            let message = parse_data_breakpoint_info(expression, response, success);
            let _ = messages.send(message);
        }
        "setDataBreakpoints" => {
            let expressions = match pending_take(pending, request_seq) {
                Some(PendingRequest::DataBreakpoints(expressions)) => expressions,
                _ => {
                    if !success {
                        let _ =
                            messages.send(DebugMessage::Error(format!("设置数据断点失败: {}", error())));
                    }
                    return;
                }
            };
            if !success {
                let _ = messages.send(DebugMessage::Error(format!(
                    "设置数据断点失败: {}",
                    error()
                )));
            }
            let results = parse_watchpoint_results(&expressions, response);
            let _ = messages.send(DebugMessage::WatchpointResults { results });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending() -> Mutex<HashMap<u64, PendingRequest>> {
        Mutex::new(HashMap::new())
    }

    #[test]
    fn groups_breakpoints_by_file_and_sorts() {
        let breakpoints = vec![
            DebugBreakpoint {
                path: "src/b.c".into(),
                line: 3,
                ..Default::default()
            },
            DebugBreakpoint {
                path: "src/a.c".into(),
                line: 9,
                condition: Some("x == 1".into()),
                ..Default::default()
            },
            DebugBreakpoint {
                path: "src/a.c".into(),
                line: 4,
                ..Default::default()
            },
        ];
        let grouped = group_breakpoints(&breakpoints);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].0, "src/a.c");
        assert_eq!(
            grouped[0].1,
            vec![
                DebugBreakpointLocation { line: 4, ..Default::default() },
                DebugBreakpointLocation {
                    line: 9,
                    condition: Some("x == 1".into()),
                    ..Default::default()
                },
            ]
        );
        assert_eq!(grouped[1].0, "src/b.c");
        assert_eq!(grouped[1].1, vec![DebugBreakpointLocation { line: 3, ..Default::default() }]);
    }

    #[test]
    fn duplicate_breakpoints_are_deduplicated() {
        let breakpoints = vec![
            DebugBreakpoint { path: "src/a.c".into(), line: 4, ..Default::default() },
            DebugBreakpoint { path: "src/a.c".into(), line: 4, ..Default::default() },
        ];
        let grouped = group_breakpoints(&breakpoints);
        assert_eq!(grouped[0].1, vec![DebugBreakpointLocation { line: 4, ..Default::default() }]);
    }

    #[test]
    fn parses_breakpoint_results_from_response_body() {
        let response = json!({
            "type": "response",
            "command": "setBreakpoints",
            "success": true,
            "body": {
                "breakpoints": [
                    { "line": 4, "verified": true },
                    { "line": 7, "verified": false, "reason": "source not found" }
                ]
            }
        });
        let parsed = parse_breakpoint_results(&response);
        assert_eq!(
            parsed,
            vec![
                DebugBreakpointResult { line: 4, verified: true, reason: String::new() },
                DebugBreakpointResult {
                    line: 7,
                    verified: false,
                    reason: "source not found".into()
                },
            ]
        );
    }

    #[test]
    fn set_breakpoints_response_yields_results_only_with_pending_file() {
        let (tx, rx) = mpsc::channel();
        let pending = pending();
        pending
            .lock()
            .unwrap()
            .insert(21, PendingRequest::Breakpoints("/tmp/e2e.c".into()));
        translate_response(
            &json!({
                "type": "response",
                "command": "setBreakpoints",
                "request_seq": 21,
                "success": true,
                "body": { "breakpoints": [{ "line": 4, "verified": true }] }
            }),
            &tx,
            &pending,
        );
        match rx.try_recv().unwrap() {
            DebugMessage::BreakpointResults { file, results } => {
                assert_eq!(file, "/tmp/e2e.c");
                assert_eq!(
                    results,
                    vec![DebugBreakpointResult {
                        line: 4,
                        verified: true,
                        reason: String::new()
                    }]
                );
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn set_breakpoints_response_without_pending_file_stays_silent() {
        let (tx, rx) = mpsc::channel();
        translate_response(
            &json!({
                "type": "response",
                "command": "setBreakpoints",
                "request_seq": 22,
                "success": true,
                "body": { "breakpoints": [{ "line": 4, "verified": true }] }
            }),
            &tx,
            &pending(),
        );
        assert!(matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn translates_evaluate_response_with_pending_expression() {
        let (tx, rx) = mpsc::channel();
        let pending = pending();
        pending.lock().unwrap().insert(
            51,
            PendingRequest::Evaluate {
                expression: "marker".into(),
                scope: "GlobalScope".into(),
            },
        );
        translate_response(
            &json!({
                "type": "response",
                "command": "evaluate",
                "request_seq": 51,
                "success": true,
                "body": { "result": "7", "type": "int" }
            }),
            &tx,
            &pending,
        );
        match rx.try_recv().unwrap() {
            DebugMessage::Evaluated {
                expression,
                result,
                type_name,
            } => {
                assert_eq!(expression, "marker");
                assert_eq!(result, "7");
                assert_eq!(type_name.as_deref(), Some("int"));
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn failed_evaluate_reports_expression() {
        let (tx, rx) = mpsc::channel();
        let pending = pending();
        pending.lock().unwrap().insert(
            52,
            PendingRequest::Evaluate {
                expression: "nope".into(),
                scope: "LocalScope".into(),
            },
        );
        translate_response(
            &json!({
                "type": "response",
                "command": "evaluate",
                "request_seq": 52,
                "success": false,
                "message": "unknown identifier"
            }),
            &tx,
            &pending,
        );
        match rx.try_recv().unwrap() {
            DebugMessage::Error(message) => {
                assert!(message.contains("nope"), "{message}");
                assert!(message.contains("unknown identifier"), "{message}");
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn scope_variables_response_carries_scope_name() {
        let (tx, rx) = mpsc::channel();
        let pending = pending();
        pending.lock().unwrap().insert(
            61,
            PendingRequest::ScopeVariables {
                scope: "GlobalScope".into(),
            },
        );
        translate_response(
            &json!({
                "type": "response",
                "command": "variables",
                "request_seq": 61,
                "success": true,
                "body": { "variables": [{ "name": "marker", "value": "0", "variablesReference": 0 }] }
            }),
            &tx,
            &pending,
        );
        match rx.try_recv().unwrap() {
            DebugMessage::ScopeVariables {
                scope,
                reference,
                variables,
            } => {
                assert_eq!(scope, "GlobalScope");
                assert_eq!(reference, swvd::scope_reference("GlobalScope"));
                assert_eq!(variables.len(), 1);
                assert_eq!(variables[0].name, "marker");
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn threads_response_lists_threads() {
        let (tx, rx) = mpsc::channel();
        translate_response(
            &json!({
                "type": "response",
                "command": "threads",
                "request_seq": 71,
                "success": true,
                "body": { "threads": [{ "id": 1, "name": "main" }] }
            }),
            &tx,
            &pending(),
        );
        match rx.try_recv().unwrap() {
            DebugMessage::Threads { threads } => {
                assert_eq!(threads, vec![ThreadInfo { id: 1, name: "main".into() }]);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn watchpoint_hit_event_is_forwarded_before_stop() {
        let (tx, rx) = mpsc::channel();
        let mut terminated = false;
        translate_event(
            &json!({
                "type": "event",
                "event": "stopped",
                "body": { "reason": "data breakpoint", "description": "watchpoint 2", "threadId": 5 }
            }),
            &tx,
            &mut terminated,
        );
        assert!(matches!(
            rx.try_recv().unwrap(),
            DebugMessage::WatchpointHit { description } if description == "watchpoint 2"
        ));
        assert!(matches!(rx.try_recv().unwrap(), DebugMessage::Stopped { .. }));
    }

    #[test]
    fn caret_event_updates_location_only() {
        let (tx, rx) = mpsc::channel();
        let mut terminated = false;
        translate_event(
            &json!({
                "type": "event",
                "event": "caret",
                "body": { "source": { "path": "/tmp/a.c" }, "line": 12 }
            }),
            &tx,
            &mut terminated,
        );
        match rx.try_recv().unwrap() {
            DebugMessage::Caret { path, line } => {
                assert_eq!(path.as_deref(), Some("/tmp/a.c"));
                assert_eq!(line, 12);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn translates_data_breakpoint_info_with_pending_expression() {
        let (tx, rx) = mpsc::channel();
        let pending = pending();
        pending
            .lock()
            .unwrap()
            .insert(31, PendingRequest::DataBreakpointInfo("marker".into()));
        translate_response(
            &json!({
                "type": "response",
                "command": "dataBreakpointInfo",
                "request_seq": 31,
                "success": true,
                "body": {
                    "dataId": "7ff7/4",
                    "description": "4 bytes at 7ff7 marker",
                    "accessTypes": ["read", "write", "readWrite"]
                }
            }),
            &tx,
            &pending,
        );
        match rx.try_recv().unwrap() {
            DebugMessage::DataBreakpointInfo {
                expression,
                data_id,
                access_types,
                description,
            } => {
                assert_eq!(expression, "marker");
                assert_eq!(data_id.as_deref(), Some("7ff7/4"));
                assert_eq!(access_types, ["read", "write", "readWrite"]);
                assert_eq!(description, "4 bytes at 7ff7 marker");
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn data_breakpoint_info_without_data_id_is_unwatchable() {
        let (tx, rx) = mpsc::channel();
        let pending = pending();
        pending
            .lock()
            .unwrap()
            .insert(32, PendingRequest::DataBreakpointInfo("marker".into()));
        translate_response(
            &json!({
                "type": "response",
                "command": "dataBreakpointInfo",
                "request_seq": 32,
                "success": true,
                "body": { "dataId": null, "description": "memory region has no permissions" }
            }),
            &tx,
            &pending,
        );
        match rx.try_recv().unwrap() {
            DebugMessage::DataBreakpointInfo { data_id, access_types, .. } => {
                assert_eq!(data_id, None);
                assert!(access_types.is_empty());
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn data_breakpoint_info_ignored_without_pending_expression() {
        let (tx, rx) = mpsc::channel();
        translate_response(
            &json!({
                "type": "response",
                "command": "dataBreakpointInfo",
                "request_seq": 33,
                "success": true,
                "body": { "dataId": "x/4" }
            }),
            &tx,
            &pending(),
        );
        assert!(matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn set_data_breakpoints_response_maps_results_by_expression() {
        let (tx, rx) = mpsc::channel();
        let pending = pending();
        pending.lock().unwrap().insert(
            41,
            PendingRequest::DataBreakpoints(vec!["marker".into(), "other".into()]),
        );
        translate_response(
            &json!({
                "type": "response",
                "command": "setDataBreakpoints",
                "request_seq": 41,
                "success": true,
                "body": {
                    "breakpoints": [
                        { "verified": true, "id": 3 },
                        { "verified": false, "message": "目标不支持更多观察点" }
                    ]
                }
            }),
            &tx,
            &pending,
        );
        match rx.try_recv().unwrap() {
            DebugMessage::WatchpointResults { results } => {
                assert_eq!(
                    results,
                    vec![
                        WatchpointResult {
                            expression: "marker".into(),
                            verified: true,
                            reason: String::new(),
                            adapter_id: Some(3),
                        },
                        WatchpointResult {
                            expression: "other".into(),
                            verified: false,
                            reason: "目标不支持更多观察点".into(),
                            adapter_id: None,
                        },
                    ]
                );
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn data_breakpoints_payload_keeps_access_type_and_order() {
        let payload = data_breakpoints_payload(&[
            WatchpointRequest {
                expression: "marker".into(),
                data_id: "a/4".into(),
                access_type: "write".into(),
            },
            WatchpointRequest {
                expression: "other".into(),
                data_id: String::new(),
                access_type: "readWrite".into(),
            },
        ]);
        assert_eq!(
            payload,
            json!({
                "breakpoints": [
                    { "dataId": "a/4", "accessType": "write" },
                    { "dataId": "", "accessType": "readWrite" }
                ]
            })
        );
    }

    #[test]
    fn last_trace_write_reports_configured_trace() {
        let (tx, rx) = mpsc::channel();
        let pending = pending();
        let config = TraceConfig {
            itm_port: 2,
            ..TraceConfig::default()
        };
        pending
            .lock()
            .unwrap()
            .insert(81, PendingRequest::TraceConfigured(config.clone()));
        translate_response(
            &json!({
                "type": "response",
                "command": "writeMemory",
                "request_seq": 81,
                "success": true,
                "body": {}
            }),
            &tx,
            &pending,
        );
        match rx.try_recv().unwrap() {
            DebugMessage::TraceConfigured { config: received } => {
                assert_eq!(received.itm_port, 2);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn translates_stopped_event() {
        let (tx, rx) = mpsc::channel();
        let mut terminated = false;
        translate_event(
            &json!({ "type": "event", "event": "stopped", "body": { "reason": "breakpoint", "threadId": 42 } }),
            &tx,
            &mut terminated,
        );
        assert!(!terminated);
        match rx.try_recv().unwrap() {
            DebugMessage::Stopped { reason, thread_id, description } => {
                assert_eq!(reason, "breakpoint");
                assert_eq!(thread_id, 42);
                assert!(description.is_none());
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn stopped_event_captures_signal_description() {
        let (tx, rx) = mpsc::channel();
        let mut terminated = false;
        translate_event(
            &json!({ "type": "event", "event": "stopped", "body": {
                "reason": "exception", "threadId": 1, "description": "EXC_BAD_ACCESS"
            } }),
            &tx,
            &mut terminated,
        );
        match rx.try_recv().unwrap() {
            DebugMessage::Stopped { description, .. } => {
                assert_eq!(description.as_deref(), Some("EXC_BAD_ACCESS"));
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn translates_terminated_event() {
        let (tx, rx) = mpsc::channel();
        let mut terminated = false;
        translate_event(
            &json!({ "type": "event", "event": "terminated" }),
            &tx,
            &mut terminated,
        );
        assert!(terminated);
        assert!(matches!(rx.try_recv().unwrap(), DebugMessage::Terminated));
    }

    #[test]
    fn translates_stack_trace_response() {
        let (tx, rx) = mpsc::channel();
        translate_response(
            &json!({
                "type": "response",
                "command": "stackTrace",
                "request_seq": 5,
                "success": true,
                "body": {
                    "stackFrames": [
                        { "id": 1, "name": "main", "source": { "path": "/tmp/e2e.c" }, "line": 4, "column": 5, "instructionPointerReference": "0x1000003e8" }
                    ]
                }
            }),
            &tx,
            &pending(),
        );
        match rx.try_recv().unwrap() {
            DebugMessage::StackTrace { frames } => {
                assert_eq!(frames.len(), 1);
                assert_eq!(frames[0].name, "main");
                assert_eq!(frames[0].file.as_deref(), Some("/tmp/e2e.c"));
                assert_eq!(frames[0].line, Some(4));
                assert_eq!(frames[0].column, Some(5));
                assert_eq!(frames[0].pc, Some(0x1000003e8));
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn translates_scopes_response_with_pending_frame() {
        let (tx, rx) = mpsc::channel();
        let pending = pending();
        pending.lock().unwrap().insert(11, PendingRequest::Scopes(524_288));
        translate_response(
            &json!({
                "type": "response",
                "command": "scopes",
                "request_seq": 11,
                "success": true,
                "body": {
                    "scopes": [
                        { "name": "Locals", "variablesReference": 1, "expensive": false },
                        { "name": "Registers", "variablesReference": 3, "expensive": false }
                    ]
                }
            }),
            &tx,
            &pending,
        );
        match rx.try_recv().unwrap() {
            DebugMessage::Scopes { frame_id, scopes } => {
                assert_eq!(frame_id, 524_288);
                assert_eq!(scopes.len(), 2);
                assert_eq!(scopes[1].name, "Registers");
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn translates_variables_response_with_pending_reference() {
        let (tx, rx) = mpsc::channel();
        let pending = pending();
        pending.lock().unwrap().insert(7, PendingRequest::Variables(3));
        translate_response(
            &json!({
                "type": "response",
                "command": "variables",
                "request_seq": 7,
                "success": true,
                "body": {
                    "variables": [
                        { "name": "General Purpose Registers", "value": "", "variablesReference": 4 }
                    ]
                }
            }),
            &tx,
            &pending,
        );
        match rx.try_recv().unwrap() {
            DebugMessage::Variables { reference, variables } => {
                assert_eq!(reference, 3);
                assert_eq!(variables.len(), 1);
                assert_eq!(variables[0].variables_reference, 4);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn translates_read_memory_response() {
        let (tx, rx) = mpsc::channel();
        translate_response(
            &json!({
                "type": "response",
                "command": "readMemory",
                "request_seq": 9,
                "success": true,
                "body": { "address": "0x1000003e8", "data": "3q2+7w==" }
            }),
            &tx,
            &pending(),
        );
        match rx.try_recv().unwrap() {
            DebugMessage::Memory { address, data } => {
                assert_eq!(address, 0x1000003e8);
                assert_eq!(data, vec![0xDE, 0xAD, 0xBE, 0xEF]);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn translates_write_memory_response() {
        let (tx, rx) = mpsc::channel();
        let pending = pending();
        pending
            .lock()
            .unwrap()
            .insert(9, PendingRequest::MemoryWrite { address: 0x2000_0010 });
        translate_response(
            &json!({
                "type": "response",
                "command": "writeMemory",
                "request_seq": 9,
                "success": true,
                "body": { "bytesWritten": 4 }
            }),
            &tx,
            &pending,
        );
        match rx.try_recv().unwrap() {
            DebugMessage::MemoryWritten { address, bytes } => {
                assert_eq!(address, 0x2000_0010);
                assert_eq!(bytes, 4);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn write_memory_reports_failure() {
        let (tx, rx) = mpsc::channel();
        let pending = pending();
        pending
            .lock()
            .unwrap()
            .insert(9, PendingRequest::MemoryWrite { address: 0x2000_0010 });
        translate_response(
            &json!({
                "type": "response",
                "command": "writeMemory",
                "request_seq": 9,
                "success": false,
                "message": "permission denied"
            }),
            &tx,
            &pending,
        );
        match rx.try_recv().unwrap() {
            DebugMessage::Error(text) => assert!(text.contains("内存写入失败")),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn parses_hex_address_with_prefix_and_without() {
        assert_eq!(parse_hex_address("0x20000010"), Some(0x2000_0010));
        assert_eq!(parse_hex_address("0X20000010"), Some(0x2000_0010));
        assert_eq!(parse_hex_address("20000010"), Some(0x2000_0010));
        assert_eq!(parse_hex_address("  0x10 "), Some(16));
        assert_eq!(parse_hex_address("zz"), None);
    }

    #[test]
    fn translates_disassemble_response() {
        let (tx, rx) = mpsc::channel();
        translate_response(
            &json!({
                "type": "response",
                "command": "disassemble",
                "request_seq": 12,
                "success": true,
                "body": {
                    "instructions": [
                        { "address": "0x1000003e8", "instruction": " movl 0xc12(%rip), %eax ; marker", "instructionBytes": "8b 05 12 0c 00 00", "line": 4, "location": { "name": "e2e.c", "path": "/tmp/e2e.c" } },
                        { "address": "0x1000003ee", "instruction": " addl $0x1, %eax", "instructionBytes": "83 c0 01" }
                    ]
                }
            }),
            &tx,
            &pending(),
        );
        match rx.try_recv().unwrap() {
            DebugMessage::Disassembly { instructions } => {
                assert_eq!(instructions.len(), 2);
                assert_eq!(instructions[0].address, 0x1000003e8);
                assert_eq!(instructions[0].line, Some(4));
                assert_eq!(instructions[0].file.as_deref(), Some("/tmp/e2e.c"));
                assert_eq!(instructions[1].address, 0x1000003ee);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn failed_stack_trace_response_reports_error() {
        let (tx, rx) = mpsc::channel();
        translate_response(
            &json!({
                "type": "response",
                "command": "stackTrace",
                "success": false,
                "message": "no thread"
            }),
            &tx,
            &pending(),
        );
        match rx.try_recv().unwrap() {
            DebugMessage::Error(message) => assert!(message.contains("no thread")),
            other => panic!("unexpected message: {other:?}"),
        }
    }
}
