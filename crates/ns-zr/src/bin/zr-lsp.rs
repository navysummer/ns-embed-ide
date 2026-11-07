//! A built-in LSP server for ZR, spoken over stdio with Content-Length framing.
//!
//! This is the language server that the IDE's LSP client (`ns_lsp`) launches
//! for ZR projects. It implements a minimal but real LSP: `initialize`,
//! `textDocument/didOpen`/`didChange`, `publishDiagnostics`, `completion`,
//! `hover`, and `shutdown`/`exit`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

fn main() {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();

    // path -> (text, version)
    let mut documents: HashMap<PathBuf, (String, i32)> = HashMap::new();

    loop {
        let Some(message) = read_message(&mut reader) else {
            break;
        };
        let method = message.get("method").and_then(|v| v.as_str()).map(str::to_string);
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        match method.as_deref() {
            Some("initialize") => {
                if let Some(id) = &id {
                    let result = json!({
                        "capabilities": {
                            "textDocumentSync": 1, // Full
                            "completionProvider": {
                                "resolveProvider": false,
                                "triggerCharacters": [".", ":"]
                            },
                            "hoverProvider": true,
                            "definitionProvider": false,
                            "referencesProvider": false
                        },
                        "serverInfo": { "name": "zr-lsp", "version": "0.1.0" }
                    });
                    respond(&mut writer, id, result);
                }
            }
            Some("initialized") => {}
            Some("textDocument/didOpen") => {
                if let Some(params) = params.as_object() {
                    if let Some(td) = params.get("textDocument").and_then(|v| v.as_object()) {
                        let uri = td.get("uri").and_then(|v| v.as_str());
                        if let Some(uri) = uri {
                            let text = td.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            let path = uri_to_path(uri);
                            documents.insert(path.clone(), (text.to_string(), 0));
                            publish_diagnostics(&mut writer, &path, text);
                        }
                    }
                }
            }
            Some("textDocument/didChange") => {
                if let Some(params) = params.as_object() {
                    let td = params.get("textDocument").and_then(|v| v.as_object());
                    let uri = td.and_then(|t| t.get("uri")).and_then(|v| v.as_str());
                    let version = td.and_then(|t| t.get("version")).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                    let text = params
                        .get("contentChanges")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                        .and_then(|c| c.get("text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if let Some(uri) = uri {
                        let path = uri_to_path(uri);
                        documents.insert(path.clone(), (text.to_string(), version));
                        publish_diagnostics(&mut writer, &path, text);
                    }
                }
            }
            Some("textDocument/completion") => {
                if let Some(id) = &id {
                    let items = completion_items();
                    respond(&mut writer, id, json!({ "isIncomplete": false, "items": items }));
                }
            }
            Some("textDocument/hover") => {
                if let Some(id) = &id {
                    let contents = json!({
                        "kind": "markdown",
                        "value": "**ZR** 内建语言\n\n`zrc` 编译、`zr-lint` 静态检查、`zr-lsp` 语言服务。"
                    });
                    respond(&mut writer, id, json!({ "contents": contents }));
                }
            }
            Some("shutdown") => {
                if let Some(id) = &id {
                    respond(&mut writer, id, Value::Null);
                }
            }
            Some("exit") => break,
            _ => {
                // Unknown request: reply null if it has an id (avoid hanging client).
                if let Some(id) = &id {
                    respond(&mut writer, id, Value::Null);
                }
            }
        }
    }
}

fn completion_items() -> Vec<Value> {
    ns_zr::KEYWORDS
        .iter()
        .map(|keyword| {
            json!({
                "label": keyword,
                "kind": 14, // Keyword
                "detail": "ZR 关键字"
            })
        })
        .collect()
}

fn publish_diagnostics<W: Write>(writer: &mut W, path: &PathBuf, text: &str) {
    let analysis = ns_zr::analyze(text);
    let diagnostics: Vec<Value> = analysis
        .diagnostics
        .iter()
        .map(|d| {
            let severity = match d.severity {
                ns_zr::Severity::Error => 1,
                ns_zr::Severity::Warning => 2,
            };
            json!({
                "range": {
                    "start": { "line": d.line.saturating_sub(1), "character": d.column.saturating_sub(1) },
                    "end": { "line": d.line.saturating_sub(1), "character": d.column.saturating_sub(1) + 1 }
                },
                "severity": severity,
                "message": d.message,
                "source": "zr-lsp"
            })
        })
        .collect();
    let uri = path_to_uri(path);
    notify(
        writer,
        "textDocument/publishDiagnostics",
        json!({ "uri": uri, "diagnostics": diagnostics }),
    );
}

fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).ok()?;
        if n == 0 {
            return None;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut buf = vec![0u8; content_length];
    reader.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

fn respond<W: Write>(writer: &mut W, id: &Value, result: Value) {
    write_message(writer, json!({ "jsonrpc": "2.0", "id": id, "result": result }));
}

fn notify<W: Write>(writer: &mut W, method: &str, params: Value) {
    write_message(writer, json!({ "jsonrpc": "2.0", "method": method, "params": params }));
}

fn write_message<W: Write>(writer: &mut W, message: Value) {
    let body = serde_json::to_vec(&message).unwrap_or_default();
    let _ = write!(writer, "Content-Length: {}\r\n\r\n", body.len());
    let _ = writer.write_all(&body);
    let _ = writer.flush();
}

fn uri_to_path(uri: &str) -> PathBuf {
    uri.strip_prefix("file://")
        .map(|s| s.split('?').next().unwrap_or(s))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(uri))
}

fn path_to_uri(path: &PathBuf) -> String {
    format!("file://{}", path.display())
}
