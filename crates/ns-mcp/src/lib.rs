//! MCP (Model Context Protocol) server for ns-embed-ide.
//!
//! Exposes IDE capabilities as JSON-RPC tools over stdio transport.

use anyhow::Result;
use serde_json::json;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};

pub mod tools;

// ── MCP protocol types ────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    params: Option<serde_json::Value>,
}

#[derive(serde::Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: serde_json::Value,
    result: Option<serde_json::Value>,
    error: Option<JsonRpcError>,
}

#[derive(serde::Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

// ── Main loop ─────────────────────────────────────────────────────────────────

pub fn run() -> Result<()> {
    eprintln!("ns-mcp server starting");

    let tools_list = tools::list_all();

    loop {
        let Some(line) = read_line() else { break };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let Ok(request) = serde_json::from_str::<JsonRpcRequest>(&line) else {
            send_error(serde_json::json!(null), "Parse error", -32700);
            continue;
        };

        let _proto = &request.jsonrpc;
        match request.method.as_str() {
            "initialize" => {
                send_result(
                    request.id.unwrap_or(json!(null)),
                    json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "serverInfo": { "name": "ns-embed-ide", "version": env!("CARGO_PKG_VERSION") }
                    }),
                );
            }
            "initialized" => {}
            "tools/list" => {
                send_result(
                    request.id.unwrap_or(json!(null)),
                    json!({ "tools": tools_list }),
                );
            }
            "tools/call" => {
                let params = request.params.as_ref().expect("missing params");
                let name: String = params["name"].as_str().unwrap_or("").to_string();
                let args: HashMap<String, serde_json::Value> =
                    serde_json::from_value(params.get("arguments").cloned().unwrap_or_default())
                        .unwrap_or_default();

                match tools::execute(&name, &args) {
                    Ok(output) => {
                        send_result(
                            request.id.unwrap_or(json!(null)),
                            json!({
                                "content": [{ "type": "text", "text": format!("{}", output) }]
                            }),
                        );
                    }
                    Err(e) => {
                        send_error(request.id.unwrap_or(json!(null)), &e.to_string(), -32000);
                    }
                }
            }
            "ping" => {
                send_result(request.id.unwrap_or(json!(null)), json!({}));
            }
            _ => {
                send_error(
                    request.id.unwrap_or(json!(null)),
                    &format!("Unknown method: {}", request.method),
                    -32601,
                );
            }
        }
    }

    eprintln!("ns-mcp server shut down");
    Ok(())
}

fn read_line() -> Option<String> {
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).ok()?;
    if line.trim().is_empty() {
        None
    } else {
        Some(line)
    }
}

fn send_result(id: serde_json::Value, result: impl serde::Serialize) {
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: Some(serde_json::to_value(result).unwrap()),
        error: None,
    };
    write_json(&resp);
}

fn send_error(id: serde_json::Value, message: &str, code: i32) {
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_string(),
        }),
    };
    write_json(&resp);
}

fn write_json(val: &impl serde::Serialize) {
    let s = serde_json::to_string(val).unwrap() + "\n";
    io::stdout().write_all(s.as_bytes()).unwrap();
    io::stdout().flush().unwrap();
}
