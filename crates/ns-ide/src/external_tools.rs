//! External tools panel: run user-defined shell commands with streaming output.
//!
//! Mirrors the agent panel's background-thread model: each tool run streams
//! stdout/stderr into a shared buffer and reports a lifecycle status. The host
//! polls [`ExternalToolsPanel::drain_events`] and re-renders.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use gpui::prelude::*;
use gpui::{Context, EventEmitter, Render, Window, div, px};

use crate::{theme, widgets};

/// Lifecycle state of one tool run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl ToolStatus {
    pub fn label(self) -> &'static str {
        match self {
            ToolStatus::Running => "运行中",
            ToolStatus::Succeeded => "成功",
            ToolStatus::Failed => "失败",
            ToolStatus::Cancelled => "已取消",
        }
    }

    pub fn color(self) -> gpui::Hsla {
        match self {
            ToolStatus::Running => theme::color(theme::ACCENT),
            ToolStatus::Succeeded => theme::color(theme::JADE),
            ToolStatus::Failed => theme::color(theme::DANGER),
            ToolStatus::Cancelled => theme::color(theme::TEXT_DIM),
        }
    }

    pub fn is_done(self) -> bool {
        matches!(
            self,
            ToolStatus::Succeeded | ToolStatus::Failed | ToolStatus::Cancelled
        )
    }
}

/// Snapshot of one tool run for rendering.
#[derive(Debug, Clone)]
pub struct ToolRun {
    pub id: u64,
    pub title: String,
    pub command: String,
    pub status: ToolStatus,
    pub output: String,
    pub exit_code: Option<i32>,
}

/// Events emitted by the tools panel.
#[derive(Debug, Clone)]
pub enum ToolEvent {
    Updated,
}

/// Mutable state of one tool run, shared with the runner thread.
struct SharedRun {
    status: ToolStatus,
    output: String,
    exit_code: Option<i32>,
    cancel: AtomicBool,
}

/// The external tools panel entity.
pub struct ExternalToolsPanel {
    runs: Vec<ToolRun>,
    shared: Vec<Arc<Mutex<SharedRun>>>,
    next_id: u64,
    receiver: Receiver<ToolEvent>,
    _sender: Sender<ToolEvent>,
    selected: Option<u64>,
}

impl ExternalToolsPanel {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        let (sender, receiver) = channel();
        Self {
            runs: Vec::new(),
            shared: Vec::new(),
            next_id: 1,
            receiver,
            _sender: sender,
            selected: None,
        }
    }

    /// Start a run of `title` with `command` in `cwd` (empty = no override).
    pub fn submit(
        &mut self,
        title: String,
        command: String,
        cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let command = command.trim().to_string();
        if command.is_empty() {
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let shared = Arc::new(Mutex::new(SharedRun {
            status: ToolStatus::Running,
            output: String::new(),
            exit_code: None,
            cancel: AtomicBool::new(false),
        }));
        let snapshot = ToolRun {
            id,
            title: title.clone(),
            command: command.clone(),
            status: ToolStatus::Running,
            output: String::new(),
            exit_code: None,
        };
        self.runs.insert(0, snapshot);
        self.shared.insert(0, shared.clone());
        self.selected = Some(id);

        let evt = self._sender.clone();
        std::thread::spawn(move || run_tool(command, cwd, shared, evt));
        cx.notify();
    }

    /// Consume pending events and sync the shared run snapshots.
    pub fn drain_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(_event) = self.receiver.try_recv() {
            changed = true;
        }
        if changed {
            self.refresh();
        }
        changed
    }

    fn refresh(&mut self) {
        for (snapshot, shared) in self.runs.iter_mut().zip(self.shared.iter()) {
            let run = shared.lock().unwrap();
            if snapshot.status != run.status || snapshot.output != run.output {
                snapshot.status = run.status;
                snapshot.output = run.output.clone();
                snapshot.exit_code = run.exit_code;
            }
        }
    }

    pub fn clear(&mut self) {
        let mut kept_runs = Vec::new();
        let mut kept_shared = Vec::new();
        for (snapshot, shared) in self.runs.drain(..).zip(self.shared.drain(..)) {
            if !shared.lock().unwrap().status.is_done() {
                kept_runs.push(snapshot);
                kept_shared.push(shared);
            }
        }
        self.runs = kept_runs;
        self.shared = kept_shared;
        if !self.runs.iter().any(|run| Some(run.id) == self.selected) {
            self.selected = self.runs.first().map(|run| run.id);
        }
    }

    pub fn cancel_all(&mut self) {
        for shared in &self.shared {
            shared.lock().unwrap().cancel.store(true, Ordering::SeqCst);
        }
    }
}

/// Run `command` in a platform shell, streaming stdout/stderr into `shared`.
fn run_tool(
    command: String,
    cwd: Option<PathBuf>,
    shared: Arc<Mutex<SharedRun>>,
    evt: Sender<ToolEvent>,
) {
    let shell = if std::env::consts::OS == "windows" {
        "cmd"
    } else {
        "sh"
    };
    let flag = if std::env::consts::OS == "windows" { "/C" } else { "-c" };
    let mut cmd = Command::new(shell);
    cmd.arg(flag)
        .arg(&command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut c = match cmd.spawn() {
        Ok(c) => c,
        Err(error) => {
            let mut run = shared.lock().unwrap();
            run.output.push_str(&format!("无法启动工具: {error}\n"));
            run.status = ToolStatus::Failed;
            run.exit_code = None;
            let _ = evt.send(ToolEvent::Updated);
            return;
        }
    };
    let stdout = c.stdout.take();
    let stderr = c.stderr.take();

    let read_pipe = |pipe: Option<Box<dyn std::io::Read + Send>>,
                     run_shared: Arc<Mutex<SharedRun>>,
                     evt: Sender<ToolEvent>| {
        let Some(pipe) = pipe else { return };
        let reader = BufReader::new(pipe);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let mut run = run_shared.lock().unwrap();
            run.output.push_str(&line);
            run.output.push('\n');
            let _ = evt.send(ToolEvent::Updated);
        }
    };
    let out_thread = std::thread::spawn({
        let run_shared = shared.clone();
        let evt = evt.clone();
        let pipe: Option<Box<dyn std::io::Read + Send>> =
            stdout.map(|p| Box::new(p) as Box<dyn std::io::Read + Send>);
        move || read_pipe(pipe, run_shared, evt)
    });
    let err_thread = std::thread::spawn({
        let run_shared = shared.clone();
        let evt = evt.clone();
        let pipe: Option<Box<dyn std::io::Read + Send>> =
            stderr.map(|p| Box::new(p) as Box<dyn std::io::Read + Send>);
        move || read_pipe(pipe, run_shared, evt)
    });
    let _ = out_thread.join();
    let _ = err_thread.join();
    let wait = c.wait();
    let exit_code = wait.ok().map(|st| st.code().unwrap_or(-1));
    let mut run = shared.lock().unwrap();
    run.exit_code = exit_code;
    run.status = if run.cancel.load(Ordering::SeqCst) {
        ToolStatus::Cancelled
    } else if exit_code == Some(0) {
        ToolStatus::Succeeded
    } else {
        ToolStatus::Failed
    };
    let _ = evt.send(ToolEvent::Updated);
}

impl EventEmitter<ToolEvent> for ExternalToolsPanel {}

impl Render for ExternalToolsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected;
        let runs = self.runs.clone();
        let selected_run = selected.and_then(|id| runs.iter().find(|r| r.id == id).cloned());
        let running = runs.iter().any(|r| !r.status.is_done());

        div()
            .flex_1()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(30.))
                    .flex_none()
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme::alpha(theme::STROKE, 0.08))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .child(format!("外部工具 · {} 次运行", runs.len())),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(widgets::ghost_button("tool-clear", "清空")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.clear();
                                    cx.notify();
                                })))
                            .when(running, |style| {
                                style.child(widgets::ghost_button("tool-cancel-all", "全部取消")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.cancel_all();
                                        cx.notify();
                                    })))
                            }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .child(
                        // Left: the run list.
                        div()
                            .id("tools-list")
                            .w(px(240.))
                            .flex_none()
                            .overflow_y_scroll()
                            .p(px(6.))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .when(runs.is_empty(), |style| {
                                style.child(
                                    div()
                                        .p(px(12.))
                                        .text_size(px(11.))
                                        .text_color(theme::color(theme::TEXT_FAINT))
                                        .child("还没有运行记录。用命令面板「运行工具…」选择一个配置的工具。"),
                                )
                            })
                            .children(runs.iter().enumerate().map(|(index, run)| {
                                let run_id = run.id;
                                let is_selected = Some(run_id) == selected;
                                div()
                                    .id(("tool-run", index))
                                    .px(px(8.))
                                    .py(px(5.))
                                    .rounded(px(6.))
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .bg(if is_selected {
                                        theme::alpha(theme::STROKE, 0.12)
                                    } else {
                                        theme::alpha(0xffffff, 0.01)
                                    })
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.07)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selected = Some(run_id);
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .flex_none()
                                            .size(px(8.))
                                            .rounded_full()
                                            .bg(run.status.color()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .text_size(px(10.5))
                                            .text_color(theme::color(theme::TEXT_DIM))
                                            .child(run.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_size(px(8.5))
                                            .text_color(run.status.color())
                                            .child(run.status.label()),
                                    )
                                    .into_any_element()
                            })),
                    )
                    .child(
                        // Right: the selected run's output.
                        div()
                            .id("tools-output")
                            .flex_1()
                            .overflow_y_scroll()
                            .p(px(12.))
                            .child(
                                div()
                                    .font_family(theme::MONO_FONT)
                                    .text_size(px(10.5))
                                    .whitespace_normal()
                                    .text_color(theme::color(theme::TEXT_DIM))
                                    .child(if let Some(run) = selected_run {
                                        let mut text = format!(
                                            "$ {}\n—— {}\n\n",
                                            run.command, run.title
                                        );
                                        text.push_str(&run.output);
                                        if let Some(code) = run.exit_code {
                                            text.push_str(&format!("\n[退出码 {code}]"));
                                        }
                                        text
                                    } else {
                                        "选择左侧一次运行查看输出。".to_string()
                                    }),
                            ),
                    ),
            )
    }
}
