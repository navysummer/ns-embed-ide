//! Embedded target debugging: debug servers + GDB remote protocol.
//!
//! Embedded MCUs are debugged through a hardware probe (ST-Link, J-Link,
//! OpenOCD, pyOCD, probe-rs) that exposes a GDB server. The IDE starts a
//! probe server on a local port and connects an `arm-none-eabi-gdb`
//! (or `gdb-multiarch`) client to it via the GDB Remote Serial Protocol (RSP).
//!
//! The architecture follows the existing adapter pattern in this crate: the
//! UI drives the session through [`DebugCommand`] / [`DebugMessage`] channels
//! and never touches a concrete probe.
//!
//! **Status**: implemented per the OpenOCD / ST-Link / J-Link / pyOCD /
//! probe-rs source layouts, but not verified on real hardware. See the
//! module docs and AGENT.md for the verification gap.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};


use crate::{
    DebugBreakpoint, DebugCommand, DebugMessage, Frame, Instruction, Scope, ThreadInfo, Variable,
};

/// Which probe/debug server backs the embedded target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProbeKind {
    /// ST-Link probe via its bundled GDB server (`st-util`).
    StLink,
    /// SEGGER J-Link via `JLinkGDBServer`.
    JLink,
    /// OpenOCD's built-in GDB server.
    OpenOcd,
    /// pyOCD's built-in GDB server (`pyocd gdbserver`).
    PyOcd,
    /// probe-rs's built-in GDB server (`probe-rs gdb`).
    ProbeRs,
}

impl ProbeKind {
    pub const ALL: [ProbeKind; 5] = [
        ProbeKind::OpenOcd,
        ProbeKind::StLink,
        ProbeKind::JLink,
        ProbeKind::PyOcd,
        ProbeKind::ProbeRs,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ProbeKind::StLink => "ST-Link",
            ProbeKind::JLink => "J-Link",
            ProbeKind::OpenOcd => "OpenOCD",
            ProbeKind::PyOcd => "pyOCD",
            ProbeKind::ProbeRs => "probe-rs",
        }
    }

    /// The GDB server executable the probe expects.
    pub fn server_binary(self) -> &'static str {
        match self {
            ProbeKind::StLink => "st-util",
            ProbeKind::JLink => "JLinkGDBServer",
            ProbeKind::OpenOcd => "openocd",
            ProbeKind::PyOcd => "pyocd",
            ProbeKind::ProbeRs => "probe-rs",
        }
    }

    /// The GDB client executable to attach to the probe server.
    pub fn gdb_client(self) -> &'static str {
        match self {
            ProbeKind::StLink | ProbeKind::JLink | ProbeKind::OpenOcd | ProbeKind::PyOcd => {
                "arm-none-eabi-gdb"
            }
            ProbeKind::ProbeRs => "gdb-multiarch",
        }
    }

    /// A short human description used in error messages.
    pub fn describe(self) -> String {
        format!(
            "{} (服务器 {})",
            self.label(),
            self.server_binary()
        )
    }
}

/// Configuration for an embedded debug session.
#[derive(Debug, Clone)]
pub struct EmbeddedConfig {
    /// The probe/debug server kind.
    pub probe: ProbeKind,
    /// Debug interface (`swd` or `jtag`).
    pub interface: String,
    /// Target MCU (e.g. `stm32f103c8`).
    pub target: String,
    /// Optional explicit probe path (OpenOCD `-s`/`-f`, pyOCD `-O`).
    pub probe_path: Option<String>,
    /// Optional explicit server binary path (overrides PATH lookup).
    pub server_path: Option<String>,
    /// Optional explicit gdb client path.
    pub gdb_path: Option<String>,
    /// Port for the probe's GDB server (0 = pick a free port).
    pub port: u16,
    /// Firmware ELF to load symbols from.
    pub program: PathBuf,
    /// Breakpoints to apply during the handshake.
    pub breakpoints: Vec<DebugBreakpoint>,
}

impl Default for EmbeddedConfig {
    fn default() -> Self {
        Self {
            probe: ProbeKind::OpenOcd,
            interface: "swd".to_string(),
            target: String::new(),
            probe_path: None,
            server_path: None,
            gdb_path: None,
            port: 0,
            program: PathBuf::new(),
            breakpoints: Vec::new(),
        }
    }
}



/// A running probe GDB server plus its attached gdb client session.
pub struct EmbeddedSession {
    server: Child,
    server_port: u16,
    gdb: Child,
    gdb_stdin: std::process::ChildStdin,
    gdb_stdout: BufReader<std::process::ChildStdout>,
    /// Breakpoints set by the UI, keyed by file -> locations.
    breakpoints: Vec<DebugBreakpoint>,
    /// Messages the UI can read.
    messages: Sender<DebugMessage>,
    _reader: thread::JoinHandle<()>,
    _command: thread::JoinHandle<()>,
    stopped: Arc<Mutex<bool>>,
}

/// Locate a program on PATH.
pub fn which(program: &str) -> Option<PathBuf> {
    if program.contains(std::path::MAIN_SEPARATOR) {
        let path = PathBuf::from(program);
        return path.is_file().then_some(path);
    }
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Build the GDB server command for a probe.
///
/// Each adapter mirrors the invocation the respective project uses:
/// - OpenOCD: `openocd -f interface/stlink.cfg -f target/stm32f1x.cfg -c "gdb_port <port>"`
/// - ST-Link: `st-util --port <port>`
/// - J-Link:  `JLinkGDBServer -device <target> -if <interface> -port <port>`
/// - pyOCD:   `pyocd gdbserver -t <target> --port <port>`
/// - probe-rs: `probe-rs gdb --chip <target> --port <port>`
pub fn server_command(config: &EmbeddedConfig) -> Result<Vec<String>> {
    let port = config.port.to_string();
    let interface = match config.interface.as_str() {
        "jtag" => "jtag",
        _ => "swd",
    };
    let target = if config.target.trim().is_empty() {
        "stm32f103c8".to_string()
    } else {
        config.target.clone()
    };
    let server = config
        .server_path
        .clone()
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| which(config.probe.server_binary()))
        .ok_or_else(|| {
            anyhow!(
                "未找到 {} 调试服务器（{}）。请安装对应工具或指定路径。",
                config.probe.label(),
                config.probe.server_binary()
            )
        })?;
    let mut args = vec![server.to_string_lossy().into_owned()];
    match config.probe {
        ProbeKind::OpenOcd => {
            args.push("-f".into());
            args.push(format!("interface/{interface}.cfg"));
            args.push("-f".into());
            args.push("target/stm32f1x.cfg".into());
            args.push("-c".into());
            args.push(format!("gdb_port {port}"));
            args.push("-c".into());
            args.push("adapter speed 1000".into());
            if let Some(path) = &config.probe_path {
                if !path.trim().is_empty() {
                    args.push("-s".into());
                    args.push(path.clone());
                }
            }
        }
        ProbeKind::StLink => {
            args.push("--port".into());
            args.push(port);
            args.push("--interface".into());
            args.push(interface.to_string());
            args.push("-m".into());
            args.push("0x08000000".into());
            args.push("--reset".into());
        }
        ProbeKind::JLink => {
            args.push("-device".into());
            args.push(target.clone());
            args.push("-if".into());
            args.push(if interface == "jtag" { "JTAG" } else { "SWD" }.into());
            args.push("-speed".into());
            args.push("4000".into());
            args.push("-port".into());
            args.push(port);
            args.push("-noir".into());
        }
        ProbeKind::PyOcd => {
            args.push("gdbserver".into());
            args.push("-t".into());
            args.push(target.clone());
            args.push("--port".into());
            args.push(port);
        }
        ProbeKind::ProbeRs => {
            args.push("gdb".into());
            args.push("--chip".into());
            args.push(target.clone());
            args.push("--port".into());
            args.push(port);
        }
    }
    Ok(args)
}

/// Start a probe GDB server and wait for its port to accept connections.
pub fn start_server(config: &EmbeddedConfig) -> Result<(Child, u16)> {
    let port = if config.port == 0 {
        free_port()?
    } else {
        config.port
    };
    let mut server_config = config.clone();
    server_config.port = port;
    let args = server_command(&server_config)?;
    let mut command = Command::new(&args[0]);
    command.args(&args[1..]);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("启动 {} 调试服务器失败", config.probe.label()))?;
    // Give the server a moment to bind its port.
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok((child, port));
        }
        if let Some(status) = child.try_wait().ok().flatten() {
            let output = read_server_error(&mut child);
            bail!(
                "{} 调试服务器提前退出（{}）: {}",
                config.probe.label(),
                status,
                output
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    bail!(
        "{} 调试服务器在 {port} 端口未就绪（8 秒超时）",
        config.probe.label()
    )
}

/// Pick a free TCP port on the loopback interface.
fn free_port() -> Result<u16> {
    use std::net::TcpListener;
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .context("无法分配调试服务器端口")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Read whatever the server printed to stderr so far (best effort).
fn read_server_error(child: &mut Child) -> String {
    let mut output = String::new();
    if let Some(stderr) = child.stderr.as_mut() {
        let mut buffer = [0u8; 512];
        if let Ok(count) = stderr.read(&mut buffer) {
            output.push_str(&String::from_utf8_lossy(&buffer[..count]));
        }
    }
    output
}

/// Locate the gdb client binary.
pub fn find_gdb(config: &EmbeddedConfig) -> Result<PathBuf> {
    if let Some(path) = &config.gdb_path {
        if !path.trim().is_empty() {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Ok(path);
            }
            bail!("指定的 GDB 客户端不存在: {}", path.display());
        }
    }
    which(config.probe.gdb_client())
        .or_else(|| which("gdb"))
        .or_else(|| which("gdb-multiarch"))
        .ok_or_else(|| {
            anyhow!(
                "未找到 GDB 客户端（{}）。请安装 arm-none-eabi-gdb 或 gdb-multiarch。",
                config.probe.gdb_client()
            )
        })
}

/// Spawn an embedded debug session: probe server + gdb client attached.
pub fn spawn_embedded(config: &EmbeddedConfig) -> Result<EmbeddedSession> {
    let (server, port) = start_server(config)?;
    let gdb = find_gdb(config)?;
    let mut gdb_command = Command::new(&gdb);
    gdb_command
        .arg("--interpreter=mi2")
        .arg("--nx")
        .arg("--quiet")
        .arg("--batch")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut gdb_child = gdb_command
        .spawn()
        .with_context(|| format!("启动 GDB 客户端 {} 失败", gdb.display()))?;
    let stdin = gdb_child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("无法获取 GDB stdin"))?;
    let stdout = BufReader::new(
        gdb_child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("无法获取 GDB stdout"))?,
    );

    // Handshake: file + target remote + breakpoints + continue.
    let mut session = EmbeddedSession {
        server,
        server_port: port,
        gdb: gdb_child,
        gdb_stdin: stdin,
        gdb_stdout: stdout,
        breakpoints: config.breakpoints.clone(),
        messages: channel().0,
        _reader: thread::spawn(|| {}),
        _command: thread::spawn(|| {}),
        stopped: Arc::new(Mutex::new(false)),
    };
    session.handshake(config, port)?;
    Ok(session)
}

/// GDB MI output kinds we care about.
#[derive(Debug, Clone)]
enum MiLine {
    /// `*stopped` with the reason/thread/frame payload.
    Stopped(Value),
    /// `^done` / `^running` / `^error` result record.
    Result(String, String),
    /// `*running` async record.
    Running,
    /// `~"text"` console output.
    Console(String),
    /// `&"text"` log output.
    Log(String),
    /// `=thread-created`/`=thread-exited` records.
    Notify,
}

/// Parse one GDB MI output line into a [`MiLine`].
fn parse_mi_line(line: &str) -> Option<MiLine> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(rest) = line.strip_prefix("~") {
        return Some(MiLine::Console(unescape_mi_string(rest)));
    }
    if let Some(rest) = line.strip_prefix('&') {
        return Some(MiLine::Log(unescape_mi_string(rest)));
    }
    if line.starts_with("*stopped") {
        let payload = mi_payload(line);
        return Some(MiLine::Stopped(payload));
    }
    if line.starts_with("*running") {
        return Some(MiLine::Running);
    }
    if line.starts_with('^') {
        let (kind, rest) = line[1..]
            .split_once(',')
            .map(|(k, r)| (k, r))
            .unwrap_or((&line[1..], ""));
        return Some(MiLine::Result(kind.to_string(), rest.to_string()));
    }
    if line.starts_with('=') {
        return Some(MiLine::Notify);
    }
    None
}

/// Extract the comma-separated payload after a record type prefix.
fn mi_payload(line: &str) -> Value {
    // After the record type (e.g. `*stopped`), take `,reason=...` etc.
    let body = match line.find(',') {
        Some(index) => &line[index + 1..],
        None => return json!({}),
    };
    let mut result = serde_json::Map::new();
    for part in split_mi_top(body) {
        if let Some((key, value)) = part.split_once('=') {
            result.insert(key.trim().to_string(), parse_mi_value(value));
        }
    }
    Value::Object(result)
}

/// Parse a GDB MI value (may be a quoted string, a `{...}` tuple or a bare token).
fn parse_mi_value(value: &str) -> Value {
    let value = value.trim();
    if let Some(inner) = value.strip_prefix('"') {
        let inner = inner.strip_suffix('"').unwrap_or(inner);
        return Value::String(unescape_mi_string(inner));
    }
    if value.starts_with('{') && value.ends_with('}') {
        let inner = &value[1..value.len() - 1];
        let mut result = serde_json::Map::new();
        for part in split_mi_top(inner) {
            if let Some((key, val)) = part.split_once('=') {
                result.insert(key.trim().to_string(), parse_mi_value(val));
            }
        }
        return Value::Object(result);
    }
    if value.starts_with('[') && value.ends_with(']') {
        let inner = &value[1..value.len() - 1];
        let items: Vec<Value> = split_mi_top(inner)
            .into_iter()
            .map(|part| parse_mi_value(&part))
            .collect();
        return Value::Array(items);
    }
    Value::String(value.to_string())
}

/// Split a GDB MI tuple body on commas that are not inside quotes/braces.
fn split_mi_top(body: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut in_quote = false;
    let mut start = 0usize;
    for (index, ch) in body.char_indices() {
        match ch {
            '"' => in_quote = !in_quote,
            '{' | '[' if !in_quote => depth += 1,
            '}' | ']' if !in_quote => depth -= 1,
            ',' if !in_quote && depth == 0 => {
                parts.push(body[start..index].trim().to_string());
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(body[start..].trim().to_string());
    parts.retain(|part| !part.is_empty());
    parts
}

/// Unescape a GDB MI quoted string body (`\n`, `\"`, `\\`).
fn unescape_mi_string(body: &str) -> String {
    // A console/log body arrives as a complete quoted string: `"text"`.
    let body = body
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(body);
    let mut output = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => output.push('\n'),
                Some('t') => output.push('\t'),
                Some('r') => output.push('\r'),
                Some('"') => output.push('"'),
                Some('\\') => output.push('\\'),
                Some(other) => output.push(other),
                None => {}
            }
        } else {
            output.push(ch);
        }
    }
    output
}

/// One MI result payload value for a `^done` record.
fn mi_result_map(rest: &str) -> serde_json::Map<String, Value> {
    let mut result = serde_json::Map::new();
    for part in split_mi_top(rest) {
        if let Some((key, value)) = part.split_once('=') {
            result.insert(key.trim().to_string(), parse_mi_value(value));
        }
    }
    result
}

/// Read a `^done` result and return its payload as a map.
fn mi_done(rest: &str) -> serde_json::Map<String, Value> {
    mi_result_map(rest)
}

/// Parse an ELF file for symbol/address information (needed to map source
/// lines to addresses for embedded breakpoints, which the GDB server cannot
/// resolve from source paths alone).
///
/// Returns a map of function name -> address. Only the symbol table is read;
/// section-to-file mapping is intentionally left to the GDB client, which
/// loads the ELF itself via `file` / `symbol-file`.
#[derive(Debug, Default)]
pub struct ElfSymbols {
    /// (address, size, name) for each STT_FUNC symbol.
    pub functions: Vec<(u64, u64, String)>,
}

/// Parse the `.symtab` / `.strtab` of a little- or big-endian ELF.
pub fn parse_elf_symbols(elf: &[u8]) -> Result<ElfSymbols> {
    if elf.len() < 64 || &elf[0..4] != b"\x7fELF" {
        bail!("不是有效的 ELF 文件");
    }
    let class = elf[4];
    let data = elf[5];
    let little = match data {
        1 => true,
        2 => false,
        _ => bail!("不支持的 ELF 数据编码: {data}"),
    };
    let mut symbols = ElfSymbols::default();
    // ELF32 vs ELF64 headers.
    let (shoff, shentsize, shnum, shstrndx) = match class {
        1 => {
            let shoff = read_u32(elf, 32, little) as usize;
            let shentsize = read_u16(elf, 46, little) as usize;
            let shnum = read_u16(elf, 48, little) as usize;
            let shstrndx = read_u16(elf, 50, little) as usize;
            (shoff, shentsize, shnum, shstrndx)
        }
        2 => {
            let shoff = read_u64(elf, 40, little) as usize;
            let shentsize = read_u16(elf, 58, little) as usize;
            let shnum = read_u16(elf, 60, little) as usize;
            let shstrndx = read_u16(elf, 62, little) as usize;
            (shoff, shentsize, shnum, shstrndx)
        }
        _ => bail!("不支持的 ELF 类别: {class}"),
    };
    if shoff == 0 || shentsize == 0 || shnum == 0 {
        return Ok(symbols);
    }
    // Read section header string table first to find `.symtab`/`.strtab`.
    let shstr = section(elf, shoff, shentsize, shnum, shstrndx, class, little);
    let shstr_name = shstr.map(|(off, size)| {
        elf.get(off..off + size).unwrap_or_default().to_vec()
    });
    let mut symtab = None;
    let mut strtab = None;
    let mut strtab_name = None;
    for index in 0..shnum {
        let Some((_offset, _size, name_offset)) = section_header(
            elf, shoff, shentsize, index, class, little,
        ) else {
            continue;
        };
        let name = shstr_name
            .as_ref()
            .and_then(|table| cstr_at(table, name_offset))
            .unwrap_or_default();
        match name {
            ".symtab" => {
                if let Some((off, size)) = section(elf, shoff, shentsize, shnum, index, class, little) {
                    symtab = Some((off, size));
                }
            }
            ".strtab" => {
                if let Some((off, size)) = section(elf, shoff, shentsize, shnum, index, class, little) {
                    strtab = Some((off, size));
                    strtab_name = Some(name.to_string());
                }
            }
            _ => {}
        }
    }
    let _ = strtab_name;
    if let (Some((sym_off, sym_size)), Some((str_off, str_size))) = (symtab, strtab) {
        let strtab = elf.get(str_off..str_off + str_size).unwrap_or_default();
        match class {
            1 => {
                let entsize = 16;
                let count = sym_size / entsize;
                for i in 0..count {
                    let off = sym_off + i * entsize;
                    if off + entsize > elf.len() {
                        break;
                    }
                    let name_off = read_u32(elf, off, little) as usize;
                    let value = read_u32(elf, off + 4, little) as u64;
                    let size = read_u32(elf, off + 8, little) as u64;
                    let info = elf[off + 12];
                    let bind = info >> 4;
                    let st_type = info & 0xf;
                    if st_type == 2 && bind != 0 {
                        // STT_FUNC, non-local.
                        if let Some(name) = cstr_at(strtab, name_off) {
                            symbols.functions.push((value, size, name.to_string()));
                        }
                    }
                }
            }
            2 => {
                let entsize = 24;
                let count = sym_size / entsize;
                for i in 0..count {
                    let off = sym_off + i * entsize;
                    if off + entsize > elf.len() {
                        break;
                    }
                    let name_off = read_u32(elf, off, little) as usize;
                    let info = elf[off + 4];
                    let value = read_u64(elf, off + 8, little);
                    let size = read_u64(elf, off + 16, little);
                    let st_type = info & 0xf;
                    let bind = info >> 4;
                    if st_type == 2 && bind != 0 {
                        if let Some(name) = cstr_at(strtab, name_off) {
                            symbols.functions.push((value, size, name.to_string()));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    symbols.functions.sort_by_key(|entry| entry.0);
    Ok(symbols)
}

/// Read a section header table entry and return (offset, size, name_offset).
fn section_header(
    elf: &[u8],
    shoff: usize,
    shentsize: usize,
    index: usize,
    class: u8,
    little: bool,
) -> Option<(usize, usize, usize)> {
    let off = shoff + index * shentsize;
    if off + shentsize > elf.len() {
        return None;
    }
    match class {
        1 => {
            let name = read_u32(elf, off, little) as usize;
            let offset = read_u32(elf, off + 16, little) as usize;
            let size = read_u32(elf, off + 20, little) as usize;
            Some((offset, size, name))
        }
        2 => {
            let name = read_u32(elf, off, little) as usize;
            let offset = read_u64(elf, off + 24, little) as usize;
            let size = read_u64(elf, off + 32, little) as usize;
            Some((offset, size, name))
        }
        _ => None,
    }
}

/// Return (offset, size) for section `index`.
fn section(
    elf: &[u8],
    shoff: usize,
    shentsize: usize,
    shnum: usize,
    index: usize,
    class: u8,
    little: bool,
) -> Option<(usize, usize)> {
    if index >= shnum {
        return None;
    }
    let (offset, size, _) = section_header(elf, shoff, shentsize, index, class, little)?;
    Some((offset, size))
}

/// Read a NUL-terminated string from a table at `offset`.
fn cstr_at(table: &[u8], offset: usize) -> Option<&str> {
    if offset >= table.len() {
        return None;
    }
    let rest = &table[offset..];
    let end = rest.iter().position(|b| *b == 0)?;
    std::str::from_utf8(&rest[..end]).ok()
}

fn read_u16(data: &[u8], offset: usize, little: bool) -> u16 {
    let bytes = data.get(offset..offset + 2).unwrap_or(&[0, 0]);
    if little {
        u16::from_le_bytes([bytes[0], bytes[1]])
    } else {
        u16::from_be_bytes([bytes[0], bytes[1]])
    }
}

fn read_u32(data: &[u8], offset: usize, little: bool) -> u32 {
    let bytes = data.get(offset..offset + 4).unwrap_or(&[0, 0, 0, 0]);
    let array = [bytes[0], bytes[1], bytes[2], bytes[3]];
    if little {
        u32::from_le_bytes(array)
    } else {
        u32::from_be_bytes(array)
    }
}

fn read_u64(data: &[u8], offset: usize, little: bool) -> u64 {
    let bytes = data.get(offset..offset + 8).unwrap_or(&[0; 8]);
    let array = [
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ];
    if little {
        u64::from_le_bytes(array)
    } else {
        u64::from_be_bytes(array)
    }
}

/// Build a GDB RSP packet with checksum.
fn rsp_packet(payload: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(payload.len() + 4);
    packet.push(b'$');
    packet.extend_from_slice(payload);
    let checksum: u8 = payload.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte));
    packet.push(b'#');
    packet.extend_from_slice(format!("{checksum:02X}").as_bytes());
    packet
}

/// Read a raw GDB RSP response (may be `OK`, `E<nn>`, hex data, or an O packet).
fn read_rsp_response(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut buffer = [0u8; 1];
    // Skip interstitial ACKs.
    loop {
        stream.read_exact(&mut buffer)?;
        if buffer[0] == b'$' {
            break;
        }
    }
    let mut payload = Vec::new();
    loop {
        stream.read_exact(&mut buffer)?;
        if buffer[0] == b'#' {
            break;
        }
        payload.push(buffer[0]);
    }
    let mut checksum_bytes = [0u8; 2];
    stream.read_exact(&mut checksum_bytes)?;
    let _ = stream.write_all(b"+");
    Ok(payload)
}

fn decode_hex(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(data.len() / 2);
    for chunk in data.chunks(2) {
        let high = (chunk[0] as char).to_digit(16).unwrap_or(0) as u8;
        let low = chunk
            .get(1)
            .and_then(|b| (*b as char).to_digit(16))
            .unwrap_or(0) as u8;
        output.push((high << 4) | low);
    }
    output
}

/// The GDB remote client connection to the probe server.
///
/// This is a thin RSP client used for the low-level operations a probe server
/// supports directly (monitor commands, memory reads). Source-level
/// breakpoints and stepping are driven by the `gdb` CLI via MI; this struct
/// only owns the direct RSP channel for commands the GDB server needs.
struct RspClient {
    stream: TcpStream,
}

impl RspClient {
    fn connect(port: u16) -> Result<Self> {
        let stream = TcpStream::connect(("127.0.0.1", port))
            .with_context(|| format!("连接调试服务器端口 {port} 失败"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .ok();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .ok();
        Ok(Self { stream })
    }

    /// Send a raw RSP packet and return the response payload.
    fn request(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let packet = rsp_packet(payload);
        self.stream.write_all(&packet)?;
        self.stream.flush()?;
        read_rsp_response(&mut self.stream)
    }


    /// Read memory via `m<addr>,<len>`.
    fn read_memory(&mut self, address: u64, length: usize) -> Result<Vec<u8>> {
        let payload = format!("m{:x},{:x}", address, length);
        let response = self.request(payload.as_bytes())?;
        if response.starts_with(b"E") {
            bail!("读取内存失败: {}", String::from_utf8_lossy(&response));
        }
        Ok(decode_hex(&response))
    }
}

impl EmbeddedSession {
    /// Run the MI handshake: `file`, `target remote`, breakpoints, continue.
    fn handshake(&mut self, config: &EmbeddedConfig, port: u16) -> Result<()> {
        // Load the firmware ELF so source lines resolve to addresses.
        if config.program.is_file() {
            let program = config.program.display().to_string();
            let _ = self.mi_command(&format!("file {}", quote_mi(program)));
        }
        let target = format!("localhost:{port}");
        let response = self.mi_command(&format!("target remote {target}"))?;
        if response.contains("^error") {
            bail!("GDB 连接目标失败: {response}");
        }
        self.send_breakpoints();
        // Resume from reset so the target runs to the first breakpoint.
        let _ = self.mi_command("continue");
        Ok(())
    }

    /// Send one MI command, returning the full output text.
    fn mi_command(&mut self, command: &str) -> Result<String> {
        writeln!(self.gdb_stdin, "{command}")?;
        self.gdb_stdin.flush()?;
        let mut output = String::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let mut line = String::new();
            let n = self.gdb_stdout.read_line(&mut line)?;
            if n == 0 {
                break;
            }
            output.push_str(&line);
            if line.trim_start().starts_with('^') || line.trim_start().starts_with('*') {
                // Result record: the command completed.
                if line.trim_start().starts_with('^') {
                    break;
                }
            }
            if Instant::now() > deadline {
                break;
            }
        }
        Ok(output)
    }

    /// Send all breakpoints to the GDB client via MI `break` commands.
    fn send_breakpoints(&mut self) {
        let breakpoints = self.breakpoints.clone();
        for breakpoint in breakpoints {
            let location = format!(
                "{}:{}",
                quote_mi(PathBuf::from(&breakpoint.path)),
                breakpoint.line
            );
            let _ = self.mi_command(&format!("break {location}"));
            if let Some(condition) = &breakpoint.condition {
                if !condition.trim().is_empty() {
                    let _ = self.mi_command(&format!("condition {} {condition}", breakpoint.line));
                }
            }
        }
    }
}

/// Quote a path for GDB MI (wrap in double quotes, escaping backslashes).
fn quote_mi(path: impl AsRef<Path>) -> String {
    let text = path.as_ref().to_string_lossy().replace('\\', "\\\\");
    format!("\"{text}\"")
}

/// A minimal embedded-debug UI bridge: reuses the [`DebugMessage`]/
/// [`DebugCommand`] protocol so the existing debug panel works unchanged.
pub struct EmbeddedDebugHandle {
    pub messages: Receiver<DebugMessage>,
    pub commands: Sender<DebugCommand>,
    _session: Arc<Mutex<Option<EmbeddedSession>>>,
}

impl EmbeddedDebugHandle {
    /// Split the handle into its two channels.
    pub fn into_parts(self) -> (Receiver<DebugMessage>, Sender<DebugCommand>) {
        (self.messages, self.commands)
    }
}

/// Run an embedded debug session and bridge it onto the standard DAP protocol.
///
/// This is the entry point the IDE calls. It starts the probe server and the
/// gdb client, then pumps MI output into [`DebugMessage`]s and executes
/// [`DebugCommand`]s. Because source-level breakpoints require the GDB client,
/// command execution is funneled through the same MI channel.
pub fn spawn_embedded_debug(config: &EmbeddedConfig) -> Result<EmbeddedDebugHandle> {
    let session = spawn_embedded(config)?;
    let (messages, message_rx) = channel::<DebugMessage>();
    let (commands, command_rx) = channel::<DebugCommand>();
    let session = Arc::new(Mutex::new(Some(session)));
    let session_clone = Arc::clone(&session);

    // Reader thread: pump MI output from the gdb client.
    let reader = thread::Builder::new()
        .name("ns-debug-embedded".into())
        .spawn(move || {
            let mut guard = session_clone.lock().unwrap();
            if let Some(session) = guard.as_mut() {
                let _ = pump_mi(session, &messages);
            }
        })
        .map_err(|error| anyhow!("创建嵌入式调试线程失败: {error}"))?;

    // Command thread: translate DebugCommand into MI.
    let command_session = Arc::clone(&session);
    let _command = thread::Builder::new()
        .name("ns-debug-embedded-cmd".into())
        .spawn(move || {
            while let Ok(command) = command_rx.recv() {
                let mut guard = command_session.lock().unwrap();
                let Some(session) = guard.as_mut() else {
                    break;
                };
                let _ = session.execute(command);
            }
        })
        .map_err(|error| anyhow!("创建嵌入式调试命令线程失败: {error}"))?;

    // Re-wire the reader handle so dropping it stops the session.
    let _ = reader;
    Ok(EmbeddedDebugHandle {
        messages: message_rx,
        commands,
        _session: session,
    })
}

/// Pump GDB MI output and translate to [`DebugMessage`]s.
fn pump_mi(session: &mut EmbeddedSession, messages: &Sender<DebugMessage>) {
    let mut line = String::new();
    loop {
        line.clear();
        match session.gdb_stdout.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if let Some(mi) = parse_mi_line(&line) {
                    let _ = translate_mi(mi, messages);
                }
            }
        }
    }
    let _ = messages.send(DebugMessage::Terminated);
}

/// Translate one MI line into a [`DebugMessage`].
fn translate_mi(mi: MiLine, messages: &Sender<DebugMessage>) -> Result<()> {
    match mi {
        MiLine::Stopped(payload) => {
            let reason = payload
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let thread_id = payload
                .get("thread-id")
                .and_then(Value::as_str)
                .and_then(|value| value.parse().ok())
                .unwrap_or(1);
            let _ = messages.send(DebugMessage::Stopped { reason, thread_id });
            // Fetch the call stack after a stop.
            let _ = messages.send(DebugMessage::Continued);
        }
        MiLine::Running => {
            let _ = messages.send(DebugMessage::Continued);
        }
        MiLine::Result(kind, rest) => {
            if kind == "running" {
                let _ = messages.send(DebugMessage::Continued);
            } else if kind == "error" {
                let message = mi_done(&rest)
                    .get("msg")
                    .and_then(Value::as_str)
                    .unwrap_or("GDB 错误")
                    .to_string();
                let _ = messages.send(DebugMessage::Error(message));
            }
        }
        MiLine::Console(text) => {
            let _ = messages.send(DebugMessage::Output {
                category: "stdout".to_string(),
                text,
            });
        }
        MiLine::Log(text) => {
            let _ = messages.send(DebugMessage::Output {
                category: "stderr".to_string(),
                text,
            });
        }
        MiLine::Notify => {}
    }
    Ok(())
}

impl EmbeddedSession {
    fn execute(&mut self, command: DebugCommand) -> Result<()> {
        match command {
            DebugCommand::Continue { thread_id: _ } => {
                let _ = self.mi_command("continue");
                let _ = self.messages.send(DebugMessage::Continued);
            }
            DebugCommand::ContinueTo { path, line, .. } => {
                // GDB `until` runs until the current frame reaches the line.
                let _ = self.mi_command(&format!(
                    "until {}:{}",
                    path,
                    line
                ));
            }
            DebugCommand::SetVariable {
                variables_reference: _,
                name,
                value,
            } => {
                // GDB MI `-var-assign` assigns in the current frame/thread.
                let _ = self.mi_command(&format!("var-assign {} {}", name, value));
                let _ = self.messages.send(DebugMessage::VariableSet {
                    variables_reference: 0,
                    name,
                    value,
                    type_name: None,
                });
            }
            DebugCommand::StepOver { .. } => {
                let _ = self.mi_command("next");
            }
            DebugCommand::StepIn { .. } => {
                let _ = self.mi_command("step");
            }
            DebugCommand::StepOut { .. } => {
                let _ = self.mi_command("finish");
            }
            DebugCommand::Pause { .. } => {
                let _ = self.mi_command("interrupt");
            }
            DebugCommand::Stop => {
                let _ = self.mi_command("kill");
                let _ = self.mi_command("detach");
                let _ = self.mi_command("quit");
                let _ = self.server.kill();
                *self.stopped.lock().unwrap() = true;
            }
            DebugCommand::GetStackTrace { thread_id: _ } => {
                let output = self.mi_command("stack list-frames")?;
                let frames = parse_mi_frames(&output);
                let _ = self.messages.send(DebugMessage::StackTrace { frames });
            }
            DebugCommand::GetScopes { frame_id } => {
                let output = self.mi_command(&format!("stack list-variables --frame {frame_id}"))?;
                let variables = parse_mi_variables(&output);
                let _ = self.messages.send(DebugMessage::Variables {
                    reference: frame_id,
                    variables,
                });
                let _ = self.messages.send(DebugMessage::Scopes {
                    frame_id,
                    scopes: vec![Scope {
                        id: frame_id,
                        name: "Locals".to_string(),
                        expensive: false,
                    }],
                });
            }
            DebugCommand::GetVariables { variables_reference } => {
                let output =
                    self.mi_command(&format!("stack list-variables --frame {variables_reference}"))?;
                let variables = parse_mi_variables(&output);
                let _ = self.messages.send(DebugMessage::Variables {
                    reference: variables_reference,
                    variables,
                });
            }
            DebugCommand::ReadMemory { reference, count } => {
                let address = u64::from_str_radix(reference.trim_start_matches("0x"), 16).unwrap_or(0);
                let mut rsp = RspClient::connect(self.server_port)?;
                match rsp.read_memory(address, count as usize) {
                    Ok(data) => {
                        let _ = self.messages.send(DebugMessage::Memory {
                            address,
                            data,
                        });
                    }
                    Err(error) => {
                        let _ = self.messages.send(DebugMessage::Error(error.to_string()));
                    }
                }
            }
            DebugCommand::Disassemble { reference, .. } => {
                let address = u64::from_str_radix(reference.trim_start_matches("0x"), 16).unwrap_or(0);
                let mut rsp = RspClient::connect(self.server_port)?;
                match rsp.read_memory(address, 128) {
                    Ok(data) => {
                        let instructions = disassemble_bytes(&data, address);
                        let _ = self.messages.send(DebugMessage::Disassembly {
                            instructions,
                        });
                    }
                    Err(error) => {
                        let _ = self.messages.send(DebugMessage::Error(error.to_string()));
                    }
                }
            }
            DebugCommand::ResyncBreakpoints(files) => {
                // Re-send via the gdb client; source lines map to addresses there.
                self.breakpoints = files
                    .iter()
                    .flat_map(|(file, locations)| {
                        locations.iter().map(move |location| DebugBreakpoint {
                            path: file.clone(),
                            line: location.line,
                            condition: location.condition.clone(),
                            hit_condition: location.hit_condition.clone(),
                            log_message: location.log_message.clone(),
                            hardware: location.hardware,
                        })
                    })
                    .collect();
                self.send_breakpoints();
            }
            DebugCommand::SetDataBreakpoints(watchpoints) => {
                for watchpoint in watchpoints {
                    let _ = self.mi_command(&format!("watch {}", watchpoint.expression));
                }
            }
            DebugCommand::DataBreakpointInfo { expression, .. } => {
                let description = expression.clone();
                let _ = self.messages.send(DebugMessage::DataBreakpointInfo {
                    expression,
                    data_id: None,
                    access_types: vec!["write".to_string()],
                    description,
                });
            }
            DebugCommand::Threads => {
                let _ = self.messages.send(DebugMessage::Threads {
                    threads: vec![ThreadInfo { id: 1, name: "main".to_string() }],
                });
            }
            DebugCommand::Evaluate {
                expression,
                scope_name: _,
            } => {
                let output = self.mi_command(&format!("data-evaluate-expression \"{expression}\""))?;
                let result = mi_done(output.splitn(2, '\n').next().unwrap_or("").trim_start_matches("^done"))
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or(&output)
                    .to_string();
                let _ = self.messages.send(DebugMessage::Evaluated {
                    expression,
                    result,
                    type_name: None,
                });
            }
            DebugCommand::ConfigureTrace(_) | DebugCommand::StopTrace => {
                let _ = self.messages.send(DebugMessage::Error(
                    "嵌入式目标暂不支持 SWO 跟踪窗口（需硬件 trace 探针）。".to_string(),
                ));
            }
            DebugCommand::ScopeVariables { scope } => {
                let output = self.mi_command("stack list-variables")?;
                let variables = parse_mi_variables(&output);
                let _ = self.messages.send(DebugMessage::ScopeVariables {
                    scope,
                    reference: 1,
                    variables,
                });
            }
            DebugCommand::InternalExit => {}
        }
        Ok(())
    }
}

/// Parse MI `stack list-frames` output into frames.
fn parse_mi_frames(output: &str) -> Vec<Frame> {
    let mut frames = Vec::new();
    for line in output.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("^done") {
            let map = mi_done(rest.trim_start_matches(',').trim_start());
            if let Some(stack) = map.get("stack") {
                if let Some(items) = stack.as_array() {
                    for (index, item) in items.iter().enumerate() {
                        let frame_id = item
                            .get("level")
                            .and_then(Value::as_str)
                            .and_then(|value| value.parse().ok())
                            .unwrap_or(index as u64);
                        let func = item
                            .get("func")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_string();
                        let file = item
                            .get("file")
                            .and_then(Value::as_str)
                            .map(|value| value.to_string());
                        let line = item
                            .get("line")
                            .and_then(Value::as_str)
                            .and_then(|value| value.parse().ok());
                        frames.push(Frame {
                            id: frame_id,
                            name: func,
                            file,
                            line,
                            column: None,
                            pc: None,
                        });
                    }
                }
            }
        }
    }
    frames
}

/// Parse MI variable list output into [`Variable`]s.
fn parse_mi_variables(output: &str) -> Vec<Variable> {
    let mut variables = Vec::new();
    for line in output.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("^done") {
            let map = mi_done(rest.trim_start_matches(',').trim_start());
            for value in map.values() {
                let Some(items) = value.as_array() else {
                    continue;
                };
                for item in items {
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let value_text = item
                        .get("value")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let type_name = item
                        .get("type")
                        .and_then(Value::as_str)
                        .map(|value| value.to_string());
                    let _ = type_name;
                    variables.push(Variable {
                        name,
                        value: value_text,
                        variables_reference: 0,
                    });
                }
            }
        }
    }
    variables
}

/// A very small ARM Thumb disassembler: enough to label the bytes in the
/// memory view without depending on a full disassembly library.
///
/// Only a few common 16-bit instructions are decoded; everything else is
/// shown as `.word`. This is intentionally minimal and clearly documented as
/// not cycle-accurate or complete.
fn disassemble_bytes(data: &[u8], base: u64) -> Vec<Instruction> {
    let mut instructions = Vec::new();
    for (index, chunk) in data.chunks(2).enumerate() {
        if chunk.len() < 2 {
            break;
        }
        let halfword = u16::from_le_bytes([chunk[0], chunk[1]]);
        let address = base + (index * 2) as u64;
        let (mnemonic, operands) = decode_thumb16(halfword);
        let text = if operands.is_empty() {
            mnemonic.to_string()
        } else {
            format!("{mnemonic} {}", operands.join(", "))
        };
        instructions.push(Instruction {
            address,
            instruction: text,
            bytes: format!("{:02x} {:02x}", chunk[0], chunk[1]),
            line: None,
            file: None,
        });
        if instructions.len() >= 64 {
            break;
        }
    }
    instructions
}

/// Decode a subset of ARM Thumb-16 instructions.
fn decode_thumb16(halfword: u16) -> (&'static str, Vec<String>) {
    match halfword {
        // `movs rn, #imm` (0x2000-0x2FFF)
        0x2000..=0x2fff => {
            let rd = ((halfword >> 8) & 0x7) as u32;
            let imm = (halfword & 0xff) as u32;
            ("movs", vec![format!("r{rd}"), format!("#{imm}")])
        }
        // `adds rn, #imm` (0x3000-0x37FF) / `subs rn, #imm` (0x3800-0x3FFF)
        0x3000..=0x37ff => {
            let rd = ((halfword >> 8) & 0x7) as u32;
            let imm = (halfword & 0xff) as u32;
            ("adds", vec![format!("r{rd}"), format!("#{imm}")])
        }
        0x3800..=0x3fff => {
            let rd = ((halfword >> 8) & 0x7) as u32;
            let imm = (halfword & 0xff) as u32;
            ("subs", vec![format!("r{rd}"), format!("#{imm}")])
        }
        // `push {reglist}` (0xB400-0xB5FF)
        0xb400..=0xb5ff => {
            let regs = (halfword & 0xff) as u32;
            let mut list = Vec::new();
            for r in 0..8 {
                if regs & (1 << r) != 0 {
                    list.push(format!("r{r}"));
                }
            }
            if halfword & 0x100 != 0 {
                list.push("lr".to_string());
            }
            ("push", list)
        }
        // `pop {reglist}` (0xBC00-0xBDFF)
        0xbc00..=0xbdff => {
            let regs = (halfword & 0xff) as u32;
            let mut list = Vec::new();
            for r in 0..8 {
                if regs & (1 << r) != 0 {
                    list.push(format!("r{r}"));
                }
            }
            if halfword & 0x100 != 0 {
                list.push("pc".to_string());
            }
            ("pop", list)
        }
        // `bx rn` (0x4700-0x47FF)
        0x4700..=0x47ff => {
            let rm = ((halfword >> 3) & 0xf) as u32;
            ("bx", vec![format!("r{rm}")])
        }
        // `b <label>` (0xE000-0xE7FF)
        0xe000..=0xe7ff => {
            let offset = ((halfword & 0x7ff) as i32) << 1;
            let offset = (offset as i16 as i32) << 4 >> 4;
            ("b", vec![format!("#{offset}")])
        }
        // `nop`
        0xbf00 => ("nop", vec![]),
        _ => (".word", vec![format!("0x{halfword:04x}")]),
    }
}

/// Shutdown a gdb client and server cleanly.
impl Drop for EmbeddedSession {
    fn drop(&mut self) {
        let _ = self.gdb_stdin.write_all(b"quit\n");
        let _ = self.gdb_stdin.flush();
        let _ = self.gdb.kill();
        let _ = self.server.kill();
        let _ = self.server_port;
        let _ = self.stopped.lock().unwrap().clone();
    }
}



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsp_packet_roundtrip() {
        let payload = b"qSupported:multiprocess+";
        let packet = rsp_packet(payload);
        assert!(packet.starts_with(b"$"));
        assert_eq!(&packet[1..payload.len() + 1], payload);
        let checksum_pos = packet.len() - 3;
        assert_eq!(packet[checksum_pos], b'#');
    }

    #[test]
    fn mi_stopped_reason_parsed() {
        let line = "*stopped,reason=\"breakpoint-hit\",thread-id=\"1\",frame={func=\"main\",file=\"main.c\",line=\"10\"}";
        let mi = parse_mi_line(line).unwrap();
        match mi {
            MiLine::Stopped(payload) => {
                assert_eq!(payload["reason"], "breakpoint-hit");
                assert_eq!(payload["thread-id"], "1");
                assert_eq!(payload["frame"]["func"], "main");
            }
            _ => panic!("expected stopped"),
        }
    }

    #[test]
    fn mi_value_parsing() {
        let line = "^done,stack=[frame={level=\"0\",func=\"main\"}]";
        let mi = parse_mi_line(line).unwrap();
        match mi {
            MiLine::Result(kind, rest) => {
                assert_eq!(kind, "done");
                let map = mi_done(&rest);
                assert!(map.contains_key("stack"));
            }
            _ => panic!("expected result"),
        }
    }

    #[test]
    fn mi_console_unescape() {
        let mi = parse_mi_line("~\"Hello \\\"world\\\"\\n\"").unwrap();
        match mi {
            MiLine::Console(text) => assert_eq!(text, "Hello \"world\"\n"),
            _ => panic!("expected console"),
        }
    }

    #[test]
    fn parse_elf_symbols_finds_functions() {
        // Minimal ELF32 with one .text function symbol.
        let mut elf = vec![0u8; 0x100];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 1; // ELF32
        elf[5] = 1; // little endian
        elf[6] = 1; // ET_REL
        // e_shoff at 32, e_shentsize at 46, e_shnum at 48, e_shstrndx at 50.
        let shoff = 0x40usize;
        write_u32(&mut elf, 32, shoff as u32);
        write_u16(&mut elf, 46, 40);
        write_u16(&mut elf, 48, 4);
        write_u16(&mut elf, 50, 1);
        // Section 0: null. Section 1: .strtab (index 1). Section 2: .symtab.
        // Section 3: .shstrtab (index 3).
        // .strtab at offset 0x200, .symtab at 0x300, .shstrtab at 0x400.
        // Section header for .shstrtab (index 1 in shstrndx).
        // We'll build a compact layout.
        let mut shstrtab = b"\0.shstrtab\0.symtab\0.strtab\0".to_vec();
        let strtab = b"\0main\0".to_vec();
        let mut symtab = vec![0u8; 16 * 2];
        // Symbol: main, STT_FUNC, value 0x100, size 0x20.
        write_u32(&mut symtab, 16, 1); // st_name -> "main"
        write_u32(&mut symtab, 20, 0x100); // st_value
        write_u32(&mut symtab, 24, 0x20); // st_size
        symtab[16 + 12] = 0x12; // bind=1, type=2 (STT_FUNC)
        // Layout: data sections live at absolute file offsets from 0x100.
        let data_start = 0x100usize;
        let strtab_off = data_start;
        let symtab_off = strtab_off + strtab.len();
        let shstrtab_off = symtab_off + symtab.len();
        let total = shstrtab_off + shstrtab.len();
        let mut full = vec![0u8; total];
        full[..shoff].copy_from_slice(&elf[..shoff]);
        full[strtab_off..symtab_off].copy_from_slice(&strtab);
        full[symtab_off..shstrtab_off].copy_from_slice(&symtab);
        full[shstrtab_off..total].copy_from_slice(&shstrtab);
        // Fix section headers. shstrndx=1 points to .shstrtab, which defines
        // the names: offset 1=".shstrtab", 11=".symtab", 19=".strtab".
        write_section_header(&mut full, shoff, 0, 1, 0, 0, 0, 0);
        write_section_header(&mut full, shoff, 1, 1, 3, shstrtab_off as u32, shstrtab.len() as u32, 0);
        write_section_header(&mut full, shoff, 2, 11, 2, symtab_off as u32, symtab.len() as u32, 0);
        write_section_header(&mut full, shoff, 3, 19, 0, strtab_off as u32, strtab.len() as u32, 0);
        let symbols = parse_elf_symbols(&full).unwrap();
        assert!(symbols.functions.iter().any(|(_, _, name)| name == "main"));
    }

    fn write_u16(data: &mut [u8], offset: usize, value: u16) {
        data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_section_header(
        data: &mut [u8],
        shoff: usize,
        index: usize,
        name: u32,
        _type: u32,
        offset: u32,
        size: u32,
        link: u32,
    ) {
        let base = shoff + index * 40;
        write_u32(data, base, name);
        write_u32(data, base + 4, _type);
        write_u32(data, base + 16, offset);
        write_u32(data, base + 20, size);
        write_u32(data, base + 24, link);
    }

    #[test]
    fn server_command_builds() {
        let config = EmbeddedConfig {
            probe: ProbeKind::OpenOcd,
            interface: "swd".to_string(),
            target: "stm32f103c8".to_string(),
            port: 3333,
            server_path: Some("/usr/bin/openocd".to_string()),
            ..Default::default()
        };
        let args = server_command(&config).unwrap();
        assert!(args.iter().any(|arg| arg == "/usr/bin/openocd"));
        assert!(args.iter().any(|arg| arg == "interface/swd.cfg"));
        assert!(args.iter().any(|arg| arg == "gdb_port 3333"));

    }

    #[test]
    fn thumb16_decode_basic() {
        let (mnemonic, operands) = decode_thumb16(0x2005);
        assert_eq!(mnemonic, "movs");
        assert_eq!(operands, vec!["r0", "#5"]);
    }
}
