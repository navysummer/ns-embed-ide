//! Language Server Protocol (LSP) client for ns-embed-ide.
//!
//! Spawns a language server (`clangd`, `rust-analyzer`, `zls`, `zr-lsp`) over
//! stdio and drives it with JSON-RPC. The client exposes:
//! - a command channel ([`LspCommand`]) for UI-issued requests,
//! - an event channel ([`LspEvent`]) for server notifications (diagnostics).
//!
//! The protocol framing is `Content-Length: N\r\n\r\n<json>` (same as DAP).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Choose the language server executable for a project's language.
pub fn server_for_language(language: &ns_project::Language) -> Option<&'static str> {
    match language {
        ns_project::Language::C | ns_project::Language::Cpp => Some("clangd"),
        ns_project::Language::Rust => Some("rust-analyzer"),
        ns_project::Language::Zig => Some("zls"),
        ns_project::Language::ZR => Some("zr-lsp"),
    }
}

/// Whether the language server is installed (found on PATH or overridden).
pub fn server_available(language: &ns_project::Language) -> bool {
    server_for_language(language)
        .map(find_command)
        .unwrap_or_default()
        .is_some()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LspDiagnostic {
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
    pub severity: LspSeverity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LspSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

impl Default for LspSeverity {
    fn default() -> Self {
        LspSeverity::Information
    }
}

/// A completion item returned by `textDocument/completion`.
#[derive(Debug, Clone, Default)]
pub struct CompletionItem {
    pub label: String,
    pub detail: String,
    pub kind: String,
}

/// A symbol/definition location (`textDocument/definition`).
#[derive(Debug, Clone, Default)]
pub struct Location {
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
}

/// A hover result (`textDocument/hover`).
#[derive(Debug, Clone, Default)]
pub struct Hover {
    pub text: String,
}

/// Commands the UI can issue to the server.
#[derive(Debug, Clone)]
pub enum LspCommand {
    OpenFile { path: PathBuf, text: String },
    ChangeFile { path: PathBuf, text: String, version: i32 },
    Completion {
        path: PathBuf,
        line: usize,
        column: usize,
        reply: mpsc::Sender<Value>,
    },
    Definition {
        path: PathBuf,
        line: usize,
        column: usize,
        reply: mpsc::Sender<Value>,
    },
    Hover {
        path: PathBuf,
        line: usize,
        column: usize,
        reply: mpsc::Sender<Value>,
    },
    References {
        path: PathBuf,
        line: usize,
        column: usize,
        reply: mpsc::Sender<Value>,
    },
    SignatureHelp {
        path: PathBuf,
        line: usize,
        column: usize,
        reply: mpsc::Sender<Value>,
    },
    InlayHints {
        path: PathBuf,
        line: usize,
        column: usize,
        reply: mpsc::Sender<Value>,
    },
    SemanticTokens {
        path: PathBuf,
        reply: mpsc::Sender<Value>,
    },
    Shutdown,
}

/// Notifications the server pushes to the UI.
#[derive(Debug, Clone)]
pub enum LspEvent {
    Diagnostics { path: PathBuf, diagnostics: Vec<LspDiagnostic> },
    ServerExit,
}

/// Handle to a running language server.
pub struct LspServer {
    child: Arc<Mutex<Option<Child>>>,
    commands: Sender<LspCommand>,
    events: Receiver<LspEvent>,
}

impl LspServer {
    /// Spawn the language server for `language` in `root`.
    pub fn spawn(language: &ns_project::Language, root: &Path) -> Result<Self> {
        let program = server_for_language(language)
            .ok_or_else(|| anyhow!("{} 没有对应的语言服务器", language))?;
        let path = find_command(program)
            .ok_or_else(|| anyhow!("未找到语言服务器 {program}（clangd / rust-analyzer / zls / zr-lsp）"))?;

        let mut child = Command::new(&path)
            .arg("--stdio")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| anyhow!("启动语言服务器 {program} 失败: {error}"))?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("无法获取语言服务器 stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("无法获取语言服务器 stdout"))?;

        let (command_tx, command_rx) = mpsc::channel::<LspCommand>();
        let (event_tx, event_rx) = mpsc::channel::<LspEvent>();
        let responses: Arc<Mutex<HashMap<u64, Sender<Value>>>> = Arc::new(Mutex::new(HashMap::new()));
        let next_id = Arc::new(AtomicU64::new(1));
        let shutdown = Arc::new(Mutex::new(false));

        // Reader thread: parse responses/notifications from stdout.
        {
            let responses = responses.clone();
            let event_tx = event_tx.clone();
            thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let message = match read_message(&mut reader) {
                        Ok(message) => message,
                        Err(_) => break,
                    };
                    if let Some(id) = message.get("id").and_then(|v| v.as_u64()) {
                        if let Some(tx) = responses.lock().unwrap().remove(&id) {
                            let _ = tx.send(message.clone());
                        }
                    } else if message.get("method").and_then(|v| v.as_str()) == Some("textDocument/publishDiagnostics") {
                        if let Some(diagnostics) = parse_diagnostics(&message) {
                            let _ = event_tx.send(LspEvent::Diagnostics {
                                path: diagnostics.path,
                                diagnostics: diagnostics.items,
                            });
                        }
                    }
                }
                let _ = event_tx.send(LspEvent::ServerExit);
            });
        }
        // Writer thread: does the initialize handshake first, then processes
        // commands from the UI channel.
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        {
            let responses = responses.clone();
            let next_id = next_id.clone();
            let shutdown = shutdown.clone();
            let root = root.to_path_buf();
            thread::spawn(move || {
                let mut stdin = stdin;
                // Initialize handshake.
                let (init_tx, init_rx) = mpsc::channel();
                responses.lock().unwrap().insert(1, init_tx);
                let init_ok = write_request(
                    &mut stdin,
                    1,
                    "initialize",
                    json!({
                        "processId": std::process::id(),
                        "rootUri": uri(&root),
                        "capabilities": {},
                    }),
                )
                .and_then(|_| {
                    let response = init_rx
                        .recv()
                        .map_err(|_| anyhow!("语言服务器初始化超时"))?;
                    if let Some(error) = response.get("error") {
                        bail!("语言服务器初始化失败: {error}");
                    }
                    Ok(())
                });
                let init_failed = init_ok.is_err();
                let _ = ready_tx.send(init_ok);
                if init_failed {
                    return;
                }
                let _ = notify(
                    &mut stdin,
                    "initialized",
                    json!({}),
                );
                while let Ok(command) = command_rx.recv() {
                    match command {
                        LspCommand::Shutdown => {
                            let _ = write_request(&mut stdin, next_id.fetch_add(1, Ordering::SeqCst), "shutdown", json!({}));
                            *shutdown.lock().unwrap() = true;
                            break;
                        }
                        LspCommand::OpenFile { path, text } => {
                            let _ = notify(
                                &mut stdin,
                                "textDocument/didOpen",
                                json!({
                                    "textDocument": {
                                        "uri": uri(&path),
                                        "languageId": language_id(&path),
                                        "version": 1,
                                        "text": text,
                                    }
                                }),
                            );
                        }
                        LspCommand::ChangeFile { path, text, version } => {
                            let _ = notify(
                                &mut stdin,
                                "textDocument/didChange",
                                json!({
                                    "textDocument": { "uri": uri(&path), "version": version },
                                    "contentChanges": [{ "text": text }],
                                }),
                            );
                        }
                        LspCommand::Completion { path, line, column, reply } => {
                            let _ = request(
                                &mut stdin,
                                &responses,
                                &next_id,
                                reply,
                                "textDocument/completion",
                                json!({
                                    "textDocument": { "uri": uri(&path) },
                                    "position": { "line": line, "character": column },
                                }),
                            );
                        }
                        LspCommand::Definition { path, line, column, reply } => {
                            let _ = request(
                                &mut stdin,
                                &responses,
                                &next_id,
                                reply,
                                "textDocument/definition",
                                json!({
                                    "textDocument": { "uri": uri(&path) },
                                    "position": { "line": line, "character": column },
                                }),
                            );
                        }
                        LspCommand::Hover { path, line, column, reply } => {
                            let _ = request(
                                &mut stdin,
                                &responses,
                                &next_id,
                                reply,
                                "textDocument/hover",
                                json!({
                                    "textDocument": { "uri": uri(&path) },
                                    "position": { "line": line, "character": column },
                                }),
                            );
                        }
                        LspCommand::References { path, line, column, reply } => {
                            let _ = request(
                                &mut stdin,
                                &responses,
                                &next_id,
                                reply,
                                "textDocument/references",
                                json!({
                                    "textDocument": { "uri": uri(&path) },
                                    "position": { "line": line, "character": column },
                                    "context": { "includeDeclaration": true },
                                }),
                            );
                        }
                        LspCommand::SignatureHelp { path, line, column, reply } => {
                            let _ = request(
                                &mut stdin,
                                &responses,
                                &next_id,
                                reply,
                                "textDocument/signatureHelp",
                                json!({
                                    "textDocument": { "uri": uri(&path) },
                                    "position": { "line": line, "character": column },
                                }),
                            );
                        }
                        LspCommand::InlayHints { path, line, column, reply } => {
                            let _ = request(
                                &mut stdin,
                                &responses,
                                &next_id,
                                reply,
                                "textDocument/inlayHint",
                                json!({
                                    "textDocument": { "uri": uri(&path) },
                                    "range": {
                                        "start": { "line": 0, "character": 0 },
                                        "end": { "line": line, "character": column },
                                    },
                                }),
                            );
                        }
                        LspCommand::SemanticTokens { path, reply } => {
                            let _ = request(
                                &mut stdin,
                                &responses,
                                &next_id,
                                reply,
                                "textDocument/semanticTokens/full",
                                json!({
                                    "textDocument": { "uri": uri(&path) },
                                }),
                            );
                        }
                    }
                }
            });
        }
        // Wait for the handshake to complete before returning.
        ready_rx
            .recv()
            .map_err(|_| anyhow!("语言服务器未完成初始化"))??;

        let server = LspServer {
            child: Arc::new(Mutex::new(Some(child))),
            commands: command_tx,
            events: event_rx,
        };
        Ok(server)
    }
    pub fn send(&self, command: LspCommand) -> Result<()> {
        self.commands.send(command).map_err(|_| anyhow!("语言服务器已关闭"))
    }

    pub fn open_file(&self, path: PathBuf, text: String) -> Result<()> {
        self.send(LspCommand::OpenFile { path, text })
    }

    pub fn change_file(&self, path: PathBuf, text: String, version: i32) -> Result<()> {
        self.send(LspCommand::ChangeFile { path, text, version })
    }

    /// Request completion items at a position; returns the raw JSON response.
    pub fn completion(&self, path: PathBuf, line: usize, column: usize) -> Result<Receiver<Value>> {
        let (reply, rx) = mpsc::channel();
        self.send(LspCommand::Completion {
            path,
            line,
            column,
            reply,
        })?;
        Ok(rx)
    }

    /// Request the definition location at a position; returns the raw JSON response.
    pub fn definition(&self, path: PathBuf, line: usize, column: usize) -> Result<Receiver<Value>> {
        let (reply, rx) = mpsc::channel();
        self.send(LspCommand::Definition {
            path,
            line,
            column,
            reply,
        })?;
        Ok(rx)
    }

    /// Request hover at a position; returns the raw JSON response.
    pub fn hover(&self, path: PathBuf, line: usize, column: usize) -> Result<Receiver<Value>> {
        let (reply, rx) = mpsc::channel();
        self.send(LspCommand::Hover {
            path,
            line,
            column,
            reply,
        })?;
        Ok(rx)
    }

    /// Request references at a position; returns the raw JSON response.
    pub fn references(&self, path: PathBuf, line: usize, column: usize) -> Result<Receiver<Value>> {
        let (reply, rx) = mpsc::channel();
        self.send(LspCommand::References {
            path,
            line,
            column,
            reply,
        })?;
        Ok(rx)
    }

    /// Request signature help at a position; returns the raw JSON response.
    pub fn signature_help(&self, path: PathBuf, line: usize, column: usize) -> Result<Receiver<Value>> {
        let (reply, rx) = mpsc::channel();
        self.send(LspCommand::SignatureHelp {
            path,
            line,
            column,
            reply,
        })?;
        Ok(rx)
    }

    /// Request inlay hints in the range up to the given position.
    pub fn inlay_hints(&self, path: PathBuf, line: usize, column: usize) -> Result<Receiver<Value>> {
        let (reply, rx) = mpsc::channel();
        self.send(LspCommand::InlayHints {
            path,
            line,
            column,
            reply,
        })?;
        Ok(rx)
    }

    /// Request full semantic tokens for a document.
    pub fn semantic_tokens(&self, path: PathBuf) -> Result<Receiver<Value>> {
        let (reply, rx) = mpsc::channel();
        self.send(LspCommand::SemanticTokens { path, reply })?;
        Ok(rx)
    }

    pub fn try_recv_event(&self) -> Option<LspEvent> {
        self.events.try_recv().ok()
    }

    pub fn shutdown(&self) {
        let _ = self.commands.send(LspCommand::Shutdown);
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.kill();
            }
        }
    }
}

impl Drop for LspServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn parse_diagnostics(message: &Value) -> Option<ParsedDiagnostics> {
    let params = message.get("params")?;
    let uri = params.get("uri")?.as_str()?;
    let path = uri_to_path(uri)?;
    let items = params
        .get("diagnostics")?
        .as_array()?
        .iter()
        .filter_map(|item| {
            let severity = match item.get("severity").and_then(|v| v.as_i64()).unwrap_or(3) {
                1 => LspSeverity::Error,
                2 => LspSeverity::Warning,
                3 => LspSeverity::Information,
                _ => LspSeverity::Hint,
            };
            let range = item.get("range")?;
            let start = range.get("start")?;
            Some(LspDiagnostic {
                path: path.clone(),
                line: start.get("line")?.as_u64()? as usize + 1,
                column: start.get("character")?.as_u64()? as usize + 1,
                severity,
                message: item.get("message")?.as_str()?.to_string(),
            })
        })
        .collect();
    Some(ParsedDiagnostics { path, items })
}

struct ParsedDiagnostics {
    path: PathBuf,
    items: Vec<LspDiagnostic>,
}

fn request(
    stdin: &mut ChildStdin,
    responses: &Arc<Mutex<HashMap<u64, Sender<Value>>>>,
    next_id: &Arc<AtomicU64>,
    reply: mpsc::Sender<Value>,
    method: &str,
    params: Value,
) -> Result<()> {
    let id = next_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel();
    responses.lock().unwrap().insert(id, tx);
    write_request(stdin, id, method, params)?;
    // Wait for the response and forward it to the UI's reply channel.
    if let Ok(response) = rx.recv() {
        let _ = reply.send(response);
    }
    Ok(())
}

fn write_request(
    stdin: &mut impl Write,
    id: u64,
    method: &str,
    params: Value,
) -> Result<()> {
    let payload = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    write_message(stdin, &payload).map_err(|error| anyhow!("发送 {method} 失败: {error}"))
}

fn notify(stdin: &mut impl Write, method: &str, params: Value) -> Result<()> {
    let payload = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    write_message(stdin, &payload).map_err(|error| anyhow!("发送 {method} 失败: {error}"))
}

fn uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    uri.strip_prefix("file://").map(PathBuf::from)
}

fn language_id(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("c") => "c",
        Some("h") => "c",
        Some("cpp") | Some("cc") | Some("hpp") => "cpp",
        Some("rs") => "rust",
        Some("zig") => "zig",
        _ => "plaintext",
    }
}

pub fn read_message(reader: &mut impl BufRead) -> std::io::Result<Value> {
    let mut content_length = None;
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "language server closed the stream",
            ));
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            match content_length {
                Some(length) => {
                    let mut buffer = vec![0u8; length];
                    reader.read_exact(&mut buffer)?;
                    return serde_json::from_slice(&buffer).map_err(|error| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                    });
                }
                None => continue,
            }
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<usize>().ok();
            }
        }
    }
}

fn write_message(writer: &mut impl Write, payload: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(payload)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

fn find_command(program: &str) -> Option<PathBuf> {
    // 1) Search PATH.
    if let Some(path_var) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path_var) {
            let candidate = directory.join(program);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    // 2) Fall back to the Cargo workspace target dir (dev convenience: the
    //    built-in `zr-lsp` binary lands there without `cargo install`).
    for profile in ["debug", "release"] {
        let candidate = workspace_target_dir().join(profile).join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn workspace_target_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/ns-lsp; the workspace target dir is two up.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("target"))
        .unwrap_or_else(|| PathBuf::from("target"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOCK_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json, os

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        name, _, value = line.partition(b":")
        headers[name.strip().lower()] = value.strip()
    length = int(headers.get(b"content-length", b"0"))
    body = sys.stdin.buffer.read(length)
    return json.loads(body)

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

def reply(id, result):
    write_message({"jsonrpc": "2.0", "id": id, "result": result})

while True:
    msg = read_message()
    if msg is None:
        break
    method = msg.get("method")
    if method == "initialize":
        reply(msg["id"], {"capabilities": {"textDocumentSync": 1}})
    elif method == "initialized":
        pass
    elif method == "textDocument/didOpen":
        params = msg["params"]
        uri = params["textDocument"]["uri"]
        write_message({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "diagnostics": [{
                    "range": {"start": {"line": 2, "character": 4},
                               "end": {"line": 2, "character": 12}},
                    "severity": 1,
                    "message": "test error",
                }],
            },
        })
    elif method == "textDocument/completion":
        reply(msg["id"], {
            "isIncomplete": False,
            "items": [
                {"label": "foo", "kind": 3, "detail": "int foo()"},
                {"label": "bar", "kind": 6, "detail": "int bar"},
            ],
        })
    elif method == "textDocument/definition":
        reply(msg["id"], {
            "uri": "file:///mock/target.c",
            "range": {"start": {"line": 9, "character": 0},
                       "end": {"line": 9, "character": 3}},
        })
    elif method == "textDocument/hover":
        reply(msg["id"], {
            "contents": {"kind": "markdown", "value": "int foo(void)"},
            "range": {"start": {"line": 0, "character": 0},
                       "end": {"line": 0, "character": 3}},
        })
    elif method == "textDocument/references":
        reply(msg["id"], [
            {"uri": "file:///mock/a.c",
             "range": {"start": {"line": 1, "character": 0},
                        "end": {"line": 1, "character": 3}}},
        ])
    elif method == "shutdown":
        reply(msg["id"], None)
    elif method == "exit":
        break
"#;

    struct MockServer {
        dir: tempdir::TempDir,
    }

    impl MockServer {
        fn new() -> Self {
            let dir = tempdir::TempDir::new("ns-lsp-mock").unwrap();
            let bin = dir.path().join("clangd");
            std::fs::write(&bin, MOCK_SERVER).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            MockServer { dir }
        }

        fn with_path(&self, f: impl FnOnce()) {
            let old = std::env::var_os("PATH").unwrap_or_default();
            let mut paths = vec![self.dir.path().to_path_buf()];
            paths.extend(std::env::split_paths(&old));
            // Tests run single-threaded per crate; mutating PATH here is safe.
            unsafe {
                std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
            }
            f();
            unsafe {
                std::env::set_var("PATH", old);
            }
        }
    }

    #[test]
    fn initialize_and_completion() {
        let mock = MockServer::new();
        mock.with_path(|| {
            let root = tempdir::TempDir::new("ns-lsp-root").unwrap();
            let server = LspServer::spawn(&ns_project::Language::C, root.path()).unwrap();

            let rx = server
                .completion(PathBuf::from("/mock/a.c"), 0, 5)
                .unwrap();
            let response = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            let items = response["result"]["items"].as_array().unwrap();
            assert_eq!(items.len(), 2);
            assert_eq!(items[0]["label"], "foo");
            assert_eq!(items[1]["label"], "bar");
        });
    }

    #[test]
    fn definition_returns_location() {
        let mock = MockServer::new();
        mock.with_path(|| {
            let root = tempdir::TempDir::new("ns-lsp-root").unwrap();
            let server = LspServer::spawn(&ns_project::Language::C, root.path()).unwrap();

            let rx = server
                .definition(PathBuf::from("/mock/a.c"), 3, 1)
                .unwrap();
            let response = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            let uri = response["result"]["uri"].as_str().unwrap();
            assert_eq!(uri, "file:///mock/target.c");
            let line = response["result"]["range"]["start"]["line"].as_u64().unwrap();
            assert_eq!(line, 9);
        });
    }

    #[test]
    fn diagnostics_pushed_on_open() {
        let mock = MockServer::new();
        mock.with_path(|| {
            let root = tempdir::TempDir::new("ns-lsp-root").unwrap();
            let server = LspServer::spawn(&ns_project::Language::C, root.path()).unwrap();
            server
                .open_file(PathBuf::from("/mock/a.c"), "int main() { return 0; }".to_string())
                .unwrap();

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                if let Some(LspEvent::Diagnostics { path, diagnostics }) = server.try_recv_event() {
                    assert_eq!(path, PathBuf::from("/mock/a.c"));
                    assert_eq!(diagnostics.len(), 1);
                    assert_eq!(diagnostics[0].message, "test error");
                    assert_eq!(diagnostics[0].severity, LspSeverity::Error);
                    assert_eq!(diagnostics[0].line, 3);
                    assert_eq!(diagnostics[0].column, 5);
                    break;
                }
                if std::time::Instant::now() > deadline {
                    panic!("diagnostic not received in time");
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
    }

    mod tempdir {
        use std::path::{Path, PathBuf};

        pub struct TempDir {
            path: PathBuf,
        }

        impl TempDir {
            pub fn new(prefix: &str) -> std::io::Result<Self> {
                let mut path = std::env::temp_dir();
                let unique = format!(
                    "{}-{}-{}",
                    prefix,
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                );
                path.push(unique);
                std::fs::create_dir_all(&path)?;
                Ok(TempDir { path })
            }

            pub fn path(&self) -> &Path {
                &self.path
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.path);
            }
        }
    }

    /// End-to-end test against a real `clangd`, if one is on PATH.
    #[test]
    fn real_clangd_pushes_diagnostics() {
        if find_command("clangd").is_none() {
            eprintln!("clangd not on PATH; skipping real-server test");
            return;
        }
        let root = tempdir::TempDir::new("ns-lsp-clangd-root").unwrap();
        let source = root.path().join("main.c");
        std::fs::write(&source, "int main() { return undefined_thing; }\n").unwrap();

        let server = LspServer::spawn(&ns_project::Language::C, root.path()).unwrap();
        server
            .open_file(source.clone(), "int main() { return undefined_thing; }\n".to_string())
            .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut got = false;
        while std::time::Instant::now() < deadline {
            if let Some(LspEvent::Diagnostics { path, diagnostics }) = server.try_recv_event() {
                if path == source && !diagnostics.is_empty() {
                    got = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(got, "clangd did not publish diagnostics within 30s");
    }
}
