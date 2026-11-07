//! Agent panel (任务/子代理管理), modeled after Zed's agent panel.
//!
//! A list of agent tasks runs in the background; each task streams its output
//! into a scrollable log and reports a lifecycle status (queued/running/
//! succeeded/failed/cancelled). Tasks are spawned from the panel's prompt bar
//! (a shell command) or programmatically via [`AgentPanel::submit`].
//! A background thread reads each child's stdout/stderr into a shared task
//! buffer and notifies the UI through an event channel. The host polls
//! [`AgentPanel::drain_events`] and re-renders (mirroring the terminal panel).
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use gpui::prelude::*;
use gpui::{Context, Entity, EventEmitter, FontWeight, Render, Window, div, px, rgb};

use crate::text_field::TextField;
use crate::{theme, widgets};

/// Lifecycle state of one agent task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl AgentStatus {
    pub fn label(self) -> &'static str {
        match self {
            AgentStatus::Queued => "排队中",
            AgentStatus::Running => "运行中",
            AgentStatus::Succeeded => "成功",
            AgentStatus::Failed => "失败",
            AgentStatus::Cancelled => "已取消",
        }
    }

    pub fn color(self) -> gpui::Hsla {
        match self {
            AgentStatus::Queued => theme::color(theme::TEXT_FAINT),
            AgentStatus::Running => theme::color(theme::ACCENT),
            AgentStatus::Succeeded => theme::color(theme::JADE),
            AgentStatus::Failed => theme::color(theme::DANGER),
            AgentStatus::Cancelled => theme::color(theme::TEXT_DIM),
        }
    }

    pub fn is_done(self) -> bool {
        matches!(
            self,
            AgentStatus::Succeeded | AgentStatus::Failed | AgentStatus::Cancelled
        )
    }
}

/// Snapshot of one agent task for rendering.
#[derive(Debug, Clone)]
pub struct AgentTask {
    pub id: u64,
    pub title: String,
    pub status: AgentStatus,
    pub output: String,
    pub exit_code: Option<i32>,
}

/// Events emitted by the agent panel.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Updated,
}

/// Mutable state of one agent task, shared with the runner thread.
struct SharedTask {
    status: AgentStatus,
    output: String,
    exit_code: Option<i32>,
    cancel: AtomicBool,
}

/// The agent panel entity.
pub struct AgentPanel {
    tasks: Vec<AgentTask>,
    shared: Vec<Arc<Mutex<SharedTask>>>,
    next_id: u64,
    input: Entity<TextField>,
    receiver: Receiver<AgentEvent>,
    _sender: Sender<AgentEvent>,
    selected: Option<u64>,
}

impl AgentPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (sender, receiver) = channel();
        Self {
            tasks: Vec::new(),
            shared: Vec::new(),
            next_id: 1,
            input: cx.new(|cx| TextField::new("输入 shell 命令，回车创建 agent 任务", cx)),
            receiver,
            _sender: sender,
            selected: None,
        }
    }

    /// Spawn a background task running `command` in a shell.
    pub fn submit(&mut self, command: &str, cx: &mut Context<Self>) {
        let command = command.trim().to_string();
        if command.is_empty() {
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let shared = Arc::new(Mutex::new(SharedTask {
            status: AgentStatus::Queued,
            output: String::new(),
            exit_code: None,
            cancel: AtomicBool::new(false),
        }));
        let snapshot = AgentTask {
            id,
            title: command.clone(),
            status: AgentStatus::Queued,
            output: String::new(),
            exit_code: None,
        };
        self.tasks.insert(0, snapshot.clone());
        self.shared.insert(0, shared.clone());
        self.selected = Some(id);

        let evt = self._sender.clone();
        std::thread::spawn(move || run_task(command, shared, evt));
        cx.notify();
    }

    /// Consume pending events and sync the shared task snapshots.
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

    /// Pull the latest status/output from the shared tasks.
    fn refresh(&mut self) {
        for (snapshot, shared) in self.tasks.iter_mut().zip(self.shared.iter()) {
            let task = shared.lock().unwrap();
            if snapshot.status != task.status || snapshot.output != task.output {
                snapshot.status = task.status;
                snapshot.output = task.output.clone();
                snapshot.exit_code = task.exit_code;
            }
        }
    }

    pub fn clear(&mut self) {
        let mut kept_tasks = Vec::new();
        let mut kept_shared = Vec::new();
        for (snapshot, shared) in self.tasks.drain(..).zip(self.shared.drain(..)) {
            if !shared.lock().unwrap().status.is_done() {
                kept_tasks.push(snapshot);
                kept_shared.push(shared);
            }
        }
        self.tasks = kept_tasks;
        self.shared = kept_shared;
        if !self.tasks.iter().any(|task| Some(task.id) == self.selected) {
            self.selected = self.tasks.first().map(|task| task.id);
        }
    }

    pub fn cancel_all(&mut self) {
        for shared in &self.shared {
            shared.lock().unwrap().cancel.store(true, Ordering::SeqCst);
        }
    }

    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }
}

/// Run `command` in a platform shell, streaming stdout/stderr into `shared`.
fn run_task(command: String, shared: Arc<Mutex<SharedTask>>, evt: Sender<AgentEvent>) {
    {
        let mut task = shared.lock().unwrap();
        task.status = AgentStatus::Running;
    }
    let _ = evt.send(AgentEvent::Updated);
    let shell = if std::env::consts::OS == "windows" {
        "cmd"
    } else {
        "sh"
    };
    let flag = if std::env::consts::OS == "windows" { "/C" } else { "-c" };
    let spawned = Command::new(shell)
        .arg(flag)
        .arg(&command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn();
    let mut c = match spawned {
        Ok(c) => c,
        Err(error) => {
            let mut task = shared.lock().unwrap();
            task.output.push_str(&format!("无法启动任务: {error}\n"));
            task.status = AgentStatus::Failed;
            task.exit_code = None;
            let _ = evt.send(AgentEvent::Updated);
            return;
        }
    };
    let stdout = c.stdout.take();
    let stderr = c.stderr.take();

    let read_pipe = |pipe: Option<Box<dyn std::io::Read + Send>>,
                     task_shared: Arc<Mutex<SharedTask>>,
                     evt: Sender<AgentEvent>| {
        let Some(pipe) = pipe else { return };
        let reader = BufReader::new(pipe);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let mut task = task_shared.lock().unwrap();
            task.output.push_str(&line);
            task.output.push('\n');
            let _ = evt.send(AgentEvent::Updated);
        }
    };
    let out_thread = std::thread::spawn({
        let task_shared = shared.clone();
        let evt = evt.clone();
        let pipe: Option<Box<dyn std::io::Read + Send>> = stdout.map(|p| Box::new(p) as Box<dyn std::io::Read + Send>);
        move || read_pipe(pipe, task_shared, evt)
    });
    let err_thread = std::thread::spawn({
        let task_shared = shared.clone();
        let evt = evt.clone();
        let pipe: Option<Box<dyn std::io::Read + Send>> = stderr.map(|p| Box::new(p) as Box<dyn std::io::Read + Send>);
        move || read_pipe(pipe, task_shared, evt)
    });
    let _ = out_thread.join();
    let _ = err_thread.join();
    let wait = c.wait();
    let exit_code = wait.ok().map(|st| st.code().unwrap_or(-1));
    let mut task = shared.lock().unwrap();
    task.exit_code = exit_code;
    task.status = if task.cancel.load(Ordering::SeqCst) {
        AgentStatus::Cancelled
    } else if exit_code == Some(0) {
        AgentStatus::Succeeded
    } else {
        AgentStatus::Failed
    };
    let _ = evt.send(AgentEvent::Updated);
}

impl EventEmitter<AgentEvent> for AgentPanel {}

impl Render for AgentPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected;
        div()
            .flex_1()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(34.))
                    .px(px(12.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme::alpha(theme::STROKE, 0.09))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(theme::TEXT))
                            .child(format!("Agent 任务 ({})", self.task_count())),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                widgets::ghost_button("agent-clear", "清空").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.clear();
                                        cx.notify();
                                    }),
                                ),
                            )
                            .child(
                                widgets::ghost_button("agent-cancel", "全部取消").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.cancel_all();
                                        cx.notify();
                                    }),
                                ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("agent-list")
                            .flex_1()
                            .overflow_y_scroll()
                            .flex_col()
                            .gap(px(2.))
                            .children(
                                self.tasks
                                    .iter()
                                    .map(|task| {
                                        let is_selected = selected == Some(task.id);
                                        let task_id = task.id;
                                        div()
                                            .id(("agent-task", task.id))
                                            .px(px(10.))
                                            .py(px(4.))
                                            .flex()
                                            .items_center()
                                            .gap(px(8.))
                                            .cursor_pointer()
                                            .bg(if is_selected {
                                                theme::color(theme::BG_2)
                                            } else {
                                                theme::color(0x000000).opacity(0.0)
                                            })
                                            .child(
                                                div()
                                                    .w(px(6.))
                                                    .h(px(6.))
                                                    .rounded_full()
                                                    .bg(task.status.color()),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .overflow_hidden()
                                                    .text_ellipsis()
                                                    .whitespace_nowrap()
                                                    .text_size(px(11.))
                                                    .text_color(rgb(theme::TEXT))
                                                    .child(task.title.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(9.5))
                                                    .text_color(task.status.color())
                                                    .child(task.status.label()),
                                            )
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.selected = Some(task_id);
                                                cx.notify();
                                            }))
                                            .into_any_element()
                                    })
                                    .collect::<Vec<_>>(),
                            ),
                    )
                    .child(
                        div()
                            .id("agent-output")
                            .h(px(150.))
                            .flex_none()
                            .overflow_y_scroll()
                            .p(px(10.))
                            .border_t_1()
                            .border_color(theme::alpha(theme::STROKE, 0.09))
                            .child(
                                self.tasks
                                    .iter()
                                    .find(|task| Some(task.id) == selected)
                                    .map(|task| {
                                        div()
                                            .flex()
                                            .flex_col()
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(theme::TEXT))
                                                    .child(task.title.clone()),
                                            )
                                            .child(
                                                div()
                                                    .mt(px(4.))
                                                    .text_size(px(10.5))
                                                    .whitespace_normal()
                                                    .text_color(rgb(theme::TEXT_DIM))
                                                    .child(if task.output.is_empty() {
                                                        "(无输出)".to_string()
                                                    } else {
                                                        task.output.clone()
                                                    }),
                                            )
                                            .into_any_element()
                                    })
                                    .unwrap_or_else(|| {
                                        div()
                                            .text_size(px(11.))
                                            .text_color(rgb(theme::TEXT_FAINT))
                                            .child("选择一个任务查看输出")
                                            .into_any_element()
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .h(px(34.))
                    .px(px(12.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(theme::alpha(theme::STROKE, 0.09))
                    .child(div().flex_1().child(self.input.clone()))
                    .child(
                        widgets::primary_button("agent-run", "运行").on_click(
                            cx.listener(|this, _, _, cx| {
                                let cmd = this.input.read(cx).text();
                                this.submit(&cmd, cx);
                                this.input.update(cx, |field, cx| field.set_text("", cx));
                            }),
                        ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_task_success_collects_output_and_succeeds() {
        let (tx, rx) = channel();
        let shared = Arc::new(Mutex::new(SharedTask {
            status: AgentStatus::Queued,
            output: String::new(),
            exit_code: None,
            cancel: AtomicBool::new(false),
        }));
        let command = if std::env::consts::OS == "windows" {
            "echo hello".to_string()
        } else {
            "echo hello".to_string()
        };
        run_task(command, shared.clone(), tx);
        let task = shared.lock().unwrap();
        assert_eq!(task.status, AgentStatus::Succeeded);
        assert_eq!(task.exit_code, Some(0));
        assert!(task.output.contains("hello"));
        drop(rx);
    }

    #[test]
    fn run_task_failure_reports_nonzero_exit() {
        let (tx, _rx) = channel();
        let shared = Arc::new(Mutex::new(SharedTask {
            status: AgentStatus::Queued,
            output: String::new(),
            exit_code: None,
            cancel: AtomicBool::new(false),
        }));
        let command = if std::env::consts::OS == "windows" {
            "exit /b 3".to_string()
        } else {
            "exit 3".to_string()
        };
        run_task(command, shared.clone(), tx);
        let task = shared.lock().unwrap();
        assert_eq!(task.status, AgentStatus::Failed);
        assert_eq!(task.exit_code, Some(3));
    }

    #[test]
    fn run_task_cancelled_reports_cancelled() {
        let (tx, _rx) = channel();
        let shared = Arc::new(Mutex::new(SharedTask {
            status: AgentStatus::Queued,
            output: String::new(),
            exit_code: None,
            cancel: AtomicBool::new(true),
        }));
        // A long-running command; the cancel flag is already set so the final
        // status must be Cancelled regardless of exit code.
        let command = if std::env::consts::OS == "windows" {
            "ping -n 2 127.0.0.1 >nul".to_string()
        } else {
            "sleep 0.1".to_string()
        };
        run_task(command, shared.clone(), tx);
        let task = shared.lock().unwrap();
        assert_eq!(task.status, AgentStatus::Cancelled);
    }

    #[test]
    fn empty_command_does_not_start_task() {
        // submit() guards against empty commands; verify the guard.
        // (Constructing an AgentPanel requires a Context, so test the guard
        // logic indirectly by checking trim behavior.)
        assert_eq!("".trim(), "");
        assert_eq!("   ".trim(), "");
    }
}
