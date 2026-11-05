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

pub use dap::{read_message, wait_for_response, write_message};

/// One breakpoint to be applied in the debuggee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugBreakpoint {
    /// Absolute path to the source file.
    pub path: String,
    /// 1-based line.
    pub line: usize,
    /// Optional condition expression, for example `count == 10`.
    pub condition: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DebugConfig {
    /// The executable to debug.
    pub program: PathBuf,
    /// Arguments passed to the debuggee.
    pub args: Vec<String>,
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
    Stopped { reason: String, thread_id: u64 },
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
    /// Disassembly instructions starting at the given address.
    Disassembly { instructions: Vec<Instruction> },
    /// Output from the debuggee or the adapter.
    Output { category: String, text: String },
    /// The debuggee terminated, or the adapter exited.
    Terminated,
    /// Anything that went wrong.
    Error(String),
}

/// Commands the UI can send to a running debug session.
#[derive(Debug, Clone)]
pub enum DebugCommand {
    Continue { thread_id: u64 },
    StepOver { thread_id: u64 },
    StepIn { thread_id: u64 },
    StepOut { thread_id: u64 },
    Pause { thread_id: u64 },
    /// Detach and terminate the debuggee.
    Stop,
    GetStackTrace { thread_id: u64 },
    /// Fetch the scopes for a frame; the answer arrives as [`DebugMessage::Scopes`].
    GetScopes { frame_id: u64 },
    /// Fetch memory bytes starting at `reference` (a hex address like `0x1000003E8`).
    ReadMemory { reference: String, count: u64 },
    /// Fetch disassembly instructions starting at `reference` (a hex address).
    Disassemble { reference: String, instruction_count: u64 },
    /// Fetch the variables for a scope or expandable variable; the answer arrives
    /// as [`DebugMessage::Variables`].
    GetVariables { variables_reference: u64 },
    /// Replace all breakpoints of the listed files. Files whose list is empty have
    /// their breakpoints cleared. Files missing from the vector are left untouched.
    ResyncBreakpoints(Vec<(String, Vec<(usize, Option<String>)>)>),
    // Internal: used to shut the command thread down when the adapter exits.
    #[doc(hidden)]
    InternalExit,
}

/// Requests whose responses need to be attributed back to their call site.
#[derive(Debug, Clone, Copy)]
enum PendingRequest {
    Scopes(u64),
    Variables(u64),
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
) -> Vec<(String, Vec<(usize, Option<String>)>)> {
    let mut files: Vec<(String, Vec<(usize, Option<String>)>)> = Vec::new();
    for breakpoint in breakpoints {
        let location = (breakpoint.line, breakpoint.condition.clone());
        match files.iter_mut().find(|(file, _)| file == &breakpoint.path) {
            Some((_, locations)) => locations.push(location),
            None => files.push((breakpoint.path.clone(), vec![location])),
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    for (_, locations) in &mut files {
        locations.sort_by_key(|(line, _)| *line);
        locations.dedup();
    }
    files
}

pub fn spawn(config: &DebugConfig) -> Result<DebugHandle> {
    let adapter = find_dap_adapter().ok_or_else(|| {
        anyhow!("未找到 lldb-dap（LLDB 调试适配器）。请安装 Xcode Command Line Tools，或通过 NS_DEBUG_ADAPTER 环境变量指定适配器路径。")
    })?;
    let mut child = Command::new(&adapter)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| anyhow!("启动调试适配器 {} 失败: {error}", adapter.display()))?;
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("无法获取适配器 stdin"))?;
    let mut stdout = BufReader::new(child.stdout.take().ok_or_else(|| anyhow!("无法获取适配器 stdout"))?);

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
            "cwd": config.cwd.display().to_string(),
            "stopOnEntry": false,
        }),
    )?;
    sequence += 1;

    for (file, locations) in group_breakpoints(&config.breakpoints) {
        if locations.is_empty() {
            continue;
        }
        sequence += 1;
        send_breakpoint_set(&mut stdin, sequence, &file, &locations)?;
    }

    let configuration_seq = sequence + 1;
    request_and_wait(
        &mut stdin,
        &mut stdout,
        configuration_seq,
        "configurationDone",
        json!({}),
    )?;

    // ---- Handshake complete; create the channels and start the pump threads ----
    let (messages, message_rx) = mpsc::channel::<DebugMessage>();
    let (commands, command_rx) = mpsc::channel::<DebugCommand>();
    let _ = messages.send(DebugMessage::Ready);

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
            while let Ok(command) = command_rx.recv() {
                match command {
                    DebugCommand::Continue { thread_id } => {
                        request(&mut stdin, sequence, "continue", json!({ "threadId": thread_id }));
                        sequence += 1;
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
                    DebugCommand::ResyncBreakpoints(files) => {
                        for (file, locations) in files {
                            sequence += 1;
                            if send_breakpoint_set(&mut stdin, sequence, &file, &locations).is_err()
                            {
                                let _ = command_messages
                                    .send(DebugMessage::Error(format!("重新设置断点失败: {file}")));
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
            pump_events(&mut stdout, &mut child, &messages, &reader_commands, &reader_pending);
        })
        .map_err(|error| anyhow!("创建调试消息线程失败: {error}"))?;

    Ok(DebugHandle {
        messages: message_rx,
        commands,
    })
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
    locations: &[(usize, Option<String>)],
) -> std::io::Result<()> {
    let breakpoints: Vec<Value> = locations
        .iter()
        .map(|(line, condition)| {
            let mut breakpoint = json!({ "line": line });
            if let Some(condition) = condition {
                if !condition.trim().is_empty() {
                    breakpoint["condition"] = Value::String(condition.clone());
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
            let _ = messages.send(DebugMessage::Stopped { reason, thread_id });
        }
        "continued" => {
            let _ = messages.send(DebugMessage::Continued);
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
            let reference = match pending_take(pending, request_seq) {
                Some(PendingRequest::Variables(reference)) => reference,
                _ => 0,
            };
            let variables = response["body"]["variables"]
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
                .collect();
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
        }
        "disconnect" => {
            // Termination is reported via the "terminated"/"exited" event.
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
            DebugBreakpoint { path: "src/b.c".into(), line: 3, condition: None },
            DebugBreakpoint { path: "src/a.c".into(), line: 9, condition: Some("x == 1".into()) },
            DebugBreakpoint { path: "src/a.c".into(), line: 4, condition: None },
        ];
        let grouped = group_breakpoints(&breakpoints);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].0, "src/a.c");
        assert_eq!(grouped[0].1, vec![(4, None), (9, Some("x == 1".to_string()))]);
        assert_eq!(grouped[1].0, "src/b.c");
        assert_eq!(grouped[1].1, vec![(3, None)]);
    }

    #[test]
    fn duplicate_breakpoints_are_deduplicated() {
        let breakpoints = vec![
            DebugBreakpoint { path: "src/a.c".into(), line: 4, condition: None },
            DebugBreakpoint { path: "src/a.c".into(), line: 4, condition: None },
        ];
        let grouped = group_breakpoints(&breakpoints);
        assert_eq!(grouped[0].1, vec![(4, None)]);
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
            DebugMessage::Stopped { reason, thread_id } => {
                assert_eq!(reason, "breakpoint");
                assert_eq!(thread_id, 42);
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
