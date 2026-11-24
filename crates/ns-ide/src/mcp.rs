//! MCP (Model Context Protocol) server panel.
//!
//! The panel owns a child `ns-mcp` process speaking JSON-RPC 2.0 over stdio.
//! Protocol/state transitions live in pure helpers (see [`mcp_proto`]) so they
//! can be unit-tested without a live process or a GPUI window; the [`McpPanel`]
//! entity wires those helpers to the UI and the child process.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use gpui::prelude::*;
use gpui::{Context, Entity, EventEmitter, Render, Window, div, px};

use crate::text_field::TextField;
use crate::theme;
use crate::widgets;

// ── Protocol / state helpers (pure, unit-testable) ─────────────────────────────

/// Pure JSON-RPC helpers shared by the UI and its tests.
mod mcp_proto {
    use serde_json::{Value, json};

    /// Build a JSON-RPC 2.0 request line.
    pub fn request(id: u64, method: &str, params: Value) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
        .to_string()
    }

    /// Build a JSON-RPC 2.0 notification (no id).
    pub fn notification(method: &str, params: Value) -> String {
        json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        })
        .to_string()
    }

    /// Classify a raw input line into a protocol message to send.
    ///
    /// - A line starting with `{` is treated as a raw JSON-RPC message and is
    ///   returned as-is (after trimming). Invalid JSON is reported so the UI can
    ///   show an error instead of silently sending garbage.
    /// - Otherwise the line is `tool_name [json-arguments]` and becomes a
    ///   `tools/call` request with the given id.
    #[derive(Debug)]
    pub enum Outgoing {
        Raw(String),
        ToolCall {
            name: String,
            args: String,
            line: String,
        },
        InvalidJson(String),
        Empty,
    }

    pub fn classify_input(input: &str, id: u64) -> Outgoing {
        let text = input.trim();
        if text.is_empty() {
            return Outgoing::Empty;
        }
        if text.starts_with('{') {
            match serde_json::from_str::<Value>(text) {
                Ok(_) => Outgoing::Raw(text.to_string()),
                Err(_) => Outgoing::InvalidJson(text.to_string()),
            }
        } else {
            let mut parts = text.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or("").to_string();
            let args = parts
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("{}")
                .to_string();
            Outgoing::ToolCall {
                line: request(
                    id,
                    "tools/call",
                    json!({
                        "name": name,
                        "arguments": serde_json::from_str::<Value>(&args)
                            .unwrap_or_else(|_| json!({ "_raw": args })),
                    }),
                ),
                name,
                args,
            }
        }
    }

    /// True when a server stdout line is a successful `initialize` response.
    /// Accepts both a bare result object and a full JSON-RPC envelope.
    pub fn is_initialize_response(line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            return false;
        };
        let object = value.get("result").unwrap_or(&value);
        object
            .get("protocolVersion")
            .and_then(Value::as_str)
            .is_some()
            && object.get("serverInfo").is_some()
    }

    /// Truncate a log buffer to at most `limit` lines by dropping the oldest.
    pub fn trim_log(lines: &mut Vec<String>, limit: usize, drop_count: usize) {
        if lines.len() > limit {
            let n = drop_count.min(lines.len());
            lines.drain(..n);
        }
    }
}

#[derive(Debug, Clone)]
pub enum McpEvent {
    Log(String),
    Error(String),
    /// The child `ns-mcp` process exited (with its exit status description).
    Exited(String),
}

pub struct McpPanel {
    running: bool,
    ready: bool,
    next_id: u64,
    writer: Option<Box<dyn Write + Send>>,
    log_lines: Vec<String>,
    input: Entity<TextField>,
    input_wired: bool,
    tx: Sender<McpEvent>,
    receiver: Option<Receiver<McpEvent>>,
}

impl McpPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (tx, rx) = channel();
        Self {
            running: false,
            ready: false,
            next_id: 1,
            writer: None,
            log_lines: Vec::new(),
            input: cx.new(|cx| TextField::new("发送 JSON-RPC 请求（工具调用）...", cx)),
            input_wired: false,
            tx,
            receiver: Some(rx),
        }
    }

    /// Resolve the `ns-mcp` binary path: compile-time override, then sibling of
    /// the running IDE binary, then PATH.
    fn binary_path() -> String {
        option_env!("NS_MCP_BIN")
            .map(|b| b.to_string())
            .or_else(|| {
                std::env::current_exe().ok().map(|mut p| {
                    p.pop();
                    p.join("ns-mcp").display().to_string()
                })
            })
            .unwrap_or_else(|| "ns-mcp".to_string())
    }

    /// Spawn the child process and begin the MCP handshake. Safe to call while
    /// already running (no-op in that case).
    pub fn start(&mut self, project_root: &std::path::Path, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let bin = Self::binary_path();

        match Command::new(&bin)
            .current_dir(project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                let pid = child.id();
                let stdout = child.stdout.take().unwrap();
                let stderr = child.stderr.take().unwrap();
                let stdin = child.stdin.take().unwrap();
                self.writer = Some(Box::new(stdin));
                self.running = true;
                self.ready = false;
                self.log_lines.push(format!(
                    "[mcp] 启动 ns-mcp ({pid})，cwd={}",
                    project_root.display()
                ));

                let tx = self.tx.clone();
                thread::spawn(move || {
                    for line in BufReader::new(stdout).lines() {
                        let Ok(line) = line else { break };
                        if !line.trim().is_empty() {
                            let _ = tx.send(McpEvent::Log(format!("[out] {line}")));
                        }
                    }
                });
                let tx2 = self.tx.clone();
                thread::spawn(move || {
                    for line in BufReader::new(stderr).lines() {
                        let Ok(line) = line else { break };
                        if !line.trim().is_empty() {
                            let _ = tx2.send(McpEvent::Log(format!("[err] {line}")));
                        }
                    }
                });
                // Reap the child and notify the UI on exit. Holding `Child` in
                // this thread (rather than the panel) avoids zombies and keeps
                // the Exit event on the real production path.
                let tx3 = self.tx.clone();
                thread::spawn(move || {
                    let status = child.wait();
                    let detail = match status {
                        Ok(s) => format!("退出状态 {s}"),
                        Err(e) => format!("等待进程失败：{e}"),
                    };
                    let _ = tx3.send(McpEvent::Exited(detail));
                });

                // Send the mandatory MCP handshake. `ready` flips when the
                // matching initialize response is observed by `poll_events`.
                let id = self.take_id();
                let init = mcp_proto::request(
                    id,
                    "initialize",
                    serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "clientInfo": { "name": "ns-embed-ide", "version": env!("CARGO_PKG_VERSION") }
                    }),
                );
                self.send_line(&init);
                self.log_lines.push(format!("→ {init}"));
                cx.notify();
            }
            Err(e) => {
                self.running = false;
                self.ready = false;
                let _ = self
                    .tx
                    .send(McpEvent::Error(format!("无法启动 ns-mcp: {e}")));
                cx.notify();
            }
        }
    }

    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    /// Drain pending events from the child process into the log. Returns true
    /// when any state changed (so the caller can request a repaint).
    pub fn poll_events(&mut self) -> bool {
        let Some(rx) = &self.receiver else {
            return false;
        };
        let mut changed = false;
        let mut handshake_ack = false;
        while let Ok(evt) = rx.try_recv() {
            changed = true;
            match evt {
                McpEvent::Log(line) => {
                    if self.running
                        && !self.ready
                        && mcp_proto::is_initialize_response(
                            line.strip_prefix("[out] ").unwrap_or(&line),
                        )
                    {
                        self.ready = true;
                        handshake_ack = true;
                    }
                    self.log_lines.push(line);
                }
                McpEvent::Error(e) => self.log_lines.push(format!("[ERROR] {e}")),
                McpEvent::Exited(status) => {
                    self.running = false;
                    self.ready = false;
                    self.log_lines.push(format!("[mcp] {status}"));
                }
            }
        }
        if handshake_ack {
            let note = mcp_proto::notification("initialized", serde_json::json!({}));
            self.send_line(&note);
            self.log_lines.push(format!("← {note}"));
            self.log_lines.push("[mcp] 握手完成，服务就绪".into());
        }
        mcp_proto::trim_log(&mut self.log_lines, 500, 100);
        changed
    }

    /// Disconnect from the server and reset connection state. Dropping the
    /// writer closes stdin; the `ns-mcp` read loop sees EOF and exits, and the
    /// wait thread reaps it and emits `Exited`.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.writer.take();
        self.running = false;
        self.ready = false;
        self.log_lines.push("[mcp] 已断开".into());
        cx.notify();
    }

    /// Request the server's tool catalog.
    pub fn list_tools(&mut self, cx: &mut Context<Self>) {
        let id = self.take_id();
        let line = mcp_proto::request(id, "tools/list", serde_json::json!({}));
        self.send_line(&line);
        self.log_lines.push("→ list_tools".into());
        cx.notify();
    }

    fn send_line(&mut self, line: &str) {
        if let Some(w) = self.writer.as_mut() {
            let msg = format!("{line}\n");
            let _ = w.write_all(msg.as_bytes());
            let _ = w.flush();
        }
    }

    /// Process a submitted text value (raw JSON-RPC line or `tool args`).
    ///
    /// This deliberately does **not** touch the input entity: it is called
    /// from the `TextField` submit callback while that entity is mutably
    /// borrowed, and the field clears itself. The button path calls
    /// [`Self::submit_input`], which reads and clears the field first.
    pub fn submit_text(&mut self, text: &str, cx: &mut Context<Self>) -> mcp_proto::Outgoing {
        let text = text.trim();
        let id = self.take_id();
        let outgoing = mcp_proto::classify_input(text, id);
        match &outgoing {
            mcp_proto::Outgoing::Empty => return outgoing,
            mcp_proto::Outgoing::InvalidJson(raw) => {
                self.log_lines.push(format!("[ERROR] 无效 JSON：{raw}"));
                cx.notify();
                return outgoing;
            }
            mcp_proto::Outgoing::Raw(line) => {
                self.send_line(line);
                self.log_lines.push(format!("→ {line}"));
            }
            mcp_proto::Outgoing::ToolCall { line, name, args } => {
                self.send_line(line);
                self.log_lines.push(format!("→ call {name}({args})"));
            }
        }
        cx.notify();
        outgoing
    }

    /// Button path: read the input field, submit its text, then clear the field.
    pub fn submit_input(&mut self, cx: &mut Context<Self>) -> mcp_proto::Outgoing {
        let text = self.input.read(cx).text().trim().to_string();
        let outgoing = self.submit_text(&text, cx);
        self.input.update(cx, |tf, cx| tf.set_text("", cx));
        outgoing
    }
}

impl EventEmitter<McpEvent> for McpPanel {}

impl Render for McpPanel {
    fn render(&mut self, _win: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Wire Enter-to-submit once. The field clears itself on submit, so the
        // callback must not touch the input entity (see `submit_text`).
        if !self.input_wired {
            self.input_wired = true;
            let this = cx.entity();
            self.input.update(cx, |field, _cx| {
                field.set_on_submit(move |text, _window, cx| {
                    this.update(cx, |panel, cx| {
                        panel.submit_text(text, cx);
                    });
                });
            });
        }

        let status_color = if self.running {
            if self.ready {
                theme::color(theme::JADE)
            } else {
                theme::color(theme::ACCENT)
            }
        } else {
            theme::color(theme::TEXT_FAINT)
        };
        let status_label = if self.running {
            if self.ready {
                "● 运行中"
            } else {
                "◌ 连接中…"
            }
        } else {
            "○ 未连接"
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(10.))
                    .py(px(6.))
                    .bg(theme::color(theme::PANEL_SOLID))
                    .border_b_1()
                    .border_color(theme::alpha(theme::STROKE, 0.18))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().w(px(6.)).h(px(6.)).rounded_full().bg(status_color))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme::color(theme::TEXT_FAINT))
                                    .child(status_label),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme::color(theme::TEXT_FAINT))
                                    .child(format!("日志 {} 行", self.log_lines.len())),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .child(widgets::ghost_button("mcp-连接", "连接").when(
                                !self.running,
                                |b| {
                                    b.on_click(cx.listener(|this, _, _, cx| {
                                        let root = std::env::current_dir().unwrap_or_default();
                                        this.start(&root, cx);
                                    }))
                                },
                            ))
                            .child(widgets::ghost_button("mcp-列出工具", "列出工具").when(
                                self.running,
                                |b| {
                                    b.on_click(cx.listener(|this, _, _, cx| {
                                        this.list_tools(cx);
                                    }))
                                },
                            ))
                            .child(widgets::ghost_button("mcp-断开", "断开").when(
                                self.running,
                                |b| {
                                    b.on_click(cx.listener(|this, _, _, cx| {
                                        this.stop(cx);
                                    }))
                                },
                            ))
                            .child(widgets::ghost_button("mcp-清空", "清空").on_click(
                                cx.listener(|this, _, _, _cx| {
                                    this.log_lines.clear();
                                    _cx.notify();
                                }),
                            )),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .font_family("Menlo")
                    .text_size(px(11.))
                    .children(
                        self.log_lines
                            .iter()
                            .map(|line| {
                                let color = if line.contains("ERROR") || line.contains("[err]") {
                                    theme::color(theme::DANGER)
                                } else if line.contains("[out]") {
                                    theme::color(theme::TEXT_FAINT)
                                } else {
                                    theme::color(theme::TEXT)
                                };
                                div()
                                    .px(px(8.))
                                    .py(px(1.))
                                    .text_color(color)
                                    .child(line.clone())
                            })
                            .collect::<Vec<_>>(),
                    ),
            )
            .child(
                div()
                    .px(px(8.))
                    .py(px(4.))
                    .border_t_1()
                    .border_color(theme::alpha(theme::STROKE, 0.18))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme::color(theme::TEXT_FAINT))
                                    .child("> "),
                            )
                            .child(div().flex_1().child(self.input.clone()))
                            .child(widgets::ghost_button("mcp-send", "发送").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.submit_input(cx);
                                }),
                            )),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    // ── Pure protocol helper tests ────────────────────────────────────────────

    #[test]
    fn request_builds_valid_json_rpc_envelope() {
        let line = mcp_proto::request(7, "tools/list", serde_json::json!({}));
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 7);
        assert_eq!(v["method"], "tools/list");
        assert_eq!(v["params"], serde_json::json!({}));
    }

    #[test]
    fn notification_has_no_id() {
        let line = mcp_proto::notification("initialized", serde_json::json!({"x": 1}));
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert!(v.get("id").is_none());
        assert_eq!(v["method"], "initialized");
    }

    #[test]
    fn classify_input_handles_raw_json() {
        let raw = r#"{"jsonrpc":"2.0","id":99,"method":"ping"}"#;
        match mcp_proto::classify_input(raw, 3) {
            mcp_proto::Outgoing::Raw(line) => assert_eq!(line, raw),
            other => panic!("expected Raw, got {other:?}"),
        }
    }

    #[test]
    fn classify_input_rejects_invalid_json() {
        match mcp_proto::classify_input("{not json", 3) {
            mcp_proto::Outgoing::InvalidJson(s) => assert_eq!(s, "{not json"),
            other => panic!("expected InvalidJson, got {other:?}"),
        }
    }

    #[test]
    fn classify_input_builds_tool_call_with_args() {
        match mcp_proto::classify_input("decode_number {\"value\":\"0x10\"}", 5) {
            mcp_proto::Outgoing::ToolCall { line, name, args } => {
                assert_eq!(name, "decode_number");
                assert_eq!(args, "{\"value\":\"0x10\"}");
                let v: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(v["method"], "tools/call");
                assert_eq!(v["id"], 5);
                assert_eq!(v["params"]["name"], "decode_number");
                assert_eq!(v["params"]["arguments"]["value"], "0x10");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn classify_input_defaults_empty_args_and_ignores_bad_args() {
        match mcp_proto::classify_input("project_info", 1) {
            mcp_proto::Outgoing::ToolCall { line, args, .. } => {
                assert_eq!(args, "{}");
                let v: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(v["params"]["arguments"], serde_json::json!({}));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        match mcp_proto::classify_input("project_info {oops", 1) {
            mcp_proto::Outgoing::ToolCall { line, .. } => {
                let v: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(v["params"]["arguments"]["_raw"], "{oops");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn classify_input_empty_is_noop() {
        assert!(matches!(
            mcp_proto::classify_input("   ", 1),
            mcp_proto::Outgoing::Empty
        ));
    }

    #[test]
    fn initialize_response_detection() {
        let envelope = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"ns-mcp","version":"0.1.0"}}}"#;
        assert!(mcp_proto::is_initialize_response(envelope));
        let bare = r#"{"protocolVersion":"2024-11-05","serverInfo":{"name":"x"}}"#;
        assert!(mcp_proto::is_initialize_response(bare));
        assert!(!mcp_proto::is_initialize_response(r#"{"result":{"x":1}}"#));
        assert!(!mcp_proto::is_initialize_response("not json"));
        // A normal tool output must not flip the handshake.
        assert!(!mcp_proto::is_initialize_response(
            r#"{"jsonrpc":"2.0","id":2,"result":{"content":[]}}"#
        ));
    }

    #[test]
    fn trim_log_drops_oldest_only_when_over_limit() {
        let mut lines: Vec<String> = (0..10).map(|i| format!("L{i}")).collect();
        mcp_proto::trim_log(&mut lines, 5, 3);
        assert_eq!(lines.len(), 7);
        assert_eq!(lines.first().unwrap(), "L3");
        mcp_proto::trim_log(&mut lines, 20, 3);
        assert_eq!(lines.len(), 7);
    }

    // ── GPUI UI / interaction tests (headless) ────────────────────────────────

    #[gpui::test]
    fn panel_starts_disconnected(cx: &mut TestAppContext) {
        let panel = cx.add_window_view(|_, cx| McpPanel::new(cx)).0;
        let state = cx.read(|app| {
            let p = panel.read(app);
            (p.running, p.ready, p.next_id, p.log_lines.len())
        });
        assert_eq!(state, (false, false, 1, 0));
    }

    #[gpui::test]
    fn submit_tool_call_appends_log_and_increments_id(cx: &mut TestAppContext) {
        let panel = cx.add_window_view(|_, cx| McpPanel::new(cx)).0;
        panel.update(cx, |p, cx| {
            p.submit_text("decode_number {\"value\":\"0x10\"}", cx);
        });
        panel.update(cx, |p, cx| {
            p.submit_text("project_info", cx);
        });
        let (next_id, log) = cx.read(|app| {
            let p = panel.read(app);
            (p.next_id, p.log_lines.clone())
        });
        assert_eq!(next_id, 3, "two submits consume ids 1 and 2");
        assert!(log.iter().any(|l| l.contains("call decode_number")));
        assert!(log.iter().any(|l| l.contains("call project_info")));
    }

    #[gpui::test]
    fn invalid_json_is_reported_not_sent(cx: &mut TestAppContext) {
        let panel = cx.add_window_view(|_, cx| McpPanel::new(cx)).0;
        let outgoing = panel.update(cx, |p, cx| p.submit_text("{bad", cx));
        assert!(matches!(outgoing, mcp_proto::Outgoing::InvalidJson(_)));
        let log = cx.read(|app| panel.read(app).log_lines.clone());
        assert!(log.iter().any(|l| l.contains("无效 JSON")));
    }

    #[gpui::test]
    fn handshake_response_flips_ready_and_sends_initialized(cx: &mut TestAppContext) {
        let panel = cx.add_window_view(|_, cx| McpPanel::new(cx)).0;
        // Simulate a running, not-yet-ready panel and feed an initialize response.
        panel.update(cx, |p, _| {
            p.running = true;
            p.ready = false;
        });
        panel.update(cx, |p, _| {
            p.tx
                .send(McpEvent::Log(
                    "[out] {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2024-11-05\",\"serverInfo\":{\"name\":\"ns-mcp\"}}}"
                        .into(),
                ))
                .unwrap();
        });
        panel.update(cx, |p, _| {
            assert!(p.poll_events());
        });
        let (ready, log) = cx.read(|app| {
            let p = panel.read(app);
            (p.ready, p.log_lines.clone())
        });
        assert!(ready, "initialize response must mark the panel ready");
        assert!(log.iter().any(|l| l.contains("握手完成")));
        assert!(log.iter().any(|l| l.contains("initialized")));
    }

    #[gpui::test]
    fn exit_event_resets_connection_state(cx: &mut TestAppContext) {
        let panel = cx.add_window_view(|_, cx| McpPanel::new(cx)).0;
        panel.update(cx, |p, _| {
            p.running = true;
            p.ready = true;
        });
        panel.update(cx, |p, _| {
            p.tx.send(McpEvent::Exited("退出码 0".into())).unwrap();
        });
        panel.update(cx, |p, _| {
            assert!(p.poll_events());
        });
        let (running, ready) = cx.read(|app| {
            let p = panel.read(app);
            (p.running, p.ready)
        });
        assert!(!running && !ready);
    }

    #[gpui::test]
    fn stop_resets_state(cx: &mut TestAppContext) {
        let panel = cx.add_window_view(|_, cx| McpPanel::new(cx)).0;
        panel.update(cx, |p, cx| {
            p.running = true;
            p.ready = true;
            p.stop(cx);
        });
        let (running, ready, log) = cx.read(|app| {
            let p = panel.read(app);
            (p.running, p.ready, p.log_lines.clone())
        });
        assert!(!running && !ready);
        assert!(log.iter().any(|l| l.contains("已断开")));
    }

    #[gpui::test]
    fn render_exposes_status_and_log_regions(cx: &mut TestAppContext) {
        use gpui::{point, size};

        let (panel, cx) = cx.add_window_view(|_, cx| McpPanel::new(cx));
        // Drive a full layout/prepaint/paint of the panel. `add_window_view`
        // already rendered once; draw again explicitly to exercise the path.
        cx.draw(point(px(0.), px(0.)), size(px(800.), px(600.)), |_, _| {
            panel.clone()
        });
        // The panel must render without panicking on an empty log and a lazy
        // input, and Enter-to-submit must have been wired by the first render.
        let (n, wired) = cx.read(|app| {
            let p = panel.read(app);
            (p.log_lines.len(), p.input_wired)
        });
        assert_eq!(n, 0);
        assert!(wired, "first render must wire Enter-to-submit");
    }
}
