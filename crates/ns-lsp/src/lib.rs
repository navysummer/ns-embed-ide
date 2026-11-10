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
        ns_project::Language::ZR => Some("zr"),
        // Assembly has no language server; clangd handles C/C++ only.
        ns_project::Language::Asm => None,
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
    /// Text to insert when the item is confirmed. Differs from `label` when
    /// the server supplies `insertText` or a `textEdit` with a replacement
    /// range; `None` means "insert `label` as-is".
    pub insert_text: Option<String>,
    /// Whether `insert_text` is a snippet (LSP `insertTextFormat`: 2) whose
    /// placeholders (`${1:...}`, `$0`, `$TM_FILENAME`) must be expanded
    /// before insertion. `false` = plain text.
    pub snippet: bool,
    /// Documentation for this item (LSP `documentation`), either a plain
    /// string or a `MarkupContent` object with a `value`. Empty when the
    /// server supplies none; shown as a preview under the completion list.
    pub documentation: String,
}
/// A document symbol (`textDocument/documentSymbol`), used for the outline and
/// breadcrumb navigation.
#[derive(Debug, Clone, Default)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: String,
    /// 0-based start line of the symbol.
    pub line: usize,
    /// 0-based start column.
    pub column: usize,
    /// Nested symbols (children of a namespace/class/function body).
    pub children: Vec<SymbolInfo>,
}

/// A symbol/definition location (`textDocument/definition`).
#[derive(Debug, Clone, Default)]
pub struct Location {
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
}

/// A single text edit (`TextEdit`) from a `WorkspaceEdit`: a range in a file
/// plus the replacement text. Lines/characters are 0-based (LSP convention).
#[derive(Debug, Clone, Default)]
pub struct TextEdit {
    pub path: PathBuf,
    pub start_line: usize,
    pub start_char: usize,
    pub end_line: usize,
    pub end_char: usize,
    pub new_text: String,
}

/// Parse a `textDocument/rename` response into file edits. Handles the
/// `changes: { uri: [TextEdit] }` shape (single-document changes map).
pub fn parse_workspace_edit(value: &Value) -> Vec<TextEdit> {
    let changes = value.get("changes").and_then(|v| v.as_object());
    let mut edits = Vec::new();
    if let Some(changes) = changes {
        for (uri, items) in changes {
            let Some(path) = uri_to_path(uri) else {
                continue;
            };
            for item in items.as_array().into_iter().flatten() {
                if let Some(edit) = parse_text_edit(&path, item) {
                    edits.push(edit);
                }
            }
        }
    }
    edits
}

/// Parse a `textDocument/formatting` response, which is a bare `TextEdit[]`
/// array (unlike rename's `WorkspaceEdit`). Each item carries a 0-based range
/// and replacement text; all apply to the requested document.
pub fn parse_format_edits(value: &Value, path: &Path) -> Vec<TextEdit> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| parse_text_edit(path, item))
        .collect()
}

/// A parsed code action (`textDocument/codeAction` item): a title plus the
/// edits it applies (from `edit.changes`, a `WorkspaceEdit`).
#[derive(Debug, Clone, Default)]
pub struct CodeAction {
    pub title: String,
    pub edits: Vec<TextEdit>,
}

/// Parse a `textDocument/codeAction` response into actions. Items may carry a
/// `command` instead of `edit`; those are skipped (no client-side handler).
pub fn parse_code_actions(value: &Value) -> Vec<CodeAction> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.to_string();
            let edits = item
                .get("edit")
                .map(parse_workspace_edit)
                .unwrap_or_default();
            Some(CodeAction { title, edits })
        })
        .collect()
}

fn parse_text_edit(path: &Path, item: &Value) -> Option<TextEdit> {
    let range = item.get("range")?;
    let start = range.get("start")?;
    let end = range.get("end")?;
    Some(TextEdit {
        path: path.to_path_buf(),
        start_line: start.get("line")?.as_u64()? as usize,
        start_char: start.get("character")?.as_u64()? as usize,
        end_line: end.get("line")?.as_u64()? as usize,
        end_char: end.get("character")?.as_u64()? as usize,
        new_text: item.get("newText")?.as_str()?.to_string(),
    })
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
    Rename {
        path: PathBuf,
        line: usize,
        column: usize,
        new_name: String,
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
    DocumentSymbols {
        path: PathBuf,
        reply: mpsc::Sender<Value>,
    },
    WorkspaceSymbols {
        query: String,
        reply: mpsc::Sender<Value>,
    },
    Formatting {
        path: PathBuf,
        reply: mpsc::Sender<Value>,
    },
    CodeAction {
        path: PathBuf,
        line: usize,
        column: usize,
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

/// Transport args for a server. `clangd` rejects the explicit `--stdio` flag
/// (Apple's build exits immediately) but defaults to stdio when given no
/// transport arg, so it is launched bare. The others accept `--stdio`.
fn transport_args(program: &str) -> &'static [&'static str] {
    match program {
        "clangd" | "zr" => &[],
        _ => &["--stdio"],
    }
}

/// How long to wait for the LSP `initialize` handshake before giving up.
const INIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

impl LspServer {
    /// Spawn the language server for `language` in `root`.
    pub fn spawn(language: &ns_project::Language, root: &Path) -> Result<Self> {
        let program = server_for_language(language)
            .ok_or_else(|| anyhow!("{} 没有对应的语言服务器", language))?;
        let path = find_command(program)
            .ok_or_else(|| anyhow!("未找到语言服务器 {program}（clangd / rust-analyzer / zls / zr）"))?;
        Self::spawn_with_path(program, &path, root)
    }

    /// Spawn the language server at an explicit executable `path`. This is the
    /// single entry point for both [`Self::spawn`] (which resolves the program
    /// on PATH) and tests that use a mock server binary, so a mock never has
    /// to mutate the process-global PATH.
    pub(crate) fn spawn_with_path(
        program: &str,
        path: &Path,
        root: &Path,
    ) -> Result<Self> {
        let mut command = Command::new(path);
        // `zr lsp` is the LSP subcommand of the zr compiler; the others are
        // standalone LSP servers.
        if program == "zr" {
            command.arg("lsp");
        }
        command.args(transport_args(program));
        let mut child = command
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
        // Set when the server process's stdout closes (child died or exited),
        // so a blocked handshake fails fast instead of waiting for a reply
        // that will never arrive.
        let server_exited = Arc::new(std::sync::atomic::AtomicBool::new(false));

        // Reader thread: parse responses/notifications from stdout.
        {
            let responses = responses.clone();
            let event_tx = event_tx.clone();
            let server_exited = server_exited.clone();
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
                server_exited.store(true, std::sync::atomic::Ordering::SeqCst);
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
            let server_exited = server_exited.clone();
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
                    // The child may have died (e.g. it rejects our launch
                    // args). Wait for the response, but bail as soon as the
                    // reader observes stdout close so we never block forever.
                    loop {
                        match init_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                            Ok(response) => {
                                if let Some(error) = response.get("error") {
                                    bail!("语言服务器初始化失败: {error}");
                                }
                                return Ok(());
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                if server_exited.load(std::sync::atomic::Ordering::SeqCst) {
                                    bail!("语言服务器进程已退出（可能不支持当前启动参数）");
                                }
                            }
                            Err(mpsc::RecvTimeoutError::Disconnected) => {
                                bail!("语言服务器初始化中断");
                            }
                        }
                    }
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
                        LspCommand::Rename { path, line, column, new_name, reply } => {
                            let _ = request(
                                &mut stdin,
                                &responses,
                                &next_id,
                                reply,
                                "textDocument/rename",
                                json!({
                                    "textDocument": { "uri": uri(&path) },
                                    "position": { "line": line, "character": column },
                                    "newName": new_name,
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
                        LspCommand::DocumentSymbols { path, reply } => {
                            let _ = request(
                                &mut stdin,
                                &responses,
                                &next_id,
                                reply,
                                "textDocument/documentSymbol",
                                json!({
                                    "textDocument": { "uri": uri(&path) },
                                }),
                            );
                        }
                        LspCommand::WorkspaceSymbols { query, reply } => {
                            let _ = request(
                                &mut stdin,
                                &responses,
                                &next_id,
                                reply,
                                "workspace/symbol",
                                json!({ "query": query }),
                            );
                        }
                        LspCommand::Formatting { path, reply } => {
                            let _ = request(
                                &mut stdin,
                                &responses,
                                &next_id,
                                reply,
                                "textDocument/formatting",
                                json!({
                                    "textDocument": { "uri": uri(&path) },
                                    "options": {
                                        "tabSize": 4,
                                        "insertSpaces": true,
                                    },
                                }),
                            );
                        }
                        LspCommand::CodeAction { path, line, column, reply } => {
                            let _ = request(
                                &mut stdin,
                                &responses,
                                &next_id,
                                reply,
                                "textDocument/codeAction",
                                json!({
                                    "textDocument": { "uri": uri(&path) },
                                    "range": {
                                        "start": { "line": line, "character": column },
                                        "end": { "line": line, "character": column },
                                    },
                                    "context": { "diagnostics": [] },
                                }),
                            );
                        }
                    }
                }
            });
        }
        // Wait for the handshake to complete before returning, but never block
        // the UI thread forever: a server that starts slowly or never responds
        // (e.g. clangd cold-starting, rust-analyzer indexing) must not freeze
        // the app. On timeout the child is killed and an error returned.
        let init_result = ready_rx
            .recv_timeout(INIT_TIMEOUT)
            .map_err(|_| anyhow!("语言服务器初始化超时（{INIT_TIMEOUT:?}）"))
            .and_then(|result| result);
        if let Err(error) = init_result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }

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

    /// Request a symbol rename at a position; returns the raw JSON response.
    pub fn rename(
        &self,
        path: PathBuf,
        line: usize,
        column: usize,
        new_name: String,
    ) -> Result<Receiver<Value>> {
        let (reply, rx) = mpsc::channel();
        self.send(LspCommand::Rename {
            path,
            line,
            column,
            new_name,
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

    /// Request the document outline (symbols in declaration order).
    pub fn document_symbols(&self, path: PathBuf) -> Result<Receiver<Value>> {
        let (reply, rx) = mpsc::channel();
        self.send(LspCommand::DocumentSymbols { path, reply })?;
        Ok(rx)
    }

    /// Request a workspace symbol search (`workspace/symbol`); the response is
    /// a `SymbolInformation[]` flat list.
    pub fn workspace_symbols(&self, query: String) -> Result<Receiver<Value>> {
        let (reply, rx) = mpsc::channel();
        self.send(LspCommand::WorkspaceSymbols { query, reply })?;
        Ok(rx)
    }

    /// Request document formatting; the response is a `TextEdit[]` array.

    pub fn formatting(&self, path: PathBuf) -> Result<Receiver<Value>> {
        let (reply, rx) = mpsc::channel();
        self.send(LspCommand::Formatting { path, reply })?;
        Ok(rx)
    }

    /// Request code actions at a position; the response is a `CodeAction[]`.
    pub fn code_action(
        &self,
        path: PathBuf,
        line: usize,
        column: usize,
    ) -> Result<Receiver<Value>> {
        let (reply, rx) = mpsc::channel();
        self.send(LspCommand::CodeAction {
            path,
            line,
            column,
            reply,
        })?;
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

/// Parse a `textDocument/documentSymbol` response into a flat, ordered outline.
///
/// The response is either a `DocumentSymbol[]` (each with `range`, `name`,
/// `kind` and optional `children`) or a `SymbolInformation[]` (each with
/// `location.range` and no nesting). Both shapes are accepted.
pub fn parse_symbols(value: &Value) -> Vec<SymbolInfo> {
    fn kind_name(kind: Option<u64>) -> String {
        // LSP SymbolKind enum: 1=File, 2=Module, 3=Namespace, 4=Package,
        // 5=Class, 6=Method, 7=Property, 8=Field, 9=Constructor, 10=Enum,
        // 11=Interface, 12=Function, 13=Variable, 14=Constant, 22=EnumMember,
        // 23=Struct, 24=Event, 26=TypeParameter.
        match kind {
            Some(2) => "module",
            Some(3) => "namespace",
            Some(4) => "package",
            Some(5) => "class",
            Some(6) => "method",
            Some(7) => "property",
            Some(8) => "field",
            Some(9) => "constructor",
            Some(10) => "enum",
            Some(11) => "interface",
            Some(12) => "function",
            Some(13) => "variable",
            Some(14) => "constant",
            Some(22) => "enum_member",
            Some(23) => "struct",
            Some(24) => "event",
            Some(26) => "type_parameter",
            _ => "symbol",
        }
        .to_string()
    }

    fn parse_one(item: &Value) -> Option<SymbolInfo> {
        let name = item.get("name")?.as_str()?.to_string();
        // `DocumentSymbol` uses `range`; `SymbolInformation` uses `location.range`.
        let range = item
            .get("range")
            .or_else(|| item.get("location")?.get("range"));
        let start = range?.get("start")?;
        let children = item
            .get("children")
            .and_then(Value::as_array)
            .map(|children| {
                children
                    .iter()
                    .filter_map(parse_one)
                    .collect()
            })
            .unwrap_or_default();
        Some(SymbolInfo {
            name,
            kind: kind_name(item.get("kind").and_then(Value::as_u64)),
            line: start.get("line").and_then(Value::as_u64).unwrap_or(0) as usize,
            column: start.get("character").and_then(Value::as_u64).unwrap_or(0) as usize,
            children,
        })
    }

    // Accept both a bare `DocumentSymbol[]` (as unit tests build) and a
    // JSON-RPC response envelope `{"result": [...]}` (as the server returns).
    let items: &[Value] = value
        .get("result")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_else(|| value.as_array().map(Vec::as_slice).unwrap_or(&[]));
    items.iter().filter_map(parse_one).collect()
}

/// A workspace symbol (`workspace/symbol`), for cross-file symbol navigation.
/// Unlike [`SymbolInfo`], a workspace symbol carries its own file location.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceSymbolInfo {
    pub name: String,
    pub kind: String,
    /// Path of the file containing the symbol (absolute, from the URI).
    pub path: PathBuf,
    /// 0-based start line.
    pub line: usize,
    /// 0-based start column.
    pub column: usize,
}

/// Parse a `workspace/symbol` response (`SymbolInformation[]`) into flat
/// workspace symbols. Each entry has `name`, `kind`, and `location.range`.
pub fn parse_workspace_symbols(value: &Value) -> Vec<WorkspaceSymbolInfo> {
    let items: &[Value] = value
        .get("result")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_else(|| value.as_array().map(Vec::as_slice).unwrap_or(&[]));
    items
        .iter()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?.to_string();
            let location = item.get("location")?;
            let path = uri_to_path(location.get("uri")?.as_str()?)?;
            let range = location.get("range")?;
            let start = range.get("start")?;
            Some(WorkspaceSymbolInfo {
                name,
                kind: symbol_kind_name(item.get("kind").and_then(Value::as_u64)),
                path,
                line: start.get("line").and_then(Value::as_u64).unwrap_or(0) as usize,
                column: start.get("character").and_then(Value::as_u64).unwrap_or(0) as usize,
            })
        })
        .collect()
}

/// Map an LSP `SymbolKind` integer to a stable lowercase name.
fn symbol_kind_name(kind: Option<u64>) -> String {
    match kind {
        Some(1) => "file",
        Some(2) => "module",
        Some(3) => "namespace",
        Some(4) => "package",
        Some(5) => "class",
        Some(6) => "method",
        Some(7) => "property",
        Some(8) => "field",
        Some(9) => "constructor",
        Some(10) => "enum",
        Some(11) => "interface",
        Some(12) => "function",
        Some(13) => "variable",
        Some(14) => "constant",
        Some(22) => "enum_member",
        Some(23) => "struct",
        Some(24) => "event",
        Some(26) => "type_parameter",
        _ => "symbol",
    }
    .to_string()
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
    // 2) Fall back to the Cargo workspace target dir (dev convenience for
    //    built-in language servers built by this workspace).
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
    elif method == "textDocument/documentSymbol":
        reply(msg["id"], [{
            "name": "main",
            "kind": 12,
            "range": {"start": {"line": 2, "character": 0},
                       "end": {"line": 6, "character": 1}},
            "selectionRange": {"start": {"line": 2, "character": 5},
                               "end": {"line": 2, "character": 9}},
            "children": [{
                "name": "x",
                "kind": 13,
                "range": {"start": {"line": 3, "character": 4},
                          "end": {"line": 3, "character": 5}},
                "selectionRange": {"start": {"line": 3, "character": 4},
                                   "end": {"line": 3, "character": 5}},
            }],
        }])
    elif method == "shutdown":
        reply(msg["id"], None)
    elif method == "exit":
        break
"#;

    struct MockServer {
        bin: PathBuf,
        _dir: tempdir::TempDir,
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
            MockServer {
                bin,
                _dir: dir,
            }
        }

        /// Spawn the mock server in `root`. The mock binary is passed
        /// explicitly so it never depends on (or mutates) the process PATH.
        fn spawn_server(&self, root: &Path) -> LspServer {
            LspServer::spawn_with_path("clangd", &self.bin, root).unwrap()
        }
    }

    #[test]
    fn initialize_and_completion() {
        let mock = MockServer::new();
        let root = tempdir::TempDir::new("ns-lsp-root").unwrap();
        let server = mock.spawn_server(root.path());

        let rx = server
            .completion(PathBuf::from("/mock/a.c"), 0, 5)
            .unwrap();
        let response = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        let items = response["result"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["label"], "foo");
        assert_eq!(items[1]["label"], "bar");
    }

    #[test]
    fn definition_returns_location() {
        let mock = MockServer::new();
        let root = tempdir::TempDir::new("ns-lsp-root").unwrap();
        let server = mock.spawn_server(root.path());

        let rx = server
            .definition(PathBuf::from("/mock/a.c"), 3, 1)
            .unwrap();
        let response = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        let uri = response["result"]["uri"].as_str().unwrap();
        assert_eq!(uri, "file:///mock/target.c");
        let line = response["result"]["range"]["start"]["line"].as_u64().unwrap();
        assert_eq!(line, 9);
    }

    #[test]
    fn diagnostics_pushed_on_open() {
        let mock = MockServer::new();
        let root = tempdir::TempDir::new("ns-lsp-root").unwrap();
        let server = mock.spawn_server(root.path());
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

    #[test]
    fn document_symbols_returns_nested_outline() {
        let mock = MockServer::new();
        let root = tempdir::TempDir::new("ns-lsp-root").unwrap();
        let server = mock.spawn_server(root.path());

        let rx = server.document_symbols(PathBuf::from("/mock/a.c")).unwrap();
        let response = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        let symbols = parse_symbols(&response);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "main");
        assert_eq!(symbols[0].kind, "function");
        assert_eq!(symbols[0].line, 2);
        assert_eq!(symbols[0].children.len(), 1);
        assert_eq!(symbols[0].children[0].name, "x");
        assert_eq!(symbols[0].children[0].kind, "variable");
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

    #[test]
    fn parses_document_symbols_with_nesting() {
        let response = serde_json::json!([
            {
                "name": "main",
                "kind": 12,
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 3, "character": 1 } },
                "children": [
                    {
                        "name": "x",
                        "kind": 13,
                        "range": { "start": { "line": 1, "character": 4 }, "end": { "line": 1, "character": 10 } },
                    }
                ],
            }
        ]);
        let symbols = parse_symbols(&response);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "main");
        assert_eq!(symbols[0].kind, "function");
        assert_eq!(symbols[0].line, 0);
        assert_eq!(symbols[0].children.len(), 1);
        assert_eq!(symbols[0].children[0].name, "x");
        assert_eq!(symbols[0].children[0].kind, "variable");
    }

    #[test]
    fn parses_flat_symbol_information() {
        let response = serde_json::json!([
            {
                "name": "global",
                "kind": 12,
                "location": {
                    "uri": "file:///a.c",
                    "range": { "start": { "line": 5, "character": 0 }, "end": { "line": 5, "character": 6 } },
                },
            }
        ]);
        let symbols = parse_symbols(&response);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "global");
        assert_eq!(symbols[0].line, 5);
        assert!(symbols[0].children.is_empty());
    }

    /// `parse_workspace_edit` must flatten the `changes: {uri: [TextEdit]}`
    /// map into file edits, skipping malformed entries and unknown URIs.
    #[test]
    fn parses_workspace_edit_changes() {
        let response = serde_json::json!({
            "changes": {
                "file:///proj/src/main.c": [
                    {
                        "range": {
                            "start": { "line": 0, "character": 0 },
                            "end": { "line": 0, "character": 3 }
                        },
                        "newText": "foo"
                    },
                    {
                        "range": {
                            "start": { "line": 2, "character": 1 },
                            "end": { "line": 2, "character": 4 }
                        },
                        "newText": "bar"
                    }
                ],
                "file:///proj/src/util.h": [
                    {
                        "range": {
                            "start": { "line": 1, "character": 0 },
                            "end": { "line": 1, "character": 2 }
                        },
                        "newText": "baz"
                    }
                ]
            }
        });
        let edits = parse_workspace_edit(&response);
        assert_eq!(edits.len(), 3);
        assert_eq!(edits[0].path, PathBuf::from("/proj/src/main.c"));
        assert_eq!(edits[0].start_line, 0);
        assert_eq!(edits[0].start_char, 0);
        assert_eq!(edits[0].end_line, 0);
        assert_eq!(edits[0].end_char, 3);
        assert_eq!(edits[0].new_text, "foo");
        assert_eq!(edits[1].path, PathBuf::from("/proj/src/main.c"));
        assert_eq!(edits[1].new_text, "bar");
        assert_eq!(edits[2].path, PathBuf::from("/proj/src/util.h"));
        assert_eq!(edits[2].new_text, "baz");
    }

    /// A `workspace/symbol` response is flattened into `WorkspaceSymbolInfo`
    /// entries carrying name, kind, path and start position.
    #[test]
    fn parses_workspace_symbols_flat_and_nested() {
        let response = serde_json::json!({
            "result": [
                {
                    "name": "main",
                    "kind": 12,
                    "location": {
                        "uri": "file:///proj/src/main.c",
                        "range": {
                            "start": { "line": 3, "character": 5 },
                            "end": { "line": 3, "character": 9 }
                        }
                    }
                },
                {
                    "name": "timer_init",
                    "kind": 6,
                    "location": {
                        "uri": "file:///proj/src/driver.c",
                        "range": {
                            "start": { "line": 10, "character": 0 },
                            "end": { "line": 10, "character": 2 }
                        }
                    }
                }
            ]
        });
        let symbols = parse_workspace_symbols(&response);
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].name, "main");
        assert_eq!(symbols[0].kind, "function");
        assert_eq!(symbols[0].path, PathBuf::from("/proj/src/main.c"));
        assert_eq!(symbols[0].line, 3);
        assert_eq!(symbols[0].column, 5);
        assert_eq!(symbols[1].name, "timer_init");
        assert_eq!(symbols[1].kind, "method");
        assert_eq!(symbols[1].line, 10);
    }

    /// Malformed edits (missing range/newText) and unknown schemes are skipped.
    #[test]
    fn parses_workspace_edit_skips_malformed() {
        let response = serde_json::json!({
            "changes": {
                "file:///proj/a.c": [
                    { "range": { "start": {}, "end": {} } }, // missing newText
                    { "newText": "x" } // missing range
                ],
                "ftp://host/a.c": [ { "range": {}, "newText": "x" } ]
            }
        });
        assert!(parse_workspace_edit(&response).is_empty());
    }

    /// `parse_format_edits` reads a bare `TextEdit[]` and tags the requested
    /// path onto each edit.
    #[test]
    fn parses_format_edits_array() {
        let response = serde_json::json!([
            {
                "range": {
                    "start": { "line": 1, "character": 0 },
                    "end": { "line": 1, "character": 8 }
                },
                "newText": "    int x;"
            },
            {
                "range": {
                    "start": { "line": 3, "character": 2 },
                    "end": { "line": 3, "character": 2 }
                },
                "newText": ""
            }
        ]);
        let edits = parse_format_edits(&response, Path::new("/proj/src/main.c"));
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].path, PathBuf::from("/proj/src/main.c"));
        assert_eq!(edits[0].start_line, 1);
        assert_eq!(edits[0].new_text, "    int x;");
        assert_eq!(edits[1].end_line, 3);
        assert_eq!(edits[1].new_text, "");
        // A non-array response yields no edits.
        assert!(parse_format_edits(&serde_json::json!({}), Path::new("/x")).is_empty());
    }

    /// `parse_code_actions` keeps actions with edits and drops command-only
    /// ones (empty edits, no client-side command handler).
    #[test]
    fn parses_code_actions_with_edits() {
        let response = serde_json::json!([
            {
                "title": "Remove unused import",
                "edit": {
                    "changes": {
                        "file:///proj/src/main.c": [
                            {
                                "range": {
                                    "start": { "line": 0, "character": 0 },
                                    "end": { "line": 0, "character": 12 }
                                },
                                "newText": ""
                            }
                        ]
                    }
                }
            },
            {
                "title": "Command-only fix",
                "command": { "title": "x", "command": "fix" }
            }
        ]);
        let actions = parse_code_actions(&response);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].title, "Remove unused import");
        assert_eq!(actions[0].edits.len(), 1);
        assert_eq!(actions[0].edits[0].path, PathBuf::from("/proj/src/main.c"));
        assert_eq!(actions[0].edits[0].new_text, "");
        // Command-only action: no edits, title still parsed.
        assert_eq!(actions[1].title, "Command-only fix");
        assert!(actions[1].edits.is_empty());
    }
}
