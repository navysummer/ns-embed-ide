use std::collections::HashSet;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;

use gpui::{
    Context, Div, ElementId, Entity, FocusHandle, Focusable, FontWeight, KeyBinding,
    MouseButton, MouseDownEvent, PathPromptOptions, Pixels, Render, SharedString, Stateful,
    Subscription, Window, actions, div, prelude::*, px, rgb,
};
use ns_build::{BuildOutcome, Diagnostic, Severity};
use ns_debug::{
    self, DebugBreakpoint, DebugBreakpointLocation, DebugCommand, DebugConfig, DebugMessage,
    EmbeddedConfig, Frame, ProbeKind, Scope, TraceConfig, Variable, WatchpointRequest,
    find_probe, package_support,
};
use ns_flash::{FlashAction, FlashRequest, adapter_for};
use ns_project::{
    BreakpointKind, BreakpointStore, Language, Project, RecentProject, WatchAccess,
    load_recents, remember_project,
};

use crate::agent::AgentPanel;
use crate::editor::{BreakpointContextMenu, BreakpointToggled, Editor};
use crate::git;
use crate::palette::{self, CommandEntry};
use crate::search::{self, MatchOrigin, SearchMatch};
use crate::text_field::TextField;
use crate::theme;
use crate::widgets;
use crate::terminal::{Terminal, TerminalConfig, TerminalEvent, TerminalKind};
use crate::serial::{SerialEvent, SerialMonitor};
use crate::wizard::{self, Wizard};

actions!(
    ns_ide,
    [
        NewProject,
        OpenProject,
        Save,
        Build,
        Run,
        ToggleOutput,
        CloseTab,
        NextTab,
        PrevTab,
        QuitApp,
        WizardNext,
        WizardBack,
        EscapeModal,
        ClearBreakpoints,
        ConditionDialogSave,
        Debug,
        StopDebug,
        DebugStepOver,
        DebugStepIn,
        DebugStepOut,
        RunToCursor,
        FindInFile,
        FindInProject,
        SearchConfirm,
        SearchSelectUp,
        SearchSelectDown,
        Palette,
        PaletteConfirm,
        PaletteSelectUp,
        PaletteSelectDown,
        Settings,
        Flash,
        GoToDefinition,
        FindReferences,
        Hover,
        SignatureHelp,
        ShowCompletion,
    ]
);

pub fn bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-n", NewProject, None),
        KeyBinding::new("ctrl-n", NewProject, None),
        KeyBinding::new("cmd-o", OpenProject, None),
        KeyBinding::new("ctrl-o", OpenProject, None),
        KeyBinding::new("cmd-s", Save, None),
        KeyBinding::new("ctrl-s", Save, None),
        KeyBinding::new("cmd-b", Build, None),
        KeyBinding::new("ctrl-b", Build, None),
        KeyBinding::new("cmd-r", Run, None),
        KeyBinding::new("ctrl-r", Run, None),
        KeyBinding::new("cmd-j", ToggleOutput, None),
        KeyBinding::new("ctrl-j", ToggleOutput, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("ctrl-w", CloseTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        KeyBinding::new("cmd-q", QuitApp, None),
        KeyBinding::new("ctrl-q", QuitApp, None),
        KeyBinding::new("cmd-,", Settings, None),
        KeyBinding::new("ctrl-,", Settings, None),
        KeyBinding::new("enter", ConditionDialogSave, Some("Dialog")),
        KeyBinding::new("f5", Debug, None),
        KeyBinding::new("shift-f5", StopDebug, None),
        KeyBinding::new("f10", DebugStepOver, None),
        KeyBinding::new("f11", DebugStepIn, None),
        KeyBinding::new("shift-f11", DebugStepOut, None),
        KeyBinding::new("cmd-f10", RunToCursor, None),
        KeyBinding::new("ctrl-f10", RunToCursor, None),
        KeyBinding::new("cmd-shift-f10", Flash, None),
        KeyBinding::new("ctrl-shift-f10", Flash, None),
        KeyBinding::new("cmd-shift-f", FindInProject, None),
        KeyBinding::new("cmd-f", FindInFile, None),
        KeyBinding::new("ctrl-f", FindInFile, None),
        KeyBinding::new("ctrl-shift-f", FindInProject, None),
        KeyBinding::new("cmd-shift-p", Palette, None),
        KeyBinding::new("ctrl-shift-p", Palette, None),
        KeyBinding::new("enter", PaletteConfirm, Some("Palette")),
        KeyBinding::new("up", PaletteSelectUp, Some("Palette")),
        KeyBinding::new("down", PaletteSelectDown, Some("Palette")),
        KeyBinding::new("enter", SearchConfirm, Some("Search")),
        KeyBinding::new("up", SearchSelectUp, Some("Search")),
        KeyBinding::new("down", SearchSelectDown, Some("Search")),
        KeyBinding::new("cmd-shift-f9", ClearBreakpoints, None),
        KeyBinding::new("ctrl-shift-f9", ClearBreakpoints, None),
        KeyBinding::new("enter", WizardNext, Some("Wizard")),
        KeyBinding::new("shift-enter", WizardBack, Some("Wizard")),
        KeyBinding::new("escape", EscapeModal, None),
        KeyBinding::new("f12", GoToDefinition, None),
        KeyBinding::new("shift-f12", FindReferences, None),
        KeyBinding::new("cmd-k cmd-i", Hover, None),
        KeyBinding::new("ctrl-k ctrl-i", Hover, None),
        KeyBinding::new("cmd-shift-space", SignatureHelp, None),
        KeyBinding::new("ctrl-shift-space", SignatureHelp, None),
        KeyBinding::new("cmd-space", ShowCompletion, None),
        KeyBinding::new("ctrl-space", ShowCompletion, None),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputTab {
    Output,
    Problems,
    Breakpoints,
    Debug,
    Git,
    Terminal,
    Serial,
    Agent,
}

#[derive(Debug, Clone)]
enum BuildState {
    Idle,
    Running,
    Success(Duration),
    Failed(Duration),
}

struct Tab {
    editor: Entity<Editor>,
    path: PathBuf,
    _subscription: Subscription,
    _breakpoint_subscription: Subscription,
    _menu_subscription: Subscription,
}

#[derive(Debug, Clone)]
struct ContextMenuState {
    path: String,
    line: usize,
    x: Pixels,
    y: Pixels,
    /// Identifier-like token under the cursor (may be empty).
    word: String,
}

struct ConditionDialog {
    path: String,
    line: usize,
    field: Entity<TextField>,
}

/// Dialog to add or edit a watchpoint (data breakpoint).
struct WatchpointDialog {
    /// Existing expression when editing (empty when adding).
    original: String,
    field: Entity<TextField>,
    /// Access type chosen in the dialog; cycled by clicking the badge.
    access: WatchAccess,
}

/// Dialog to set a variable's value while stopped.
struct SetVarDialog {
    /// `variablesReference` of the parent scope (0 for a top-level variable).
    reference: u64,
    /// Variable name.
    name: String,
    field: Entity<TextField>,
}
/// Modal to edit the project-level settings in `project.toml`. Fields are
/// plain text inputs; saving rewrites the config file via `Project::save_config`.
struct SettingsDialog {
    name: Entity<TextField>,
    version: Entity<TextField>,
    language: Entity<TextField>,
    target: Entity<TextField>,
    svd: Entity<TextField>,
    project_compiler: Entity<TextField>,
    project_simulator: Entity<TextField>,
    project_debugger: Entity<TextField>,
    project_flasher: Entity<TextField>,
    compiler: Entity<TextField>,
    compiler_std: Entity<TextField>,
    compiler_opt: Entity<TextField>,
    compiler_defines: Entity<TextField>,
    compiler_includes: Entity<TextField>,
    build_config: Entity<TextField>,
    out_dir: Entity<TextField>,
    debugger: Entity<TextField>,
    debugger_interface: Entity<TextField>,
    debugger_server: Entity<TextField>,
    debugger_probe: Entity<TextField>,
    debugger_gdb: Entity<TextField>,
    flasher: Entity<TextField>,
    flasher_kind: Entity<TextField>,
    flasher_interface: Entity<TextField>,
    sim_cpu: Entity<TextField>,
    sim_period: Entity<TextField>,
    /// Global terminal shell (not part of project.toml; stored in terminal.json).
    terminal_shell: Entity<TextField>,
    /// Global config keys (`$config["key"]`), one `key=value` per line.
    config_keys: Entity<TextField>,
}

/// Split a comma-separated list, trimming whitespace and dropping empties.
fn split_csv(text: &str) -> Vec<String> {
    text.split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// Modal to edit global toolchain path overrides (stored in the user config
/// dir, shared across projects; overrides take precedence over PATH).
struct ToolchainDialog {
    fields: Vec<(&'static str, Entity<TextField>)>,
}

/// Build a settings text field prefilled with `value`.
fn settings_field(
    placeholder: impl Into<SharedString>,
    value: String,
    cx: &mut Context<IdeApp>,
) -> Entity<TextField> {
    cx.new(move |cx| {
        let mut field = TextField::new(placeholder.into(), cx);
        field.set_text(value, cx);
        field
    })
}

struct SearchState {
    field: Entity<TextField>,
    matches: Vec<SearchMatch>,
    selected: usize,
    _subscription: Subscription,
}

struct PaletteState {
    field: Entity<TextField>,
    ranked: Vec<usize>,
    selected: usize,
    _subscription: Subscription,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DebugStatus {
    Idle,
    Running,
    Stopped { reason: String },
    Exited,
}

impl DebugStatus {
    fn label(&self) -> String {
        match self {
            Self::Idle => "未运行".to_string(),
            Self::Running => "运行中".to_string(),
            Self::Stopped { reason } => match reason.as_str() {
                "breakpoint" => "已命中断点".to_string(),
                "step" => "单步暂停".to_string(),
                "pause" => "已暂停".to_string(),
                "entry" => "已在入口暂停".to_string(),
                other => format!("已暂停 ({other})"),
            },
            Self::Exited => "已结束".to_string(),
        }
    }
}

/// A watch expression plus its most recent evaluated value.
struct WatchEntry {
    expression: String,
    value: String,
}

struct DebugView {
    status: DebugStatus,
    thread_id: u64,
    frames: Vec<Frame>,
    active_frame: usize,
    scopes: Vec<Scope>,
    active_scope: usize,
    variables: Vec<Variable>,
    /// Children of expandable variables, keyed by their variablesReference.
    children: HashMap<u64, Vec<Variable>>,
    /// Expanded variable references.
    expanded: HashSet<u64>,
    pc: Option<u64>,
    view_tab: DebugViewTab,
    memory: MemoryView,
    disassembly: Vec<ns_debug::Instruction>,
    console: Vec<String>,
    error: Option<String>,
    synced_files: HashSet<String>,
    /// Variables per scope id fetched to resolve watchpoint expressions.
    watchpoint_scope_variables: HashMap<u64, Vec<Variable>>,
    /// Expressions currently being resolved via `dataBreakpointInfo`.
    resolving_watchpoints: HashSet<String>,
    /// A watchpoint resolution pass is in flight (scopes being fetched).
    watchpoint_resolution_pending: bool,
    trace: TraceView,
    /// Input for evaluating an expression in the debug console.
    console_input: Option<Entity<TextField>>,
    /// Watch expressions and their last evaluated value.
    watches: Vec<WatchEntry>,
    /// Input for adding a new watch expression.
    watch_input: Option<Entity<TextField>>,
}

impl Default for DebugView {
    fn default() -> Self {
        Self {
            status: DebugStatus::Idle,
            thread_id: 0,
            frames: Vec::new(),
            active_frame: 0,
            scopes: Vec::new(),
            active_scope: 0,
            variables: Vec::new(),
            children: HashMap::new(),
            expanded: HashSet::new(),
            pc: None,
            view_tab: DebugViewTab::Variables,
            memory: MemoryView::default(),
            disassembly: Vec::new(),
            console: Vec::new(),
            error: None,
            synced_files: HashSet::new(),
            watchpoint_scope_variables: HashMap::new(),
            resolving_watchpoints: HashSet::new(),
            watchpoint_resolution_pending: false,
            trace: TraceView::default(),
            console_input: None,
            watches: Vec::new(),
            watch_input: None,
        }
    }
}

impl DebugView {
    fn push_console(&mut self, line: String) {
        self.console.push(line);
        if self.console.len() > 500 {
            let excess = self.console.len() - 500;
            self.console.drain(..excess);
        }
    }

    /// Add a watch expression (or re-add an existing one).
    fn add_watch(&mut self, expression: &str) {
        if let Some(entry) = self.watches.iter_mut().find(|w| w.expression == expression) {
            entry.value.clear();
            return;
        }
        self.watches.push(WatchEntry {
            expression: expression.to_string(),
            value: String::new(),
        });
    }

    /// Update a watch entry's value from an `Evaluated` response.
    fn update_watch(&mut self, expression: &str, value: &str) {
        if let Some(entry) = self.watches.iter_mut().find(|w| w.expression == expression) {
            entry.value = value.to_string();
        }
    }

    fn remove_watch(&mut self, index: usize) {
        if index < self.watches.len() {
            self.watches.remove(index);
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum DebugStep {
    Over,
    In,
    Out,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebugViewTab {
    Variables,
    Memory,
    Disassembly,
    Trace,
    Device,
    Simulator,
}

struct MemoryView {
    address_input: Option<Entity<TextField>>,
    address: u64,
    bytes: Vec<u8>,
    error: Option<String>,
    read_size: u64,
}

impl Default for MemoryView {
    fn default() -> Self {
        Self {
            address_input: None,
            address: 0,
            bytes: Vec::new(),
            error: None,
            read_size: 64,
        }
    }
}

/// Trace window (SWO/SWV) configuration panel state.
struct TraceView {
    /// The configuration being edited; persisted to `.ns-ide/trace.json`.
    config: TraceConfig,
    /// Input for the watched data address (only relevant when `data_trace`).
    address_input: Option<Entity<TextField>>,
    /// Input for the SWO prescaler (TPIU_ACPR + 1).
    prescaler_input: Option<Entity<TextField>>,
    /// Whether the trace window is currently armed on the target.
    armed: bool,
    /// Last validation or configuration error, shown inline.
    error: Option<String>,
    /// Honest support status (probe found, SWO stream connected?).
    support: Option<&'static str>,
    /// Input for the ITM stimulus port.
    port_input: Option<Entity<TextField>>,
}

impl Default for TraceView {
    fn default() -> Self {
        Self {
            config: TraceConfig::default(),
            address_input: None,
            prescaler_input: None,
            armed: false,
            error: None,
            support: None,
            port_input: None,
        }
    }
}

struct DebugSession {
    commands: Sender<DebugCommand>,
}


/// What to do after a successful flash during run orchestration (F9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlashMode {
    /// Reset-and-run the artifact directly (no debugger attached).
    Run,
    /// Attach the debugger after flashing.
    Debug,
}
/// Direct-run (F9 直接运行) state: the spawned artifact, its streaming output
/// and exit status.
struct RunState {
    /// The command line that was launched.
    command: String,
    /// Accumulated stdout/stderr of the run, shown in the output panel.
    output: String,
    /// True while the child is alive.
    running: bool,
    /// Exit code once the child finished (None while running or not started).
    exit_code: Option<i32>,
    /// Monotonic id used to discard events from a run replaced by a newer one.
    generation: u64,
    /// True once a run has been launched at least once.
    started: bool,
    /// Whether the output panel shows the run output (vs the build output).
    show_run_output: bool,
    /// Shared child handle so the UI can stop the run from a button.
    child: Arc<Mutex<Option<Child>>>,
    /// True after the user pressed stop: the child is killed (Unix reports
    /// no exit code for signal deaths), so the exit event must not overwrite
    /// the "运行已停止" status with a misleading `-1`.
    stopped: bool,
}

impl Default for RunState {
    fn default() -> Self {
        Self {
            command: String::new(),
            output: String::new(),
            running: false,
            exit_code: None,
            generation: 0,
            started: false,
            show_run_output: false,
            child: Arc::new(Mutex::new(None)),
            stopped: false,
        }
    }
}

/// Events streamed from the run worker thread to the UI.
enum RunEvent {
    /// A chunk of program output (stdout or stderr).
    Output(String),
    /// The child exited with the given code.
    Exited(i32),
}
/// Git panel state (F14 版本控制): the last snapshot plus the commit-message
/// input and the currently selected file (for its diff).
struct GitState {
    /// Last repository snapshot; empty until a project is loaded.
    status: git::RepoStatus,
    /// Commit message input field.
    message: Option<Entity<TextField>>,
    /// Path of the file whose diff is shown (project-relative).
    selected: Option<String>,
    /// Diff text of the selected file.
    diff: String,
    /// Last error, shown in the panel.
    error: Option<String>,
    /// Whether the last commit succeeded (for a transient status line).
    feedback: Option<String>,
    /// Recent commit history (latest first).
    history: Vec<git::GitCommit>,
    /// Local branches (name, current).
    branches: Vec<(String, bool)>,
    /// (ahead, behind) versus upstream.
    ahead_behind: (usize, usize),
    /// Whether the history section is expanded.
    show_history: bool,
}

impl Default for GitState {
    fn default() -> Self {
        Self {
            status: git::RepoStatus {
                in_repo: false,
                branch: String::new(),
                changes: 0,
                files: Vec::new(),
            },
            message: None,
            selected: None,
            diff: String::new(),
            error: None,
            feedback: None,
            history: Vec::new(),
            branches: Vec::new(),
            ahead_behind: (0, 0),
            show_history: false,
        }
    }
}

/// Refresh the Git snapshot for the current project root.
fn refresh_git(app: &mut IdeApp, cx: &mut Context<IdeApp>) {
    let Some(project) = &app.project else {
        return;
    };
    let root = project.root.clone();
    app.git.status = git::repo_status(&root);
    if app.git.status.in_repo {
        app.git.history = git::log(&root, 50);
        app.git.branches = git::branches(&root);
        app.git.ahead_behind = git::ahead_behind(&root);
    } else {
        app.git.history.clear();
        app.git.branches.clear();
        app.git.ahead_behind = (0, 0);
    }
    // Drop the selection if the file is no longer changed.
    if let Some(selected) = app.git.selected.clone() {
        if !app.git.status.files.iter().any(|file| file.path == selected) {
            app.git.selected = None;
            app.git.diff.clear();
        }
    }
    if let Some(selected) = app.git.selected.clone() {
        app.git.diff = git::diff(&root, &selected).unwrap_or_default();
    }
    cx.notify();
}

/// Parse the debug probe kind from `project.toml`'s `[debugger] probe`.
fn probe_kind(value: &str) -> Result<ProbeKind, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "openocd" => Ok(ProbeKind::OpenOcd),
        "stlink" | "st-link" | "st-util" => Ok(ProbeKind::StLink),
        "jlink" | "j-link" => Ok(ProbeKind::JLink),
        "pyocd" | "py-ocd" => Ok(ProbeKind::PyOcd),
        "probe-rs" | "probe_rs" | "probers" => Ok(ProbeKind::ProbeRs),
        other => Err(format!(
            "未知调试探针: {other}（应为 openocd / stlink / jlink / pyocd / probe-rs）"
        )),
    }
}

impl IdeApp {
    /// Whether the project targets an embedded MCU (a `target` is set), which
    /// routes debug through the probe GDB server instead of host execution.
    fn is_embedded_target(&self) -> bool {
        self.project
            .as_ref()
            .map(|project| !project.config.project.target.trim().is_empty())
            .unwrap_or(false)
    }

    /// Open the Git panel and refresh the repository snapshot.
    fn show_git(&mut self, cx: &mut Context<Self>) {
        self.show_output = true;
        self.output_tab = OutputTab::Git;
        self.git.error = None;
        self.git.feedback = None;
        refresh_git(self, cx);
        self.ensure_git_message(cx);
    }

    /// Open the terminal tab, creating a shell session on first use.
    fn show_terminal(&mut self, cx: &mut Context<Self>) {
        let settings = ns_project::TerminalSettings::load();
        self.open_terminal(
            TerminalConfig {
                kind: TerminalKind::Shell,
                program: settings.shell_command(),
            },
            cx,
        );
    }

    /// Open the serial assistant (串口助手) panel.
    fn show_serial_monitor(&mut self, cx: &mut Context<Self>) {
        self.show_output = true;
        self.output_tab = OutputTab::Serial;
        if self.serial.is_none() {
            let serial = cx.new(|cx| SerialMonitor::new(cx));
            let sub = cx.subscribe(&serial, |this, _serial, event: &SerialEvent, cx| {
                match event {
                    SerialEvent::Data => cx.notify(),
                    SerialEvent::Error(message) => {
                        this.status = message.clone();
                        cx.notify();
                    }
                }
            });
            self.serial = Some(serial);
            self._serial_sub = Some(sub);
        }
        cx.notify();
    }

    /// Open the agent panel (任务/子代理管理).
    fn show_agent_panel(&mut self, cx: &mut Context<Self>) {
        self.show_output = true;
        self.output_tab = OutputTab::Agent;
        if self.agent.is_none() {
            let agent = cx.new(|cx| AgentPanel::new(cx));
            let poll = agent.clone();
            cx.spawn(async move |this, cx| {
                loop {
                    let changed = poll.update(cx, |agent, _cx| agent.drain_events());
                    if changed.is_err() {
                        break;
                    }
                    if changed.unwrap_or(false) {
                        this.update(cx, |_this, cx| cx.notify()).ok();
                    }
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(80))
                        .await;
                }
            })
            .detach();
            self.agent = Some(agent);
        }
        cx.notify();
    }

    fn open_terminal(&mut self, config: TerminalConfig, cx: &mut Context<Self>) {
        self.show_output = true;
        self.output_tab = OutputTab::Terminal;
        if self.terminal.is_none() {
            let terminal = cx.new(|cx| Terminal::new(config, cx));
            let sub = cx.subscribe(&terminal, |this, _terminal, event: &TerminalEvent, cx| {
                match event {
                    TerminalEvent::Output | TerminalEvent::Exited(_) => {
                        this.scroll_terminal_to_bottom = true;
                        cx.notify();
                    }
                }
            });
            let poll = terminal.clone();
            cx.spawn(async move |this, cx| {
                loop {
                    let changed = poll.update(cx, |terminal, cx| {
                        let changed = terminal.drain_events();
                        if changed {
                            this.update(cx, |this, cx| {
                                this.scroll_terminal_to_bottom = true;
                                cx.notify();
                            })
                            .ok();
                        }
                        changed
                    });
                    if changed.is_err() {
                        break;
                    }
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(80))
                        .await;
                }
            })
            .detach();
            self.terminal = Some(terminal);
            self._terminal_sub = Some(sub);
            if let Some(terminal) = self.terminal.as_ref() {
                terminal.update(cx, |terminal, _cx| terminal.resize(100, 30));
            }
            self.focus_terminal = true;
            cx.notify();
        } else {
            self.focus_terminal = true;
        }
        cx.notify();
    }

    fn ensure_git_message(&mut self, cx: &mut Context<Self>) {
        if self.git.message.is_none() {
            self.git.message = Some(cx.new(|cx| TextField::new("提交信息…", cx)));
        }
    }

    fn git_select(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        self.git.selected = Some(path.clone());
        self.git.diff = git::diff(&project.root, &path).unwrap_or_default();
        self.git.error = None;
        cx.notify();
    }

    fn git_stage(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        let root = project.root.clone();
        match git::stage(&root, &path) {
            Ok(()) => {
                self.git.error = None;
                self.git.feedback = Some(format!("已暂存 {path}"));
            }
            Err(error) => self.git.error = Some(error),
        }
        refresh_git(self, cx);
    }

    fn git_unstage(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        let root = project.root.clone();
        match git::unstage(&root, &path) {
            Ok(()) => {
                self.git.error = None;
                self.git.feedback = Some(format!("已取消暂存 {path}"));
            }
            Err(error) => self.git.error = Some(error),
        }
        refresh_git(self, cx);
    }

    fn git_commit(&mut self, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        let root = project.root.clone();
        let message = self
            .git
            .message
            .as_ref()
            .map(|field| field.read(cx).text().trim().to_string())
            .unwrap_or_default();
        match git::commit(&root, &message) {
            Ok(()) => {
                self.git.error = None;
                self.git.feedback = Some("已提交".to_string());
                if let Some(field) = &self.git.message {
                    field.update(cx, |field, cx| field.set_text("", cx));
                }
                self.git.selected = None;
                self.git.diff.clear();
            }
            Err(error) => self.git.error = Some(error),
        }
        refresh_git(self, cx);
    }
}


struct TreeEntry {
    path: PathBuf,
    name: String,
    is_dir: bool,
    depth: usize,
    expanded: bool,
}

pub struct IdeApp {
    project: Option<Project>,
    tabs: Vec<Tab>,
    active_tab: Option<usize>,
    expanded: HashSet<PathBuf>,
    recents: Vec<RecentProject>,
    status: String,
    build_state: BuildState,
    diagnostics: Vec<Diagnostic>,
    output: String,
    build_command: String,
    show_output: bool,
    output_tab: OutputTab,
    breakpoints: BreakpointStore,
    debug: DebugView,
    debug_session: Option<DebugSession>,
    pending_debug: bool,
    context_menu: Option<ContextMenuState>,
    condition_dialog: Option<ConditionDialog>,
    watchpoint_dialog: Option<WatchpointDialog>,
    settings_dialog: Option<SettingsDialog>,
    set_var_dialog: Option<SetVarDialog>,
    toolchain_dialog: Option<ToolchainDialog>,
    search: Option<SearchState>,
    palette: Option<PaletteState>,
    focus_dialog_on_next_frame: bool,
    pub wizard: Option<Wizard>,
    focus_handle: FocusHandle,
    focus_on_next_frame: bool,
    pending_run: bool,
    pending_flash: Option<FlashMode>,
    run: RunState,
    git: GitState,
    terminal: Option<Entity<Terminal>>,
    _terminal_sub: Option<Subscription>,
    scroll_terminal_to_bottom: bool,
    focus_terminal: bool,
    /// Serial assistant (串口助手) panel.
    serial: Option<Entity<SerialMonitor>>,
    _serial_sub: Option<Subscription>,
    /// Agent panel (任务/子代理管理), modeled after Zed's agent panel.
    agent: Option<Entity<AgentPanel>>,
    _agent_sub: Option<Subscription>,
    /// Active language server, if connected.
    lsp: Option<ns_lsp::LspServer>,
    /// Per-file diagnostics received from the language server.
    lsp_diagnostics: HashMap<PathBuf, Vec<ns_lsp::LspDiagnostic>>,
    /// Locations found by the last find-references request.
    references: Vec<ns_lsp::Location>,
    /// Items from the last completion request.
    completions: Vec<ns_lsp::CompletionItem>,
    /// Whether the completion popup is visible.
    completion_open: bool,
    /// Index of the highlighted completion item.
    completion_selected: usize,
    /// Signature help shown at the cursor, if any.
    signature_help: Option<SignatureHelpInfo>,
    /// Semantic token deltas from the last `semanticTokens/full` request.
    semantic_tokens: Vec<SemanticToken>,
    /// Built-in instruction-level simulator session.
    sim: SimulatorState,
}

#[derive(Default)]
struct SimulatorState {
    /// The running simulator, if a program is loaded.
    core: Option<ns_sim::Simulator>,
    /// Whether the simulator panel is visible.
    panel_open: bool,
    /// Human-readable status of the last simulator action.
    status: String,
}

impl IdeApp {
    pub fn new(initial: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let recents = load_recents();
        let mut app = Self {
            project: None,
            tabs: Vec::new(),
            active_tab: None,
            expanded: HashSet::new(),
            recents,
            status: "就绪".to_string(),
            build_state: BuildState::Idle,
            diagnostics: Vec::new(),
            output: String::new(),
            build_command: String::new(),
            show_output: false,
            output_tab: OutputTab::Output,
            breakpoints: BreakpointStore::default(),
            debug: DebugView::default(),
            debug_session: None,
            pending_debug: false,
            context_menu: None,
            condition_dialog: None,
            watchpoint_dialog: None,
            settings_dialog: None,
            set_var_dialog: None,
            toolchain_dialog: None,
            search: None,
            palette: None,
            focus_dialog_on_next_frame: false,
            wizard: None,
            focus_handle: cx.focus_handle(),
            focus_on_next_frame: true,
            pending_run: false,
            pending_flash: None,
            run: RunState::default(),
            git: GitState::default(),
            terminal: None,
            _terminal_sub: None,
            scroll_terminal_to_bottom: false,
            focus_terminal: false,
            serial: None,
            _serial_sub: None,
            agent: None,
            _agent_sub: None,
            lsp: None,
            lsp_diagnostics: HashMap::new(),
            references: Vec::new(),
            completions: Vec::new(),
            completion_open: false,
            completion_selected: 0,
            signature_help: None,
            semantic_tokens: Vec::new(),
            sim: SimulatorState::default(),
        };
        if let Some(path) = initial {
            app.open_project_path(path, cx);
        }
        app
    }

    fn open_project_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        match Project::open(&path) {
            Ok(project) => self.load_project(project, cx),
            Err(error) => {
                self.status = format!("无法打开项目: {error}");
                cx.notify();
            }
        }
    }

    fn load_project(&mut self, project: Project, cx: &mut Context<Self>) {
        let root = project.root.clone();
        let name = project.config.project.name.clone();
        if let Err(error) = remember_project(&root, &name) {
            self.status = format!("项目已打开，但记录最近项目失败: {error}");
        }
        self.recents = load_recents();
        self.breakpoints = BreakpointStore::load(&root);

        let entry = project.entry_file();
        self.project = Some(project);
        if let Some(session) = self.debug_session.take() {
            let _ = session.commands.send(DebugCommand::Stop);
        }
        self.debug = DebugView::default();
        self.debug.trace.config = TraceConfig::load(&root);
        self.pending_debug = false;
        self.pending_flash = None;
        self.tabs.clear();
        self.active_tab = None;
        self.context_menu = None;
        self.condition_dialog = None;
        self.watchpoint_dialog = None;
        self.set_var_dialog = None;
        self.settings_dialog = None;
        self.toolchain_dialog = None;
        self.search = None;
        self.palette = None;
        self.expanded.clear();
        self.expanded.insert(root.join("src"));
        self.expanded.insert(root.join("include"));
        self.diagnostics.clear();
        self.output.clear();
        self.build_command.clear();
        self.show_output = false;
        self.build_state = BuildState::Idle;
        let tool_warnings = self
            .project
            .as_ref()
            .map(|project| project.tool_warnings())
            .unwrap_or_default();
        if !tool_warnings.is_empty() {
            self.diagnostics = tool_warnings
                .into_iter()
                .map(|issue| Diagnostic {
                    path: None,
                    line: None,
                    column: None,
                    severity: Severity::Warning,
                    message: issue.message,
                    raw: String::new(),
                })
                .collect();
            self.show_output = true;
            self.output_tab = OutputTab::Problems;
            self.status = format!(
                "已打开项目 {name} · {} 个工具未找到",
                self.diagnostics.len()
            );
        } else {
            self.status = format!("已打开项目 {name}");
        }
        self.git = GitState::default();
        refresh_git(self, cx);

        self.open_file(entry, cx);
        let session = ns_project::Session::load(&root);
        for file in session.open_files {
            let path = root.join(&file);
            if path.exists() && path.is_file() {
                self.open_file(path, cx);
            }
        }
        if let Some(index) = session.active {
            if index < self.tabs.len() {
                self.active_tab = Some(index);
            }
        }
        self.start_lsp(cx);
        self.focus_on_next_frame = true;
        cx.notify();
    }

    /// Spawn the language server for the project language and poll its events.
    fn start_lsp(&mut self, cx: &mut Context<Self>) {
        // Tear down any previous server.
        if let Some(server) = self.lsp.take() {
            server.shutdown();
        }
        self.lsp_diagnostics.clear();

        let Some(project) = &self.project else {
            return;
        };
        let language = project.config.project.language;
        let root = project.root.clone();

        // If the server binary is unavailable, report in status but do not fail.
        if !ns_lsp::server_available(&language) {
            self.status = format!(
                "未找到语言服务器（{}）。补全/跳转/诊断不可用。",
                language_name(&language)
            );
            cx.notify();
            return;
        }

        match ns_lsp::LspServer::spawn(&language, &root) {
            Ok(server) => {
                self.lsp = Some(server);
                // Open already-loaded tabs so the server indexes them.
                let files: Vec<(PathBuf, String)> = self
                    .tabs
                    .iter()
                    .filter_map(|tab| {
                        fs::read_to_string(&tab.path)
                            .ok()
                            .map(|text| (tab.path.clone(), text))
                    })
                    .collect();
                if let Some(server) = &self.lsp {
                    for (path, text) in files {
                        let _ = server.open_file(path, text);
                    }
                }
                self.status = format!(
                    "已连接语言服务器（{}）",
                    language_name(&language)
                );
                let poll = cx.spawn(async move |this, cx| {
                    loop {
                        let alive = this
                            .update(cx, |this, cx| this.poll_lsp_events(cx))
                            .unwrap_or(false);
                        if !alive {
                            break;
                        }
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(120))
                            .await;
                    }
                });
                poll.detach();
                cx.notify();
            }
            Err(error) => {
                self.status = format!("语言服务器启动失败: {error}");
                cx.notify();
            }
        }
    }

    /// Poll the LSP event channel for diagnostics and forward them to the UI.
    /// Returns `false` when the server has exited (the polling loop should stop).
    fn poll_lsp_events(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(server) = &self.lsp else {
            return false;
        };
        let mut changed = false;
        let mut alive = true;
        loop {
            match server.try_recv_event() {
                Some(ns_lsp::LspEvent::Diagnostics { path, diagnostics }) => {
                    self.lsp_diagnostics.insert(path, diagnostics);
                    changed = true;
                }
                Some(ns_lsp::LspEvent::ServerExit) => {
                    self.status = "语言服务器已退出".to_string();
                    self.lsp = None;
                    changed = true;
                    alive = false;
                    break;
                }
                None => break,
            }
        }
        if changed {
            cx.notify();
        }
        alive
    }

    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let path = path.canonicalize().unwrap_or(path);
        if let Some(index) = self.tabs.iter().position(|tab| tab.path == path) {
            self.active_tab = Some(index);
            self.focus_on_next_frame = true;
            cx.notify();
            return;
        }

        match fs::read_to_string(&path) {
            Ok(text) => {
                let text_for_lsp = text.clone();
                let language = Language::from_path(&path);
                let breakpoint_key = self.breakpoint_key(&path);
                let breakpoint_lines = self.breakpoints.lines(&breakpoint_key);
                let editor = cx.new(|cx| {
                    let mut editor = Editor::new(text, language, cx);
                    editor.set_breakpoints(breakpoint_lines, cx);
                    editor
                });
                let version = std::cell::Cell::new(1i32);
                let did_change_path = path.clone();
                let subscription = cx.observe(&editor, move |this, editor, cx| {
                    if let Some(server) = &this.lsp {
                        let next_version = version.get() + 1;
                        version.set(next_version);
                        let text = editor.read(cx).text().to_string();
                        let _ = server.change_file(did_change_path.clone(), text, next_version);
                    }
                    cx.notify();
                });
                let breakpoint_subscription = cx.subscribe(
                    &editor,
                    move |this, _editor, event: &BreakpointToggled, cx| {
                        this.on_breakpoint_toggled(&breakpoint_key, event.line, event.enabled, cx);
                    },
                );
                let menu_key = self.breakpoint_key(&path);
                let menu_subscription = cx.subscribe(
                    &editor,
                    move |this, _editor, event: &BreakpointContextMenu, cx| {
                        this.context_menu = Some(ContextMenuState {
                            path: menu_key.clone(),
                            line: event.line,
                            x: event.x,
                            y: event.y,
                            word: event.word.clone(),
                        });
                        cx.notify();
                    },
                );
                self.tabs.push(Tab {
                    editor,
                    path: path.clone(),
                    _subscription: subscription,
                    _breakpoint_subscription: breakpoint_subscription,
                    _menu_subscription: menu_subscription,
                });
                self.active_tab = Some(self.tabs.len() - 1);
                if let Some(server) = &self.lsp {
                    let _ = server.open_file(path.clone(), text_for_lsp);
                }
                self.status = format!("打开 {}", self.display_path(&path));
                self.focus_on_next_frame = true;
                self.save_session(cx);
                cx.notify();
            }
            Err(error) => {
                self.status = format!("无法读取 {}: {error}", path.display());
                cx.notify();
            }
        }
    }

    fn display_path(&self, path: &Path) -> String {
        if let Some(project) = &self.project {
            project.relative(path)
        } else {
            path.display().to_string()
        }
    }

    fn breakpoint_key(&self, path: &Path) -> String {
        self.project
            .as_ref()
            .map(|project| project.relative(path))
            .unwrap_or_else(|| path.to_string_lossy().replace('\\', "/"))
    }

    fn on_breakpoint_toggled(
        &mut self,
        path: &str,
        line: usize,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        if enabled {
            self.breakpoints.set(path, line, true);
        } else {
            self.breakpoints.remove(path, line);
        }
        self.status = if enabled {
            format!("已添加断点 {path}:{line}")
        } else {
            format!("已删除断点 {path}:{line}")
        };
        self.persist_breakpoints();
        self.sync_debugger_breakpoints();
        cx.notify();
    }

    fn persist_breakpoints(&mut self) {
        let Some(root) = self.project.as_ref().map(|project| project.root.clone()) else {
            return;
        };
        if let Err(error) = self.breakpoints.save(&root) {
            self.status = format!("{}（断点保存失败: {error}）", self.status);
        }
    }

    fn save_active(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.active_tab else {
            self.status = "没有可保存的文件".to_string();
            cx.notify();
            return;
        };
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let path = tab.path.clone();
        let text = tab.editor.read(cx).text().to_string();
        match fs::write(&path, text) {
            Ok(()) => {
                tab.editor.update(cx, |editor, _cx| editor.mark_saved());
                self.status = format!("已保存 {}", self.display_path(&path));
            }
            Err(error) => {
                self.status = format!("保存失败: {error}");
            }
        }
        cx.notify();
    }

    fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        let closed_path = self.tabs.get(index).map(|tab| tab.path.clone());
        self.tabs.remove(index);
        if let Some(path) = closed_path {
            self.lsp_diagnostics.remove(&path);
        }
        match self.active_tab {
            Some(active) if active == index => {
                self.active_tab = if self.tabs.is_empty() {
                    None
                } else {
                    Some(index.min(self.tabs.len() - 1))
                };
            }
            Some(active) if active > index => {
                self.active_tab = Some(active - 1);
            }
            _ => {}
        }
        self.focus_on_next_frame = true;
        self.save_session(cx);
        cx.notify();
    }

    fn activate_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            self.active_tab = Some(index);
            self.focus_on_next_frame = true;
            self.save_session(cx);
            cx.notify();
        }
    }

    /// Persist the current set of open files and active tab to
    /// `.ns-ide/session.json`. Best-effort; failures are reported to the
    /// status bar so the user knows the session may not restore.
    fn save_session(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            return;
        };
        let root = project.root.clone();
        let open_files: Vec<PathBuf> = self
            .tabs
            .iter()
            .filter_map(|tab| tab.path.strip_prefix(&root).ok().map(PathBuf::from))
            .collect();
        let session = ns_project::Session {
            open_files,
            active: self.active_tab,
        };
        if let Err(error) = session.save(&root) {
            self.status = format!("保存会话失败: {error}");
            cx.notify();
        }
    }

    fn next_tab(&mut self, forward: bool, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        let current = self.active_tab.unwrap_or(0);
        let len = self.tabs.len();
        let next = if forward {
            (current + 1) % len
        } else {
            (current + len - 1) % len
        };
        self.active_tab = Some(next);
        self.focus_on_next_frame = true;
        cx.notify();
    }

    fn active_editor(&self) -> Option<Entity<Editor>> {
        self.active_tab
            .and_then(|index| self.tabs.get(index).map(|tab| tab.editor.clone()))
    }

    fn find_in_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.active_editor() {
            editor.update(cx, |editor, cx| editor.open_find(cx));
            if let Some(field) = editor.read(cx).find_query_field_entity() {
                window.focus(&field.read(cx).focus_handle(cx));
            }
        }
    }

    fn close_find_in_file(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.active_editor() {
            editor.update(cx, |editor, cx| editor.close_find(cx));
        }
    }

    fn open_wizard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wizard = Wizard::new(cx);
        wizard::focus_wizard(&wizard, window, cx);
        self.wizard = Some(wizard);
        cx.notify();
    }

    pub fn wizard_cancel(&mut self, cx: &mut Context<Self>) {
        self.wizard = None;
        self.focus_on_next_frame = true;
        cx.notify();
    }

    pub fn wizard_back(&mut self, cx: &mut Context<Self>) {
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        if wizard.step == 0 {
            self.wizard = None;
            self.focus_on_next_frame = true;
        } else {
            wizard.step -= 1;
            wizard.error = None;
        }
        cx.notify();
    }

    pub fn wizard_next(&mut self, cx: &mut Context<Self>) {
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        if let Some(error) = wizard.validate(cx) {
            wizard.error = Some(error);
            cx.notify();
            return;
        }
        if wizard.step + 1 < wizard::STEPS.len() {
            wizard.step += 1;
            wizard.error = None;
            cx.notify();
        } else {
            self.create_project_from_wizard(cx);
        }
    }

    pub fn create_project_from_wizard(&mut self, cx: &mut Context<Self>) {
        let Some(wizard) = self.wizard.as_ref() else {
            return;
        };
        if let Some(error) = wizard.validate(cx) {
            if let Some(wizard) = self.wizard.as_mut() {
                wizard.error = Some(error);
            }
            cx.notify();
            return;
        }
        let spec = wizard.spec(cx);
        match Project::create(spec) {
            Ok(project) => {
                // Generate linker/startup files for the selected target.
                if let Some(wizard) = &self.wizard {
                    if let Some((linker_path, startup_path)) =
                        wizard.generate_device_files(&project.root)
                    {
                        self.status = format!(
                            "已生成设备文件: {} 与 {}",
                            linker_path.display(),
                            startup_path.display()
                        );
                    }
                }
                let name = project.config.project.name.clone();
                self.wizard = None;
                self.load_project(project, cx);
                self.status = format!("已创建项目 {name}");
                cx.notify();
            }
            Err(error) => {
                if let Some(wizard) = self.wizard.as_mut() {
                    wizard.error = Some(error.to_string());
                }
                self.status = format!("创建项目失败: {error}");
                cx.notify();
            }
        }
    }

    pub fn pick_location(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择项目保存位置".into()),
        });
        cx.spawn(async move |this, cx| {
            match receiver.await {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.update(cx, |this, cx| {
                            if let Some(wizard) = this.wizard.as_mut() {
                                wizard
                                    .location
                                    .update(cx, |field, cx| {
                                        field.set_text(path.display().to_string(), cx)
                                    });
                            }
                            cx.notify();
                        })
                        .ok();
                    }
                }
                Ok(Ok(None)) | Err(_) => {}
                Ok(Err(error)) => {
                    this.update(cx, |this, cx| {
                        if let Some(wizard) = this.wizard.as_mut() {
                            wizard.error = Some(format!("打开目录选择器失败: {error}"));
                        }
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn open_project_dialog(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("打开 ns-embed-ide 项目文件夹".into()),
        });
        cx.spawn(async move |this, cx| {
            match receiver.await {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.update(cx, |this, cx| this.open_project_path(path, cx)).ok();
                    }
                }
                Ok(Ok(None)) | Err(_) => {}
                Ok(Err(error)) => {
                    this.update(cx, |this, cx| {
                        this.status = format!("打开项目对话框失败: {error}");
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn start_build(&mut self, run_after: bool, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let plan = match ns_build::plan_build(project) {
            Ok(plan) => plan,
            Err(error) => {
                self.show_output = true;
                self.output_tab = OutputTab::Output;
                self.build_state = BuildState::Failed(Duration::ZERO);
                self.output = format!("构建计划失败\n\n{error}\n");
                self.build_command.clear();
                self.status = format!("构建失败: {error}");
                cx.notify();
                return;
            }
        };

        self.pending_run = run_after;
        self.run.show_run_output = false;
        self.show_output = true;
        self.output_tab = OutputTab::Output;
        self.build_state = BuildState::Running;
        self.diagnostics.clear();
        self.build_command = plan.command_line();
        self.output = format!("$ {}\n\n正在构建…\n", self.build_command);
        self.status = "正在构建…".to_string();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let task = cx
                .background_executor()
                .spawn(async move { ns_build::execute(&plan) });
            let outcome = task.await;
            this.update(cx, |this, cx| this.finish_build(outcome, cx))
                .ok();
        })
        .detach();
    }

    fn start_static_analysis(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let Some(plan) = ns_build::plan_static_analysis(project) else {
            self.show_output = true;
            self.output_tab = OutputTab::Output;
            self.build_state = BuildState::Failed(Duration::ZERO);
            self.output = format!(
                "未找到适用于 {} 的静态分析工具（clang-tidy / cargo clippy / zig / zr-lint），\n\
                 请安装后重试。\n",
                project.config.project.language
            );
            self.build_command.clear();
            self.status = "静态分析: 工具缺失".to_string();
            cx.notify();
            return;
        };
        self.show_output = true;
        self.output_tab = OutputTab::Output;
        self.build_state = BuildState::Running;
        self.diagnostics.clear();
        self.build_command = plan.command_line();
        self.output = format!("$ {}\n\n正在静态分析…\n", self.build_command);
        self.status = "正在静态分析…".to_string();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let task = cx
                .background_executor()
                .spawn(async move { ns_build::execute(&plan) });
            let outcome = task.await;
            this.update(cx, |this, cx| this.finish_static_analysis(outcome, cx))
                .ok();
        })
        .detach();
    }

    /// Flash the built artifact using `project.toml`'s `[flasher]` adapter.
    fn start_flash(&mut self, cx: &mut Context<Self>) {
        self.flash_artifact(FlashAction::Flash, None, cx);
    }

    /// Run an explicit flash action (flash / erase / verify / read-back).
    fn flash_action(&mut self, action: FlashAction, cx: &mut Context<Self>) {
        self.flash_artifact(action, None, cx);
    }


    /// Run the flash adapter and, when `mode` is set, continue the F9
    /// orchestration (reset-and-run or attach the debugger) after success.
    fn flash_artifact(&mut self, action: FlashAction, mode: Option<FlashMode>, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let artifact = project.artifact_path();
        // Erase does not need a firmware artifact; the other actions do.
        if action != FlashAction::Erase && !artifact.exists() {
            self.show_output = true;
            self.output_tab = OutputTab::Output;
            self.output = format!(
                "✕ 找不到固件 {}。请先构建项目。\n",
                artifact.display()
            );
            self.status = format!("{}失败: 找不到固件", action.label());
            cx.notify();
            return;
        }
        let adapter = match adapter_for(project) {
            Ok(adapter) => adapter,
            Err(error) => {
                self.show_output = true;
                self.output_tab = OutputTab::Output;
                self.output = format!("✕ {error}\n");
                self.status = format!("{}失败: {error}", action.label());
                cx.notify();
                return;
            }
        };
        let read_to = (action == FlashAction::Read).then(|| {
            artifact.with_extension("readback.bin")
        });
        let request = FlashRequest {
            tool: adapter.tool(),
            action,
            firmware: artifact.clone(),
            interface: project.config.flasher.interface.clone(),
            read_to,
            device: None,
        };
        self.show_output = true;
        self.output_tab = OutputTab::Output;
        self.output = format!(
            "正在用 {} {} {}…\n",
            adapter.tool().label(),
            action.label(),
            artifact.display()
        );
        self.status = format!("正在{} ({})…", action.label(), adapter.tool().label());
        cx.notify();


        let action = request.action;
        let read_to = request.read_to.clone();
        cx.spawn(async move |this, cx| {
            let (tx, rx) = std::sync::mpsc::channel::<String>();
            let adapter_task = std::thread::spawn({
                let tx = tx.clone();
                move || {
                    // Run the adapter on a blocking thread; stream each line
                    // over the channel so the UI shows progress live.
                    let mut on_line = |line: &str| {
                        let _ = tx.send(format!("{line}\n"));
                    };
                    adapter.run(&request, &mut on_line)
                }
            });
            // Drain the channel on the UI thread; after the adapter finishes
            // (channel closed), join the thread to learn the outcome.
            let result;
            let mut done = false;
            while !done {
                while let Ok(line) = rx.try_recv() {
                    let text = line;
                    if this
                        .update(cx, |this, cx| {
                            this.output.push_str(&text);
                            cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                if adapter_task.is_finished() {
                    done = true;
                } else {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(50))
                        .await;
                }
            }
            // Drain any remaining lines, then join.
            while let Ok(line) = rx.try_recv() {
                let text = line;
                this.update(cx, |this, cx| {
                    this.output.push_str(&text);
                    cx.notify();
                })
                .ok();
            }
            result = Some(adapter_task.join().expect("adapter thread panicked"));
            this.update(cx, |this, cx| match result {
                Some(Ok(outcome)) => {
                    if !outcome.combined_output().trim().is_empty() {
                        this.output.push('\n');
                    }
                    if outcome.success {
                        let done = match action {
                            FlashAction::Flash => format!("烧写完成 ({})", outcome.tool.label()),
                            FlashAction::Erase => format!("擦除完成 ({})", outcome.tool.label()),
                            FlashAction::Verify => format!("校验通过 ({})", outcome.tool.label()),
                            FlashAction::Read => format!(
                                "读回完成 → {}",
                                read_to.as_ref().map_or(
                                    "未知文件".to_string(),
                                    |p| p.display().to_string()
                                )
                            ),
                        };
                        this.status = done;
                        match mode {
                            Some(FlashMode::Run) => this.spawn_run(cx),
                            Some(FlashMode::Debug) => {
                                if this.is_embedded_target() {
                                    this.launch_embedded_debug(cx);
                                } else {
                                    this.launch_debug(cx);
                                }
                            }
                            None => {}
                        }
                    } else {
                        this.status = format!(
                            "{}失败 ({}: {})",
                            action.label(),
                            outcome.tool.label(),
                            outcome.code.map_or("未知退出码".to_string(), |c| c.to_string())
                        );
                    }
                    cx.notify();
                }
                Some(Err(error)) => {
                    this.output = format!("✕ {error}\n");
                    this.status = format!("{}失败: {error}", action.label());
                    cx.notify();
                }
                None => {}
            })
            .ok();
        })
        .detach();
    }


    /// F9 调试运行: build → flash → attach the debugger.
    fn flash_and_debug(&mut self, cx: &mut Context<Self>) {
        self.pending_flash = Some(FlashMode::Debug);
        self.start_build(false, cx);
    }

    /// F9 直接运行: build → flash → reset-and-run without a debugger.
    fn flash_and_run(&mut self, cx: &mut Context<Self>) {
        self.pending_flash = Some(FlashMode::Run);
        self.start_build(false, cx);
    }

    /// 嵌入式调试入口: build → flash → attach the probe GDB server.
    /// Same orchestration as `flash_and_debug` but routed to the embedded
    /// target when the project sets a `target` MCU.
    fn start_embedded_debug(&mut self, cx: &mut Context<Self>) {
        if self.debug_session.is_some() {
            self.continue_debug(cx);
            return;
        }
        self.pending_flash = Some(FlashMode::Debug);
        self.start_build(false, cx);
    }

    fn finish_static_analysis(&mut self, outcome: BuildOutcome, cx: &mut Context<Self>) {
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default();
        let diagnostics = outcome.diagnostics(&root);
        self.diagnostics = diagnostics
            .into_iter()
            .filter(|diagnostic| diagnostic.severity != Severity::Info)
            .collect();
        self.build_command = outcome.plan.command_line();
        let mut output = format!("$ {}\n\n", self.build_command);
        output.push_str(&outcome.combined_output());
        output.push_str(&format!(
            "\n[静态分析] {} 个问题 · {:.2}s\n",
            self.diagnostics.len(),
            outcome.duration.as_secs_f64()
        ));
        self.output = output;
        self.build_state = if self.diagnostics.is_empty() {
            BuildState::Success(outcome.duration)
        } else {
            BuildState::Failed(outcome.duration)
        };
        self.output_tab = if self.diagnostics.is_empty() {
            OutputTab::Output
        } else {
            OutputTab::Problems
        };
        self.show_output = true;
        self.status = format!(
            "静态分析完成 · {} 个问题",
            self.diagnostics.len()
        );
        cx.notify();
    }

    fn finish_build(&mut self, outcome: BuildOutcome, cx: &mut Context<Self>) {
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default();
        let diagnostics = outcome.diagnostics(&root);
        self.diagnostics = diagnostics
            .into_iter()
            .filter(|diagnostic| diagnostic.severity != Severity::Info)
            .collect();
        self.build_command = outcome.plan.command_line();
        let mut output = format!("$ {}\n\n", self.build_command);
        output.push_str(&outcome.combined_output());
        output.push_str(&format!(
            "\n[{}] 用时 {:.2}s\n",
            if outcome.success { "成功" } else { "失败" },
            outcome.duration.as_secs_f64()
        ));
        self.output = output;
        self.build_state = if outcome.success {
            BuildState::Success(outcome.duration)
        } else {
            BuildState::Failed(outcome.duration)
        };
        self.output_tab = if self.diagnostics.is_empty() {
            OutputTab::Output
        } else {
            OutputTab::Problems
        };
        self.show_output = true;
        self.status = if outcome.success {
            if self.pending_run {
                "构建成功".to_string()
            } else {
                format!(
                    "构建成功 · {} 个问题 · {:.2}s",
                    self.diagnostics.len(),
                    outcome.duration.as_secs_f64()
                )
            }
        } else {
            format!(
                "构建失败 · {} 个错误 · {:.2}s",
                self.diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.severity == Severity::Error)
                    .count(),
                outcome.duration.as_secs_f64()
            )
        };
        let should_run = self.pending_run;
        self.pending_run = false;
        if let Some(mode) = self.pending_flash.take() {
            if outcome.success {
                self.flash_artifact(FlashAction::Flash, Some(mode), cx);
                return;
            }
            self.status = "构建失败，烧写未执行".to_string();
        }
        if self.pending_debug {
            self.pending_debug = false;
            if outcome.success {
                if self.is_embedded_target() {
                    self.launch_embedded_debug(cx);
                } else {
                    self.launch_debug(cx);
                }
                return;
            }
            self.status = "构建失败，调试会话未启动".to_string();
        }
        if should_run {
            if outcome.success {
                self.spawn_run(cx);
            } else {
                self.status = "构建失败，未启动运行".to_string();
            }
        }
        cx.notify();
    }

    /// Stop the current direct run, if any.
    fn stop_run(&mut self, cx: &mut Context<Self>) {
        // Signal the child to die; the worker thread owns `wait()` so the UI
        // thread never blocks on it.
        if let Ok(mut guard) = self.run.child.lock() {
            if let Some(child) = guard.as_mut() {
                let _ = child.kill();
            }
        }
        // Cancel a pending run- after build too: stopping means "do not run".
        self.pending_run = false;
        if self.run.running {
            self.run.running = false;
            self.run.stopped = true;
            self.status = "运行已停止".to_string();
            cx.notify();
        }
    }

    /// Restart the run: stop the current process (if any), then rebuild and
    /// run again. Used by the toolbar restart button and `restart-run`.
    fn restart_run(&mut self, cx: &mut Context<Self>) {
        self.stop_run(cx);
        self.run.started = false;
        self.run.output.clear();
        self.start_build(true, cx);
    }

    /// Spawn the built artifact directly and stream its output (F9 直接运行).
    fn spawn_run(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let artifact = project.artifact_path();
        let cwd = project.root.clone();
        if !artifact.exists() {
            self.status = format!("找不到运行产物: {}", artifact.display());
            self.show_output = true;
            self.output_tab = OutputTab::Output;
            self.run.output = format!("找不到运行产物: {}\n", artifact.display());
            cx.notify();
            return;
        }
        self.stop_run(cx);
        let command_line = artifact.display().to_string();
        let mut child = match Command::new(&artifact)
            .current_dir(&cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                self.status = format!("启动运行失败: {error}");
                self.run.output = format!("$ {command_line}\n\n启动失败: {error}\n");
                self.show_output = true;
                self.output_tab = OutputTab::Output;
                cx.notify();
                return;
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let child_shared = self.run.child.clone();
        *child_shared.lock().unwrap() = Some(child);

        let (sender, receiver) = std::sync::mpsc::channel::<RunEvent>();
        self.run.command = command_line.clone();
        self.run.output = format!("$ {command_line}\n\n");
        self.run.running = true;
        self.run.exit_code = None;
        self.run.generation += 1;
        let generation = self.run.generation;
        self.run.stopped = false;
        self.run.started = true;
        self.output_tab = OutputTab::Output;
        self.status = "正在运行…".to_string();
        cx.notify();

        // Worker threads: stream stdout/stderr, then report the exit code.
        if let Some(stdout) = stdout {
            let sender = sender.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    match line {
                        Ok(line) => {
                            if sender.send(RunEvent::Output(format!("{line}\n"))).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
        if let Some(stderr) = stderr {
            let sender = sender.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    match line {
                        Ok(line) => {
                            if sender.send(RunEvent::Output(format!("{line}\n"))).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
        let child_shared_waiter = child_shared.clone();
        std::thread::spawn(move || {
            // Take ownership so `wait()` never runs while holding the lock
            // (the UI's stop button would otherwise block on it).
            let child = child_shared_waiter.lock().unwrap().take();
            let code = child
                .and_then(|mut child| child.wait().ok())
                .and_then(|status| status.code())
                .unwrap_or(-1);
            let _ = sender.send(RunEvent::Exited(code));
        });

        let receiver = receiver;
        cx.spawn(async move |this, cx| {
            while let Ok(event) = receiver.recv() {
                if this
                    .update(cx, |this, cx| this.handle_run_event(event, generation, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Apply one run event to the UI state; events from a replaced run are
    /// discarded so a stale exit never pollutes a newer run's output.
    fn handle_run_event(&mut self, event: RunEvent, generation: u64, cx: &mut Context<Self>) {
        if generation != self.run.generation {
            return;
        }
        match event {
            RunEvent::Output(chunk) => {
                self.run.output.push_str(&chunk);
            }
            RunEvent::Exited(code) => {
                self.run.running = false;
                self.run.exit_code = Some(code);
                // A manual stop kills the child; Unix reports no code for a
                // signal death, and the stop already set the status, so do not
                // overwrite it with a misleading `-1`.
                if self.run.stopped {
                    self.run.output.push_str("\n[运行已停止]\n");
                } else {
                    self.run.output.push_str(&format!(
                        "\n[进程退出，代码 {code}]\n"
                    ));
                    self.status = format!("运行结束 · 退出代码 {code}");
                }
            }
        }
        cx.notify();
    }

    fn start_debug(&mut self, cx: &mut Context<Self>) {
        // If a session exists, F5 means "继续".
        if self.debug_session.is_some() {
            self.continue_debug(cx);
            return;
        }
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let plan = match ns_build::plan_build(project) {
            Ok(plan) => plan,
            Err(error) => {
                self.show_output = true;
                self.output_tab = OutputTab::Debug;
                self.debug.error = Some(format!("构建计划失败: {error}"));
                self.debug.push_console(format!("✕ 构建计划失败: {error}"));
                cx.notify();
                return;
            }
        };

        self.pending_debug = true;
        self.show_output = true;
        self.output_tab = OutputTab::Output;
        self.build_state = BuildState::Running;
        self.diagnostics.clear();
        self.build_command = plan.command_line();
        self.output = format!("$ {}\n\n正在为调试构建…\n", self.build_command);
        self.status = "正在为调试构建…".to_string();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let task = cx
                .background_executor()
                .spawn(async move { ns_build::execute(&plan) });
            let outcome = task.await;
            this.update(cx, |this, cx| this.finish_build(outcome, cx))
                .ok();
        })
        .detach();
    }

    fn launch_debug(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            return;
        };
        let artifact = project.artifact_path();
        if !artifact.exists() {
            self.debug.error = Some(format!("找不到调试产物: {}", artifact.display()));
            self.debug.push_console(format!(
                "✕ 找不到调试产物 {}。请先构建项目。",
                artifact.display()
            ));
            self.output_tab = OutputTab::Debug;
            cx.notify();
            return;
        }
        let root = project.root.clone();
        let breakpoints: Vec<DebugBreakpoint> = self
            .breakpoints
            .all()
            .iter()
            .filter(|breakpoint| breakpoint.enabled)
            .map(|breakpoint| DebugBreakpoint {
                path: root.join(&breakpoint.path).display().to_string(),
                line: breakpoint.line,
                condition: if breakpoint.kind == BreakpointKind::Conditional
                    && !breakpoint.condition.trim().is_empty()
                {
                    Some(breakpoint.condition.clone())
                } else {
                    None
                },
                hit_condition: if !breakpoint.hit_count.trim().is_empty() {
                    Some(breakpoint.hit_count.clone())
                } else {
                    None
                },
                log_message: if breakpoint.kind == BreakpointKind::Log
                    && !breakpoint.log_message.trim().is_empty()
                {
                    Some(breakpoint.log_message.clone())
                } else {
                    None
                },
                hardware: breakpoint.kind == BreakpointKind::Hardware,
            })
            .collect();
        let synced_files: HashSet<String> = breakpoints
            .iter()
            .map(|breakpoint| breakpoint.path.clone())
            .collect();
        let config = DebugConfig {
            program: artifact,
            args: Vec::new(),
            cwd: root,
            breakpoints,
        };

        match ns_debug::spawn(&config) {
            Ok(handle) => {
                let (messages, commands) = handle.into_parts();
                let (bridge_sender, bridge_receiver) =
                    futures::channel::mpsc::unbounded::<DebugMessage>();
                cx.background_executor()
                    .spawn(async move {
                        while let Ok(message) = messages.recv() {
                            if bridge_sender.unbounded_send(message).is_err() {
                                break;
                            }
                        }
                    })
                    .detach();

                let console_commands = commands.clone();
                self.debug = DebugView {
                    status: DebugStatus::Running,
                    synced_files,
                    console_input: Some({
                        let input = cx.new(|cx| TextField::new("输入表达式，回车求值", cx));
                        input.update(cx, |field, _cx| {
                            let commands = console_commands.clone();
                            field.set_on_submit(move |text, _window, _cx| {
                                let text = text.trim().to_string();
                                if text.is_empty() {
                                    return;
                                }
                                let _ = commands.send(DebugCommand::Evaluate {
                                    expression: text,
                                    scope_name: "console".to_string(),
                                });
                            });
                        });
                        input
                    }),
                    watch_input: Some({
                        let app: Entity<IdeApp> = cx.entity();
                        let input = cx.new(|cx| TextField::new("输入监视表达式，回车添加", cx));
                        input.update(cx, |field, _cx| {
                            field.set_on_submit(move |text, _window, _cx| {
                                let text = text.trim().to_string();
                                if text.is_empty() {
                                    return;
                                }
                                let app = app.clone();
                                let _ = app.update(_cx, |this, cx| {
                                    this.debug.add_watch(&text);
                                    if let Some(session) = &this.debug_session {
                                        let _ = session.commands.send(DebugCommand::Evaluate {
                                            expression: text,
                                            scope_name: "watch".to_string(),
                                        });
                                    }
                                    cx.notify();
                                });
                            });
                        });
                        input
                    }),
                    ..DebugView::default()
                };
                self.debug.push_console("调试会话已启动（lldb-dap）".to_string());
                self.debug_session = Some(DebugSession { commands });
                self.output_tab = OutputTab::Debug;
                self.show_output = true;
                self.status = "调试会话已启动".to_string();
                cx.notify();

                cx.spawn(async move |this, cx| {
                    let mut receiver = bridge_receiver;
                    while let Some(message) = receiver.next().await {
                        if this
                            .update(cx, |this, cx| this.handle_debug_message(message, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(error) => {
                self.debug.error = Some(error.to_string());
                self.debug.push_console(format!("✕ {error}"));
                self.output_tab = OutputTab::Debug;
                self.show_output = true;
                self.status = "调试会话启动失败".to_string();
                cx.notify();
            }
        }
    }

    /// Launch an embedded-target debug session: start the probe GDB server
    /// (OpenOCD / ST-Link / J-Link / pyOCD / probe-rs) and attach a GDB
    /// client to it via `target remote`.
    ///
    /// **Not verified on real hardware.** The probe server commands and the
    /// GDB MI plumbing are implemented per the upstream project layouts; a
    /// hardware-in-the-loop run is required before this is considered proven.
    fn launch_embedded_debug(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            return;
        };
        let artifact = project.artifact_path();
        if !artifact.exists() {
            self.debug.error = Some(format!("找不到调试产物: {}", artifact.display()));
            self.debug.push_console(format!(
                "✕ 找不到调试产物 {}。请先构建项目。",
                artifact.display()
            ));
            self.output_tab = OutputTab::Debug;
            cx.notify();
            return;
        }
        let probe = match probe_kind(&project.config.debugger.probe) {
            Ok(probe) => probe,
            Err(error) => {
                self.debug.error = Some(error.clone());
                self.debug.push_console(format!("✕ {error}"));
                self.output_tab = OutputTab::Debug;
                self.show_output = true;
                cx.notify();
                return;
            }
        };
        let root = project.root.clone();
        let breakpoints: Vec<ns_debug::DebugBreakpoint> = self
            .breakpoints
            .all()
            .iter()
            .filter(|breakpoint| breakpoint.enabled)
            .map(|breakpoint| ns_debug::DebugBreakpoint {
                path: root.join(&breakpoint.path).display().to_string(),
                line: breakpoint.line,
                condition: if breakpoint.kind == BreakpointKind::Conditional
                    && !breakpoint.condition.trim().is_empty()
                {
                    Some(breakpoint.condition.clone())
                } else {
                    None
                },
                hit_condition: if !breakpoint.hit_count.trim().is_empty() {
                    Some(breakpoint.hit_count.clone())
                } else {
                    None
                },
                log_message: if breakpoint.kind == BreakpointKind::Log
                    && !breakpoint.log_message.trim().is_empty()
                {
                    Some(breakpoint.log_message.clone())
                } else {
                    None
                },
                hardware: breakpoint.kind == BreakpointKind::Hardware,
            })
            .collect();
        let config = EmbeddedConfig {
            probe,
            interface: project.config.debugger.interface.clone(),
            target: project.config.project.target.clone(),
            probe_path: (!project.config.debugger.server.is_empty())
                .then(|| project.config.debugger.server.clone()),
            server_path: (!project.config.debugger.path.is_empty())
                .then(|| project.expand_path(&project.config.debugger.path)),
            gdb_path: (!project.config.debugger.gdb.is_empty())
                .then(|| project.expand_path(&project.config.debugger.gdb)),
            port: 0,
            program: artifact,
            breakpoints,
        };

        let app: Entity<IdeApp> = cx.entity();
        self.debug = DebugView {
            status: DebugStatus::Running,
            console_input: Some(cx.new(|cx| TextField::new("输入表达式，回车求值", cx))),
            watch_input: Some({
                let input = cx.new(|cx| TextField::new("输入监视表达式，回车添加", cx));
                input.update(cx, |field, _cx| {
                    let app = app.clone();
                    field.set_on_submit(move |text, _window, _cx| {
                        let text = text.trim().to_string();
                        if text.is_empty() {
                            return;
                        }
                        let app = app.clone();
                        let _ = app.update(_cx, |this, cx| {
                            this.debug.add_watch(&text);
                            if let Some(session) = &this.debug_session {
                                let _ = session.commands.send(DebugCommand::Evaluate {
                                    expression: text,
                                    scope_name: "watch".to_string(),
                                });
                            }
                            cx.notify();
                        });
                    });
                });
                input
            }),
            ..DebugView::default()
        };
        self.debug.push_console(format!(
            "正在启动嵌入式调试（{} · {}）…",
            probe.label(),
            config.target
        ));
        self.output_tab = OutputTab::Debug;
        self.show_output = true;
        self.status = "正在连接目标调试服务器…".to_string();
        cx.notify();

        // Run the probe server + gdb attach off the UI thread so the handshake
        // never blocks rendering; the bridge forwards messages back.
        let handle_config = config.clone();
        cx.spawn(async move |this, cx| {
            let task = cx
                .background_executor()
                .spawn(async move {
                    ns_debug::spawn_embedded_debug(&handle_config)
                });
            match task.await {
                Ok(handle) => {
                    let (messages, commands) = handle.into_parts();
                    let (bridge_sender, bridge_receiver) =
                        futures::channel::mpsc::unbounded::<DebugMessage>();
                    cx.background_executor()
                        .spawn(async move {
                            while let Ok(message) = messages.recv() {
                                if bridge_sender.unbounded_send(message).is_err() {
                                    break;
                                }
                            }
                        })
                        .detach();
                    this.update(cx, |this, cx| {
                        if let Some(input) = this.debug.console_input.clone() {
                            let commands = commands.clone();
                            input.update(cx, |field, _cx| {
                                field.set_on_submit(move |text, _window, _cx| {
                                    let text = text.trim().to_string();
                                    if text.is_empty() {
                                        return;
                                    }
                                    let _ = commands.send(DebugCommand::Evaluate {
                                        expression: text,
                                        scope_name: "console".to_string(),
                                    });
                                });
                            });
                        }
                        this.debug_session = Some(DebugSession { commands });
                        this.debug.push_console("嵌入式调试会话已启动".to_string());
                        this.status = "嵌入式调试会话已启动".to_string();
                        cx.notify();
                    })
                    .ok();
                    cx.spawn(async move |cx| {
                        let mut receiver = bridge_receiver;
                        while let Some(message) = receiver.next().await {
                            if this
                                .update(cx, |this, cx| this.handle_debug_message(message, cx))
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                    .detach();
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.debug.error = Some(error.to_string());
                        this.debug.push_console(format!("✕ {error}"));
                        this.status = "嵌入式调试启动失败".to_string();
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn handle_debug_message(
        &mut self,
        message: DebugMessage,
        cx: &mut Context<Self>,
    ) {
        match message {
            DebugMessage::Ready => {
                self.debug.status = DebugStatus::Running;
                self.debug.push_console("调试器已就绪".to_string());
            }
            DebugMessage::Stopped { reason, thread_id } => {
                self.debug.status = DebugStatus::Stopped { reason: reason.clone() };
                self.debug.thread_id = thread_id;
                self.debug.frames.clear();
                self.debug.scopes.clear();
                self.debug.variables.clear();
                self.debug.children.clear();
                self.debug.expanded.clear();
                self.debug.pc = None;
                self.debug.disassembly.clear();
                self.debug.active_frame = 0;
                self.debug.active_scope = 0;
                self.debug.push_console(format!(
                    "● 程序暂停 · {}",
                    self.debug.status.label()
                ));
                if let Some(session) = &self.debug_session {
                    let _ = session.commands.send(DebugCommand::GetStackTrace { thread_id });
                }
                self.begin_watchpoint_resolution();
                // Re-evaluate every watch expression at the new stop.
                let watches: Vec<String> = self
                    .debug
                    .watches
                    .iter()
                    .map(|watch| watch.expression.clone())
                    .collect();
                for expression in watches {
                    if let Some(session) = &self.debug_session {
                        let _ = session.commands.send(DebugCommand::Evaluate {
                            expression,
                            scope_name: "watch".to_string(),
                        });
                    }
                }
                self.output_tab = OutputTab::Debug;
                self.status = format!("调试暂停 · {}", self.debug.status.label());
                cx.notify();
            }
            DebugMessage::Continued => {
                self.debug.status = DebugStatus::Running;
                self.debug.push_console("▶ 程序继续运行".to_string());
                self.debug.watchpoint_resolution_pending = false;
                self.debug.resolving_watchpoints.clear();
                self.debug.watchpoint_scope_variables.clear();
                self.breakpoints.clear_watchpoint_hits();
                self.clear_debug_highlights(cx);
            }
            DebugMessage::StackTrace { frames } => {
                self.debug.pc = frames.first().and_then(|frame| frame.pc);
                self.debug.frames = frames;
                self.debug.active_frame = 0;
                self.focus_debug_frame(0, cx);
                if let Some(frame) = self.debug.frames.first() {
                    if let Some(session) = &self.debug_session {
                        let _ = session.commands.send(DebugCommand::GetScopes {
                            frame_id: frame.id,
                        });
                    }
                }
            }
            DebugMessage::Scopes { scopes, .. } => {
                self.debug.scopes = scopes;
                self.debug.active_scope = 0;
                if self.debug.watchpoint_resolution_pending {
                    // Fetch every scope's variables so watchpoint expressions
                    // can be located; the replies drive `collect_watchpoint_scope`.
                    for scope in self.debug.scopes.clone() {
                        if let Some(session) = &self.debug_session {
                            let _ = session.commands.send(DebugCommand::GetVariables {
                                variables_reference: scope.id,
                            });
                        }
                    }
                } else if let Some(scope) = self.debug.scopes.first() {
                    if let Some(session) = &self.debug_session {
                        let _ = session.commands.send(DebugCommand::GetVariables {
                            variables_reference: scope.id,
                        });
                    }
                }
            }
            DebugMessage::Variables { reference, variables } => {
                if self.debug.watchpoint_resolution_pending {
                    self.collect_watchpoint_scope(reference, variables.clone());
                }
                let is_active_scope = self
                    .debug
                    .scopes
                    .get(self.debug.active_scope)
                    .map(|scope| scope.id == reference)
                    .unwrap_or(false);
                if is_active_scope || reference == 0 && self.debug.variables.is_empty() {
                    self.debug.variables = variables;
                } else {
                    self.debug.children.insert(reference, variables);
                }
            }
            DebugMessage::Memory { address, data } => {
                self.debug.memory.address = address;
                self.debug.memory.bytes = data;
                self.debug.memory.error = None;
                self.debug
                    .push_console(format!("内存 {address:#x} · {} 字节", self.debug.memory.bytes.len()));
            }
            DebugMessage::Disassembly { instructions } => {
                self.debug.disassembly = instructions;
            }
            DebugMessage::Output { category, text } => {
                let prefix = match category.as_str() {
                    "stdout" | "console" => "",
                    "stderr" => "! ",
                    _ => "",
                };
                for line in text.lines() {
                    if !line.is_empty() {
                        self.debug.push_console(format!("{prefix}{line}"));
                    }
                }
            }
            DebugMessage::BreakpointResults { file, results } => {
                for result in results {
                    if let Some(relative) = self
                        .project
                        .as_ref()
                        .map(|project| project.relative(Path::new(&file)))
                        .filter(|relative| !relative.is_empty())
                    {
                        self.breakpoints
                            .mark_verified(&relative, result.line, result.verified, &result.reason);
                    }
                }
                self.persist_breakpoints();
            }
            DebugMessage::DataBreakpointInfo {
                expression,
                data_id,
                access_types,
                description,
            } => {
                self.resolve_watchpoint(expression, data_id, access_types, description);
            }
            DebugMessage::WatchpointResults { results } => {
                for result in results {
                    self.breakpoints.mark_watchpoint_verified(
                        &result.expression,
                        result.verified,
                        &result.reason,
                        result.adapter_id,
                    );
                    if !result.verified {
                        self.debug.push_console(format!(
                            "✕ 观察点 `{}` 未绑定: {}",
                            result.expression, result.reason
                        ));
                    }
                }
                self.persist_breakpoints();
            }
            DebugMessage::WatchpointHit { description } => {
                let hit_expression = self
                    .breakpoints
                    .watchpoints()
                    .iter()
                    .find(|watchpoint| {
                        watchpoint
                            .adapter_id
                            .map(|id| description.contains(&format!("{id}")))
                            .unwrap_or(false)
                    })
                    .map(|watchpoint| watchpoint.expression.clone());
                if let Some(expression) = hit_expression {
                    self.breakpoints.mark_watchpoint_hit(&expression);
                }
            }
            DebugMessage::Threads { .. } => {}
            DebugMessage::ScopeVariables { .. } => {}
            DebugMessage::Caret { path, line } => {
                let Some(path) = path else {
                    return;
                };
                if let Some(relative) = self
                    .project
                    .as_ref()
                    .map(|project| project.relative(Path::new(&path)))
                    .filter(|relative| !relative.is_empty())
                {
                    self.focus_source_line(&relative, line, cx);
                }
            }
            DebugMessage::TraceConfigured { config } => {
                self.debug.trace.config = config.clone();
                self.debug.trace.armed = true;
                self.debug.trace.error = None;
                self.debug.push_console(format!(
                    "◈ trace 窗口已就绪（DWT 数据跟踪={}，ITM 端口={}）",
                    config.data_trace, config.itm_port
                ));
                cx.notify();
            }
            DebugMessage::Evaluated {
                expression,
                result,
                type_name,
            } => {
                // Watch evaluation updates the watch list; everything else
                // (console, completion) goes to the console log.
                let is_watch = self
                    .debug
                    .watches
                    .iter()
                    .any(|watch| watch.expression == expression);
                if is_watch {
                    self.debug.update_watch(&expression, &result);
                } else {
                    let line = match type_name {
                        Some(type_name) => format!("({type_name}) {expression} = {result}"),
                        None => format!("{expression} = {result}"),
                    };
                    self.debug.push_console(line);
                }
            }
            DebugMessage::VariableSet {
                variables_reference,
                name,
                value,
                ..
            } => {
                self.debug.push_console(format!("{name} = {value}"));
                // Refresh the scope so the new value shows in the variables panel.
                if let Some(session) = &self.debug_session {
                    let _ = session.commands.send(DebugCommand::GetVariables {
                        variables_reference,
                    });
                }
            }
            DebugMessage::Terminated => {
                self.debug.status = DebugStatus::Exited;
                self.debug.push_console("■ 调试会话已结束".to_string());
                self.debug_session = None;
                self.debug.watchpoint_resolution_pending = false;
                self.debug.trace.armed = false;
                self.debug.resolving_watchpoints.clear();
                self.debug.watchpoint_scope_variables.clear();
                self.breakpoints.clear_watchpoint_hits();
                self.clear_debug_highlights(cx);
                self.status = "调试会话已结束".to_string();
            }
            DebugMessage::Error(error) => {
                self.debug.error = Some(error.clone());
                self.debug.push_console(format!("✕ {error}"));
            }
        }
        cx.notify();
    }

    fn focus_debug_frame(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.debug.frames.len() {
            return;
        }
        self.debug.active_frame = index;
        let frame = &self.debug.frames[index];
        let file = frame.file.clone();
        let line = frame.line.unwrap_or(1);

        // Highlight the stopped line in whichever tab shows this file.
        let mut focused = false;
        for tab in &self.tabs {
            let matches = file
                .as_deref()
                .map(|file| {
                    fs::canonicalize(file)
                        .map(|canonical| canonical == tab.path)
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            tab.editor.update(cx, |editor, cx| {
                editor.set_debug_line(if matches { Some(line) } else { None }, cx);
            });
            if matches {
                focused = true;
            }
        }

        if !focused {
            if let Some(file) = file {
                let path = PathBuf::from(&file);
                if path.exists() {
                    self.open_file(path, cx);
                    if let Some(editor) = self.active_editor() {
                        editor.update(cx, |editor, cx| {
                            editor.set_cursor_line(line, cx);
                            editor.set_debug_line(Some(line), cx);
                        });
                    }
                } else {
                    self.debug.push_console(format!("源文件不存在: {file}"));
                }
            }
        }
    }

    fn select_debug_frame(&mut self, index: usize, cx: &mut Context<Self>) {
        self.focus_debug_frame(index, cx);
        if let Some(frame) = self.debug.frames.get(index) {
            if let Some(session) = &self.debug_session {
                let _ = session.commands.send(DebugCommand::GetScopes { frame_id: frame.id });
            }
        }
        cx.notify();
    }

    fn select_debug_scope(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.debug.scopes.len() {
            return;
        }
        self.debug.active_scope = index;
        if let Some(session) = &self.debug_session {
            let _ = session.commands.send(DebugCommand::GetVariables {
                variables_reference: self.debug.scopes[index].id,
            });
        }
        cx.notify();
    }

    fn toggle_variable_expansion(&mut self, reference: u64, cx: &mut Context<Self>) {
        if reference == 0 {
            return;
        }
        if self.debug.expanded.contains(&reference) {
            self.debug.expanded.remove(&reference);
        } else {
            self.debug.expanded.insert(reference);
            if !self.debug.children.contains_key(&reference) {
                if let Some(session) = &self.debug_session {
                    let _ = session.commands.send(DebugCommand::GetVariables {
                        variables_reference: reference,
                    });
                }
            }
        }
        cx.notify();
    }

    fn set_view_tab(&mut self, tab: DebugViewTab, cx: &mut Context<Self>) {
        self.debug.view_tab = tab;
        if tab == DebugViewTab::Memory {
            self.ensure_memory_input(cx);
        }
        if tab == DebugViewTab::Disassembly && self.debug.disassembly.is_empty() && self.debug.pc.is_some() {
            self.disassemble_at_pc(cx);
        }
        if tab == DebugViewTab::Trace {
            self.ensure_trace_fields(cx);
        }
        cx.notify();
    }

    /// Load the built artifact into the built-in simulator and start a session.
    fn start_simulation(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let artifact = project.artifact_path();
        if !artifact.exists() {
            self.sim.status = format!("找不到产物: {}", artifact.display());
            self.sim.panel_open = true;
            self.status = self.sim.status.clone();
            cx.notify();
            return;
        }
        let Ok(bytes) = std::fs::read(&artifact) else {
            self.sim.status = format!("读取产物失败: {}", artifact.display());
            self.sim.panel_open = true;
            self.status = self.sim.status.clone();
            cx.notify();
            return;
        };
        let mut core = ns_sim::Simulator::new();
        // Prefer ELF when the artifact is ELF; otherwise load raw at entry.
        let is_elf = bytes.len() >= 4 && &bytes[0..4] == b"\x7fELF";
        if is_elf {
            match core.load_elf(&bytes) {
                Ok(_) => {}
                Err(error) => {
                    self.sim.status = format!("加载 ELF 失败: {error}");
                    self.sim.panel_open = true;
                    self.status = self.sim.status.clone();
                    cx.notify();
                    return;
                }
            }
        } else {
            core.load_image(ns_sim::ENTRY_POINT, &bytes);
        }
        // The simulator records UART bytes in `outputs`; the panel reads them
        // directly, so no extra routing is needed here.
        core.peripherals.putchar = Box::new(|_| {});
        self.sim.core = Some(core);
        self.sim_sync_breakpoints(cx);
        self.sim.panel_open = true;
        self.sim.status = format!(
            "已加载 {} 到仿真器（入口 0x{:x}）",
            artifact.display(),
            self.sim.core.as_ref().map(|c| c.pc).unwrap_or(0)
        );
        self.status = self.sim.status.clone();
        self.debug.view_tab = DebugViewTab::Simulator;
        cx.notify();
    }

    fn sim_step(&mut self, cx: &mut Context<Self>) {
        if let Some(core) = self.sim.core.as_mut() {
            let result = core.step();
            self.sim.status = match result {
                ns_sim::StepResult::Halted => "仿真已停止（halted）".to_string(),
                ns_sim::StepResult::Breakpoint => "命中断点".to_string(),
                ns_sim::StepResult::Advanced => format!("单步: PC=0x{:x}", core.pc),
            };
        }
        cx.notify();
    }

    /// Sync the simulator's breakpoints from the active editor's breakpoints,
    /// mapping source lines to addresses via the ELF line table.
    fn sim_sync_breakpoints(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor() else {
            return;
        };
        let lines = editor.read(cx).breakpoint_lines();
        let Some(core) = self.sim.core.as_mut() else {
            return;
        };
        let mut addresses = Vec::new();
        for line in lines {
            // The line table maps each source line to its instruction address;
            // source line N maps to line N in the coarse table.
            if let Some(address) = core.address_for_line(0, line as u32) {
                addresses.push(address);
            }
        }
        core.breakpoints = addresses;
    }


    fn sim_run(&mut self, cx: &mut Context<Self>) {
        self.sim_sync_breakpoints(cx);
        let Some(core) = self.sim.core.as_mut() else {
            cx.notify();
            return;
        };
        let result = core.run(1_000_000);
        let status = match result {
            ns_sim::StepResult::Halted => format!(
                "仿真结束: {} 周期, UART 输出 {} 字节",
                core.cycle_count,
                core.peripherals.outputs.len()
            ),
            ns_sim::StepResult::Breakpoint => {
                let line = core
                    .line_for_address(core.pc)
                    .map(|(_file, line)| format!("（源码第 {line} 行）"))
                    .unwrap_or_default();
                format!("命中断点 @ 0x{:x} {line}", core.pc)
            }
            ns_sim::StepResult::Advanced => format!("已运行 {} 周期（超时上限）", core.cycle_count),
        };
        self.sim.status = status;
        cx.notify();
    }

    fn sim_reset(&mut self, cx: &mut Context<Self>) {
        if let Some(core) = self.sim.core.as_mut() {
            core.reset(core.pc);
            self.sim.status = "仿真已复位".to_string();
        }
        cx.notify();
    }

    fn sim_stop(&mut self, cx: &mut Context<Self>) {
        self.sim.core = None;
        self.sim.panel_open = false;
        self.sim.status = "仿真已停止".to_string();
        self.status = "仿真已停止".to_string();
        cx.notify();
    }

    fn ensure_memory_input(&mut self, cx: &mut Context<Self>) {
        if self.debug.memory.address_input.is_some() {
            return;
        }
        let pc_hex = self.debug.pc.map(|pc| format!("0x{pc:x}")).unwrap_or_default();
        let field = cx.new(|cx| {
            let mut field = TextField::new("例如 0x1000003e8", cx);
            if !pc_hex.is_empty() {
                field.set_text(pc_hex, cx);
            }
            field
        });
        self.debug.memory.address_input = Some(field);
    }

    /// Create the trace panel's text fields, seeded from the loaded config.
    fn ensure_trace_fields(&mut self, cx: &mut Context<Self>) {
        let trace = &mut self.debug.trace;
        if trace.address_input.is_none() {
            let text = trace.config.data_address.clone();
            let field = cx.new(|cx| {
                let mut field = TextField::new("例如 0x20000010", cx);
                if !text.is_empty() {
                    field.set_text(text, cx);
                }
                field
            });
            trace.address_input = Some(field);
        }
        if trace.prescaler_input.is_none() {
            let text = trace.config.swo_prescaler.to_string();
            let field = cx.new(|cx| {
                let mut field = TextField::new("1", cx);
                field.set_text(text, cx);
                field
            });
            trace.prescaler_input = Some(field);
        }
        if trace.port_input.is_none() {
            let text = trace.config.itm_port.to_string();
            let field = cx.new(|cx| {
                let mut field = TextField::new("0", cx);
                field.set_text(text, cx);
                field
            });
            trace.port_input = Some(field);
        }
        if trace.support.is_none() {
            let probe = find_probe(std::env::var("NS_TRACE_PROBE").ok().as_deref());
            trace.support = package_support(probe.as_ref());
        }
    }

    /// Sync the edited fields back into `config`; invalid numeric fields are
    /// left untouched so the user can keep typing.
    fn trace_sync_fields(&mut self, cx: &mut Context<Self>) {
        let trace = &mut self.debug.trace;
        if let Some(field) = &trace.address_input {
            trace.config.data_address = field.read(cx).text().trim().to_string();
        }
        if let Some(field) = &trace.prescaler_input {
            if let Ok(value) = field.read(cx).text().trim().parse::<u32>() {
                trace.config.swo_prescaler = value;
            }
        }
        if let Some(field) = &trace.port_input {
            if let Ok(value) = field.read(cx).text().trim().parse::<u8>() {
                trace.config.itm_port = value;
            }
        }
    }

    /// Save the current trace config to `.ns-ide/trace.json`.
    fn trace_save(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.project.as_ref().map(|project| project.root.clone()) else {
            return;
        };
        self.trace_sync_fields(cx);
        if let Err(error) = self.debug.trace.config.save(&root) {
            self.debug.trace.error = Some(format!("保存跟踪配置失败: {error}"));
        }
    }

    fn trace_toggle_data(&mut self, cx: &mut Context<Self>) {
        self.debug.trace.config.data_trace = !self.debug.trace.config.data_trace;
        self.debug.trace.error = None;
        self.trace_save(cx);
        cx.notify();
    }

    fn trace_cycle_mode(&mut self, cx: &mut Context<Self>) {
        self.debug.trace.config.mode = self.debug.trace.config.mode.next();
        self.debug.trace.error = None;
        self.trace_save(cx);
        cx.notify();
    }

    fn trace_arm(&mut self, cx: &mut Context<Self>) {
        let Some(commands) = self
            .debug_session
            .as_ref()
            .map(|session| session.commands.clone())
        else {
            self.debug.trace.error = Some("请先启动调试会话".to_string());
            cx.notify();
            return;
        };
        self.ensure_trace_fields(cx);
        self.trace_sync_fields(cx);
        if let Err(reason) = self.debug.trace.config.validate() {
            self.debug.trace.error = Some(reason);
            cx.notify();
            return;
        }
        self.debug.trace.error = None;
        let config = self.debug.trace.config.clone();
        let _ = commands.send(DebugCommand::ConfigureTrace(config));
        self.debug.trace.armed = true;
        self.debug.push_console("◈ 已请求启用跟踪窗口…".to_string());
        self.trace_save(cx);
        cx.notify();
    }

    fn trace_disarm(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = &self.debug_session {
            let _ = session.commands.send(DebugCommand::StopTrace);
        }
        self.debug.trace.armed = false;
        self.debug.trace.error = None;
        cx.notify();
    }

    fn set_memory_read_size(&mut self, size: u64, cx: &mut Context<Self>) {
        self.debug.memory.read_size = size;
        cx.notify();
    }

    fn read_memory(&mut self, cx: &mut Context<Self>) {
        self.ensure_memory_input(cx);
        let Some(field) = &self.debug.memory.address_input else {
            return;
        };
        let text = field.read(cx).text();
        let reference = text.trim().to_string();
        if parse_hex_address(&reference).is_none() {
            self.debug.memory.error = Some(format!("无效地址: {reference}"));
            cx.notify();
            return;
        }
        self.debug.memory.error = None;
        let count = self.debug.memory.read_size;
        if let Some(session) = &self.debug_session {
            let _ = session.commands.send(DebugCommand::ReadMemory { reference, count });
        } else {
            self.debug.memory.error = Some("调试会话未运行".to_string());
        }
        cx.notify();
    }

    fn disassemble_at_pc(&mut self, cx: &mut Context<Self>) {
        let Some(pc) = self.debug.pc else {
            self.status = "没有 PC 信息（程序未暂停）".to_string();
            cx.notify();
            return;
        };
        if let Some(session) = &self.debug_session {
            let _ = session.commands.send(DebugCommand::Disassemble {
                reference: format!("0x{pc:x}"),
                instruction_count: 24,
            });
        } else {
            self.status = "调试会话未运行".to_string();
        }
        cx.notify();
    }

    fn variable_rows(
        &self,
        variables: &[Variable],
        parent_reference: u64,
        depth: usize,
        next_id: &mut usize,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let mut rows = Vec::new();
        for variable in variables {
            let row_id = *next_id;
            *next_id += 1;
            rows.push(self.variable_row(variable, parent_reference, depth, row_id, cx));
            let reference = variable.variables_reference;
            if reference > 0 && self.debug.expanded.contains(&reference) {
                if let Some(children) = self.debug.children.get(&reference) {
                    rows.extend(self.variable_rows(children, reference, depth + 1, next_id, cx));
                }
            }
        }
        rows
    }

    fn variable_row(
        &self,
        variable: &Variable,
        parent_reference: u64,
        depth: usize,
        row_id: usize,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let reference = variable.variables_reference;
        let expandable = reference > 0;
        let expanded = expandable && self.debug.expanded.contains(&reference);
        let chevron = if expandable {
            if expanded { "▾" } else { "▸" }
        } else {
            " "
        };
        div()
            .id(("dbg-var", row_id))
            .px(px(8.))
            .py(px(3.))
            .rounded(px(6.))
            .flex()
            .items_baseline()
            .gap(px(6.))
            .pl(px(8. + depth as f32 * 14.))
            .overflow_hidden()
            .cursor_pointer()
            .when(expandable, |style| {
                style.hover(|style| style.bg(theme::alpha(theme::STROKE, 0.07)))
            })
            .when(expandable, |style| {
                style.on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_variable_expansion(reference, cx)
                }))
            })
            .child(
                div()
                    .w(px(12.))
                    .flex_none()
                    .text_size(px(9.))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(chevron),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(theme::MONO_FONT)
                    .text_size(px(10.5))
                    .text_color(rgb(theme::ACCENT_3))
                    .child(variable.name.clone()),
            )
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_size(px(10.5))
                    .text_color(rgb(theme::TEXT_DIM))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(variable.value.clone()),
            )
            .child(
                div()
                    .id(("dbg-var-eye", row_id))
                    .flex_none()
                    .ml(px(6.))
                    .text_size(px(12.))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .cursor_pointer()
                    .hover(|style| style.text_color(rgb(theme::ACCENT_3)))
                    .on_click(cx.listener({
                        let name = variable.name.clone();
                        move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.add_watchpoint_from_variable(&name, cx);
                        }
                    }))
                    .child("👁"),
            )
            .child(
                div()
                    .id(("dbg-var-edit", row_id))
                    .flex_none()
                    .ml(px(4.))
                    .text_size(px(12.))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .cursor_pointer()
                    .hover(|style| style.text_color(rgb(theme::ACCENT_3)))
                    .on_click(cx.listener({
                        let name = variable.name.clone();
                        let value = variable.value.clone();
                        move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.open_set_var_dialog(parent_reference, &name, &value, cx);
                        }
                    }))
                    .child("✎"),
            )
            .into_any_element()
    }

    fn continue_debug(&mut self, cx: &mut Context<Self>) {
        match &self.debug.status {
            DebugStatus::Stopped { .. } => {
                if let Some(session) = &self.debug_session {
                    let _ = session.commands.send(DebugCommand::Continue {
                        thread_id: self.debug.thread_id,
                    });
                }
                self.debug.status = DebugStatus::Running;
                self.debug.watchpoint_resolution_pending = false;
                self.debug.resolving_watchpoints.clear();
                self.debug.watchpoint_scope_variables.clear();
                self.breakpoints.clear_watchpoint_hits();
                self.clear_debug_highlights(cx);
                self.status = "调试运行中".to_string();
            }
            DebugStatus::Running => {
                if let Some(session) = &self.debug_session {
                    let _ = session.commands.send(DebugCommand::Pause {
                        thread_id: self.debug.thread_id,
                    });
                }
                self.status = "正在暂停…".to_string();
            }
            DebugStatus::Idle | DebugStatus::Exited => {
                self.status = "调试会话未运行".to_string();
            }
        }
        cx.notify();
    }

    fn stop_debug(&mut self, cx: &mut Context<Self>) {
        // Cancel a pending debug after build: stopping means "do not attach".
        self.pending_debug = false;
        if let Some(session) = self.debug_session.take() {
            let _ = session.commands.send(DebugCommand::Stop);
        }
        self.debug.watchpoint_resolution_pending = false;
        self.debug.resolving_watchpoints.clear();
        self.debug.watchpoint_scope_variables.clear();
        self.breakpoints.clear_watchpoint_hits();
        self.clear_debug_highlights(cx);
        self.status = "调试会话已停止".to_string();
        cx.notify();
    }

    fn debug_step(&mut self, step: DebugStep, cx: &mut Context<Self>) {
        if !matches!(self.debug.status, DebugStatus::Stopped { .. }) {
            self.status = "程序未暂停，无法单步".to_string();
            cx.notify();
            return;
        }
        let command = match step {
            DebugStep::Over => DebugCommand::StepOver { thread_id: self.debug.thread_id },
            DebugStep::In => DebugCommand::StepIn { thread_id: self.debug.thread_id },
            DebugStep::Out => DebugCommand::StepOut { thread_id: self.debug.thread_id },
        };
        if let Some(session) = &self.debug_session {
            let _ = session.commands.send(command);
        }
        self.debug.status = DebugStatus::Running;
        self.clear_debug_highlights(cx);
        cx.notify();
    }

    /// Run the debuggee until the cursor line in the active editor (DAP
    /// `continueTo` / GDB `until`).
    fn run_to_cursor(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.debug.status, DebugStatus::Stopped { .. }) {
            self.status = "程序未暂停，无法运行到光标".to_string();
            cx.notify();
            return;
        }
        let Some((path, line_0based, _)) = self.cursor_location(cx) else {
            self.status = "请将光标放在要运行到的行".to_string();
            cx.notify();
            return;
        };
        let line = line_0based + 1; // DAP/GDB use 1-based lines.
        let path_string = path.to_string_lossy().into_owned();
        if let Some(session) = &self.debug_session {
            let _ = session.commands.send(DebugCommand::ContinueTo {
                thread_id: self.debug.thread_id,
                path: path_string,
                line,
            });
        }
        self.debug.status = DebugStatus::Running;
        self.clear_debug_highlights(cx);
        self.status = format!("正在运行到 {}:{line}", path.display());
        cx.notify();
    }

    fn open_breakpoint(&mut self, path: String, line: usize, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        let full_path = project.root.join(path);
        self.open_file(full_path, cx);
        if let Some(editor) = self.active_editor() {
            editor.update(cx, |editor, cx| editor.set_cursor_line(line, cx));
        }
        cx.notify();
    }

    fn remove_breakpoint(&mut self, path: &str, line: usize, cx: &mut Context<Self>) {
        if !self.breakpoints.remove(path, line) {
            return;
        }
        self.sync_editor_breakpoints(path, cx);
        self.status = format!("已删除断点 {path}:{line}");
        self.persist_breakpoints();
        self.sync_debugger_breakpoints();
        cx.notify();
    }

    fn sync_editor_breakpoints(&mut self, path: &str, cx: &mut Context<Self>) {
        let lines = self.breakpoints.lines(path);
        for tab in &self.tabs {
            if self.breakpoint_key(&tab.path) == path {
                tab.editor
                    .update(cx, |editor, cx| editor.set_breakpoints(lines.clone(), cx));
            }
        }
    }

    fn sync_all_editor_breakpoints(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<String> = self
            .tabs
            .iter()
            .map(|tab| self.breakpoint_key(&tab.path))
            .collect();
        for path in paths {
            self.sync_editor_breakpoints(&path, cx);
        }
    }

    /// Push the current set of enabled breakpoints to the running debug session, if any.
    /// Files that no longer have breakpoints are explicitly cleared in the adapter.
    fn sync_debugger_breakpoints(&mut self) {
        if self.debug_session.is_none() {
            return;
        }
        let Some(project) = &self.project else {
            return;
        };
        let root = project.root.clone();
        let mut by_file: HashMap<String, Vec<DebugBreakpointLocation>> = HashMap::new();
        for breakpoint in self.breakpoints.all().iter().filter(|breakpoint| breakpoint.enabled) {
            let absolute = root.join(&breakpoint.path).display().to_string();
            let condition = if breakpoint.kind == ns_project::BreakpointKind::Conditional
                && !breakpoint.condition.trim().is_empty()
            {
                Some(breakpoint.condition.clone())
            } else {
                None
            };
            let hit_condition = if !breakpoint.hit_count.trim().is_empty() {
                Some(breakpoint.hit_count.clone())
            } else {
                None
            };
            let log_message = if breakpoint.kind == ns_project::BreakpointKind::Log
                && !breakpoint.log_message.trim().is_empty()
            {
                Some(breakpoint.log_message.clone())
            } else {
                None
            };
            by_file
                .entry(absolute)
                .or_default()
                .push(DebugBreakpointLocation {
                    line: breakpoint.line,
                    condition,
                    hit_condition,
                    log_message,
                    hardware: breakpoint.kind == ns_project::BreakpointKind::Hardware,
                });
        }
        for locations in by_file.values_mut() {
            locations.sort_by_key(|location| location.line);
            locations.dedup();
        }
        let mut files: Vec<String> = by_file.keys().cloned().collect();
        for previously in &self.debug.synced_files {
            if !by_file.contains_key(previously) {
                files.push(previously.clone());
            }
        }
        files.sort();
        let payload: Vec<(String, Vec<DebugBreakpointLocation>)> = files
            .into_iter()
            .map(|file| {
                let locations = by_file.get(&file).cloned().unwrap_or_default();
                (file, locations)
            })
            .collect();
        self.debug.synced_files = payload.iter().map(|(file, _)| file.clone()).collect();
        if let Some(session) = &self.debug_session {
            let _ = session.commands.send(DebugCommand::ResyncBreakpoints(payload));
        }
    }

    fn clear_debug_highlights(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            tab.editor.update(cx, |editor, cx| editor.set_debug_line(None, cx));
        }
    }

    /// Focus the editor tab that shows `path` (project-relative) at `line`.
    fn focus_source_line(&mut self, path: &str, line: usize, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        let absolute = project.root.join(path);
        if self
            .tabs
            .iter()
            .any(|tab| self.breakpoint_key(&tab.path) == path)
        {
            if let Some(editor) = self
                .tabs
                .iter()
                .find(|tab| self.breakpoint_key(&tab.path) == path)
                .and_then(|tab| self.tabs.iter().position(|t| t.path == tab.path))
                .map(|index| self.tabs[index].editor.clone())
            {
                editor.update(cx, |editor, cx| editor.set_cursor_line(line, cx));
            }
        } else if absolute.exists() {
            self.open_file(absolute, cx);
            if let Some(editor) = self.active_editor() {
                editor.update(cx, |editor, cx| editor.set_cursor_line(line, cx));
            }
        }
    }

    /// Read the source text of a line from an open tab, falling back to disk.
    fn source_line_text(&self, path: &str, line: usize, cx: &mut Context<Self>) -> Option<String> {
        let absolute = self
            .project
            .as_ref()
            .map(|project| project.root.join(path))
            .unwrap_or_else(|| PathBuf::from(path));
        for tab in &self.tabs {
            if tab.path == absolute {
                let text = tab.editor.read(cx).text().to_string();
                return text.lines().nth(line.saturating_sub(1)).map(str::to_string);
            }
        }
        fs::read_to_string(&absolute)
            .ok()
            .and_then(|text| text.lines().nth(line.saturating_sub(1)).map(str::to_string))
    }

    /// Begin a watchpoint resolution pass after a stop. Every enabled
    /// watchpoint is re-resolved because the old adapter ids are invalid once
    /// the debuggee resumes. The pass fetches each scope's variables to find
    /// where the expression lives, then asks `dataBreakpointInfo` per
    /// expression and finally arms the whole set with one `setDataBreakpoints`.
    fn begin_watchpoint_resolution(&mut self) {
        if self.debug_session.is_none() {
            return;
        }
        let enabled = self
            .breakpoints
            .watchpoints()
            .iter()
            .any(|watchpoint| watchpoint.enabled);
        if !enabled {
            if let Some(session) = &self.debug_session {
                let _ = session.commands.send(DebugCommand::SetDataBreakpoints(Vec::new()));
            }
            return;
        }
        // Invalidate previous ids; the set is re-armed once every expression
        // has a fresh data id.
        for watchpoint in self.breakpoints.watchpoints.iter_mut() {
            watchpoint.verified = None;
            watchpoint.reason.clear();
            watchpoint.hit = false;
            watchpoint.data_id.clear();
            watchpoint.adapter_id = None;
        }
        self.debug.watchpoint_scope_variables.clear();
        self.debug.resolving_watchpoints.clear();
        self.debug.watchpoint_resolution_pending = true;
        // Fetch the variables of every scope to locate each expression.
        let scopes = self.debug.scopes.clone();
        for scope in &scopes {
            if let Some(session) = &self.debug_session {
                let _ = session.commands.send(DebugCommand::GetVariables {
                    variables_reference: scope.id,
                });
            }
        }
    }

    /// Called from the `Variables` handler while a resolution pass is active:
    /// remember the scope's variables, then try to resolve the expressions it
    /// may contain.
    fn collect_watchpoint_scope(&mut self, reference: u64, variables: Vec<Variable>) {
        if !self.debug.watchpoint_resolution_pending {
            return;
        }
        self.debug
            .watchpoint_scope_variables
            .insert(reference, variables);
        self.try_resolve_watchpoints();
    }

    /// Ask `dataBreakpointInfo` for every enabled watchpoint once every scope
    /// has been fetched, so each expression is resolved in the scope that
    /// actually lists it (falling back to the global scope id `0`).
    fn try_resolve_watchpoints(&mut self) {
        if !self.debug.watchpoint_resolution_pending {
            return;
        }
        let expressions: Vec<String> = self
            .breakpoints
            .watchpoints()
            .iter()
            .filter(|watchpoint| watchpoint.enabled)
            .map(|watchpoint| watchpoint.expression.clone())
            .collect();
        if expressions.is_empty() {
            self.debug.watchpoint_resolution_pending = false;
            return;
        }
        // Wait until every scope has been fetched so the expression-to-scope
        // mapping is complete before any `dataBreakpointInfo` request.
        let all_scopes_fetched = self.debug.scopes.iter().all(|scope| {
            self.debug
                .watchpoint_scope_variables
                .contains_key(&scope.id)
        });
        if !all_scopes_fetched {
            return;
        }
        let scopes = self.debug.scopes.clone();
        for expression in expressions {
            if self.debug.resolving_watchpoints.contains(&expression) {
                continue;
            }
            let scope = scopes
                .iter()
                .find(|scope| {
                    self.debug
                        .watchpoint_scope_variables
                        .get(&scope.id)
                        .map(|variables| variables.iter().any(|v| v.name == expression))
                        .unwrap_or(false)
                })
                .map(|scope| scope.id)
                .unwrap_or(0);
            self.debug.resolving_watchpoints.insert(expression.clone());
            if let Some(session) = &self.debug_session {
                let _ = session.commands.send(DebugCommand::DataBreakpointInfo {
                    expression,
                    scope,
                });
            }
        }
    }

    /// Store a resolved `dataId` on its watchpoint. When every enabled
    /// expression has been answered, arm the whole set with the adapter.
    fn resolve_watchpoint(
        &mut self,
        expression: String,
        data_id: Option<String>,
        _access_types: Vec<String>,
        description: String,
    ) {
        self.debug.resolving_watchpoints.remove(&expression);
        match data_id {
            Some(data_id) => {
                self.breakpoints.set_watchpoint_data_id(&expression, &data_id);
                self.breakpoints.mark_watchpoint_verified(&expression, true, "", None);
                self.debug.push_console(format!(
                    "◈ 观察点 `{expression}` 已解析（{description}）"
                ));
            }
            None => {
                self.breakpoints.mark_watchpoint_verified(
                    &expression,
                    false,
                    &format!("无法解析: {description}"),
                    None,
                );
                self.debug.push_console(format!(
                    "✕ 观察点 `{expression}` 无法解析: {description}"
                ));
            }
        }
        // Arm once every enabled expression has been answered.
        let all_answered = self
            .breakpoints
            .watchpoints()
            .iter()
            .filter(|watchpoint| watchpoint.enabled)
            .all(|watchpoint| {
                watchpoint.verified.is_some()
                    && !self.debug.resolving_watchpoints.contains(&watchpoint.expression)
            });
        if all_answered {
            self.debug.watchpoint_resolution_pending = false;
            self.send_armed_watchpoints();
        }
    }

    /// Build the `setDataBreakpoints` payload from the currently resolved
    /// watchpoints and send it to the adapter.
    fn send_armed_watchpoints(&mut self) {
        if self.debug_session.is_none() {
            return;
        }
        let requests: Vec<WatchpointRequest> = self
            .breakpoints
            .watchpoints()
            .iter()
            .filter(|watchpoint| watchpoint.enabled && watchpoint.verified == Some(true))
            .map(|watchpoint| WatchpointRequest {
                expression: watchpoint.expression.clone(),
                data_id: watchpoint.data_id.clone(),
                access_type: watchpoint.access.dap_value().to_string(),
            })
            .collect();
        if let Some(session) = &self.debug_session {
            let _ = session.commands.send(DebugCommand::SetDataBreakpoints(requests));
        }
    }

    fn toggle_breakpoint_enabled(&mut self, path: &str, line: usize, cx: &mut Context<Self>) {
        let Some(breakpoint) = self.breakpoints.find(path, line) else {
            return;
        };
        let enabled = !breakpoint.enabled;
        self.breakpoints.set(path, line, enabled);
        self.sync_editor_breakpoints(path, cx);
        self.status = if enabled {
            format!("已启用断点 {path}:{line}")
        } else {
            format!("已禁用断点 {path}:{line}")
        };
        self.persist_breakpoints();
        self.sync_debugger_breakpoints();
        cx.notify();
    }
    fn toggle_breakpoint_hardware(&mut self, path: &str, line: usize, cx: &mut Context<Self>) {
        let Some(hardware) = self.breakpoints.set_hardware(path, line) else {
            self.status = "该行没有断点，无法切换硬件断点".to_string();
            cx.notify();
            return;
        };
        self.sync_editor_breakpoints(path, cx);
        self.status = if hardware {
            format!("已改为硬件断点 {path}:{line}")
        } else {
            format!("已改为软件断点 {path}:{line}")
        };
        self.persist_breakpoints();
        self.sync_debugger_breakpoints();
        cx.notify();
    }

    fn clear_all_breakpoints(&mut self, cx: &mut Context<Self>) {
        if self.breakpoints.is_empty() {
            return;
        }
        let count = self.breakpoints.len();
        self.breakpoints.clear();
        self.sync_all_editor_breakpoints(cx);
        self.status = format!("已清除全部 {count} 个断点");
        self.persist_breakpoints();
        self.sync_debugger_breakpoints();
        cx.notify();
    }

    fn add_breakpoint(&mut self, path: &str, line: usize, cx: &mut Context<Self>) {
        self.breakpoints.set(path, line, true);
        self.sync_editor_breakpoints(path, cx);
        self.status = format!("已添加断点 {path}:{line}");
        self.persist_breakpoints();
        self.sync_debugger_breakpoints();
        self.context_menu = None;
        cx.notify();
    }

    fn clear_file_breakpoints(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.breakpoints.all().iter().all(|breakpoint| breakpoint.path != path) {
            self.context_menu = None;
            cx.notify();
            return;
        }
        self.breakpoints.clear_file(path);
        self.sync_editor_breakpoints(path, cx);
        self.status = format!("已清除 {path} 中的断点");
        self.persist_breakpoints();
        self.sync_debugger_breakpoints();
        self.context_menu = None;
        cx.notify();
    }

    fn open_condition_dialog(&mut self, path: &str, line: usize, cx: &mut Context<Self>) {
        let existing = self
            .breakpoints
            .find(path, line)
            .map(|breakpoint| breakpoint.condition.clone())
            .unwrap_or_default();
        let field = cx.new(|cx| {
            let mut field = TextField::new("例如 count == 10", cx);
            if !existing.trim().is_empty() {
                field.set_text(existing, cx);
            }
            field
        });
        self.condition_dialog = Some(ConditionDialog {
            path: path.to_string(),
            line,
            field,
        });
        self.context_menu = None;
        self.focus_dialog_on_next_frame = true;
        cx.notify();
    }

    fn save_condition_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.condition_dialog.take() else {
            return;
        };
        let condition = dialog.field.read(cx).text();
        self.breakpoints
            .set_condition(&dialog.path, dialog.line, &condition);
        self.sync_editor_breakpoints(&dialog.path, cx);
        let trimmed = condition.trim().to_string();
        self.status = if trimmed.is_empty() {
            format!("已清除断点 {}:{} 的条件", dialog.path, dialog.line)
        } else {
            format!("已设置断点 {}:{} 的条件：{trimmed}", dialog.path, dialog.line)
        };
        self.persist_breakpoints();
        self.sync_debugger_breakpoints();
        cx.notify();
    }

    /// Open the project settings modal, prefilled from `project.toml`. Only
    /// available when a project is loaded.
    fn open_settings(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开或新建项目".to_string();
            cx.notify();
            return;
        };
        let config = project.config.clone();
        let global_keys = ns_project::ConfigKeys::load();
        let keys_text = global_keys
            .keys
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("\n");
        let settings = SettingsDialog {
            name: settings_field("项目名称", config.project.name, cx),
            version: settings_field("项目版本", config.project.version, cx),
            language: settings_field("C | C++ | Rust | Zig | ZR", config.project.language.to_string(), cx),
            target: settings_field("目标 MCU，例如 stm32f103c8", config.project.target, cx),
            svd: settings_field(
                "SVD 设备描述（内置目录键或 .svd 文件路径，留空=按 target 推导）",
                config.project.svd,
                cx,
            ),
            project_compiler: settings_field("项目编译器（如 arm-none-eabi-gcc）", config.project.compiler, cx),
            project_simulator: settings_field("仿真器（如 ns-sim）", config.project.simulator, cx),
            project_debugger: settings_field("调试器（如 gdb）", config.project.debugger, cx),
            project_flasher: settings_field("烧写器（如 openocd）", config.project.flasher, cx),
            compiler: settings_field(
                "编译器路径（留空=从 PATH 发现，支持 $config[\"key\"]）",
                config.compiler.path,
                cx,
            ),
            compiler_std: settings_field("编译标准（如 c11 / gnu17）", config.compiler.std, cx),
            compiler_opt: settings_field("优化选项（如 -O2）", config.compiler.opt, cx),
            compiler_defines: settings_field("宏定义（逗号分隔，如 FOO=1,BAR）", config.compiler.defines.join(","), cx),
            compiler_includes: settings_field("头文件目录（逗号分隔）", config.compiler.includes.join(","), cx),
            build_config: settings_field("debug | release", config.build.config, cx),
            out_dir: settings_field("输出目录", config.build.out_dir, cx),
            debugger: settings_field(
                "调试器路径（留空=自动发现，支持 $config[\"key\"]）",
                config.debugger.path,
                cx,
            ),
            debugger_interface: settings_field("调试接口（swd | jtag）", config.debugger.interface, cx),
            debugger_server: settings_field("调试服务器/探针路径（可选）", config.debugger.server, cx),
            debugger_probe: settings_field(
                "调试探针（openocd | stlink | jlink | pyocd | probe-rs）",
                config.debugger.probe,
                cx,
            ),
            debugger_gdb: settings_field("GDB 客户端路径（可选）", config.debugger.gdb, cx),
            flasher: settings_field(
                "烧写器路径（留空=自动发现，支持 $config[\"key\"]）",
                config.flasher.path,
                cx,
            ),
            flasher_kind: settings_field(
                "openocd | pyocd | jlink | stlink | probe-rs",
                config.flasher.kind,
                cx,
            ),
            flasher_interface: settings_field("烧写接口（swd | jtag）", config.flasher.interface, cx),
            sim_cpu: settings_field("仿真 CPU（可选）", config.sim.cpu, cx),
            sim_period: settings_field("仿真周期（周期数，0=默认）", config.sim.period.to_string(), cx),
            terminal_shell: settings_field(
                format!(
                    "终端 shell（留空=默认 {}）",
                    ns_project::terminal_label(&ns_project::default_terminal_shell())
                ),
                ns_project::TerminalSettings::load().shell,
                cx,
            ),
            config_keys: settings_field(
                "全局配置键（每行 key=value，供 $config[\"key\"] 引用）",
                keys_text,
                cx,
            ),
        };
        self.settings_dialog = Some(settings);
        self.focus_dialog_on_next_frame = true;
        cx.notify();
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.settings_dialog.take() else {
            return;
        };
        let Some(project) = self.project.as_mut() else {
            self.status = "没有已加载的项目".to_string();
            cx.notify();
            return;
        };
        let name = dialog.name.read(cx).text();
        let version = dialog.version.read(cx).text();
        let language = dialog.language.read(cx).text();
        let target = dialog.target.read(cx).text();
        let svd = dialog.svd.read(cx).text();
        let project_compiler = dialog.project_compiler.read(cx).text();
        let project_simulator = dialog.project_simulator.read(cx).text();
        let project_debugger = dialog.project_debugger.read(cx).text();
        let project_flasher = dialog.project_flasher.read(cx).text();
        let compiler = dialog.compiler.read(cx).text();
        let compiler_std = dialog.compiler_std.read(cx).text();
        let compiler_opt = dialog.compiler_opt.read(cx).text();
        let compiler_defines = dialog.compiler_defines.read(cx).text();
        let compiler_includes = dialog.compiler_includes.read(cx).text();
        let build_config = dialog.build_config.read(cx).text();
        let out_dir = dialog.out_dir.read(cx).text();
        let debugger = dialog.debugger.read(cx).text();
        let debugger_interface = dialog.debugger_interface.read(cx).text();
        let debugger_server = dialog.debugger_server.read(cx).text();
        let debugger_probe = dialog.debugger_probe.read(cx).text();
        let debugger_gdb = dialog.debugger_gdb.read(cx).text();
        let flasher = dialog.flasher.read(cx).text();
        let flasher_kind = dialog.flasher_kind.read(cx).text();
        let flasher_interface = dialog.flasher_interface.read(cx).text();
        let sim_cpu = dialog.sim_cpu.read(cx).text();
        let sim_period = dialog.sim_period.read(cx).text();

        // ---- Validation ----
        if name.trim().is_empty() {
            self.status = "项目名称不能为空".to_string();
            cx.notify();
            return;
        }
        if !matches!(build_config.trim(), "debug" | "release") {
            self.status = "构建配置必须为 debug 或 release".to_string();
            cx.notify();
            return;
        }
        let kind = flasher_kind.trim().to_ascii_lowercase();
        if !matches!(kind.as_str(), "" | "openocd" | "pyocd" | "jlink" | "stlink" | "probe-rs") {
            self.status = "烧写器类型必须为 openocd / pyocd / jlink / stlink / probe-rs".to_string();
            cx.notify();
            return;
        }
        let probe = debugger_probe.trim().to_ascii_lowercase();
        if !matches!(probe.as_str(), "" | "openocd" | "stlink" | "jlink" | "pyocd" | "probe-rs") {
            self.status = "调试探针必须为 openocd / stlink / jlink / pyocd / probe-rs".to_string();
            cx.notify();
            return;
        }
        let parsed_language = language.trim().parse::<Language>();
        let Ok(parsed_language) = parsed_language else {
            self.status = format!("语言必须为 C / C++ / Rust / Zig / ZR（当前：{language}）");
            cx.notify();
            return;
        };
        let period = sim_period.trim().parse::<u64>().unwrap_or(0);

        // ---- Write the whole config (project + compiler + debugger + flasher + build + sim) ----
        project.config.project.name = name;
        project.config.project.version = version;
        project.config.project.language = parsed_language;
        project.config.project.target = target;
        project.config.project.svd = svd;
        project.config.project.compiler = project_compiler;
        project.config.project.simulator = project_simulator;
        project.config.project.debugger = project_debugger;
        project.config.project.flasher = project_flasher;
        project.config.compiler.path = compiler;
        project.config.compiler.std = compiler_std;
        project.config.compiler.opt = compiler_opt;
        project.config.compiler.defines = split_csv(&compiler_defines);
        project.config.compiler.includes = split_csv(&compiler_includes);
        project.config.build.config = build_config;
        project.config.build.out_dir = out_dir;
        project.config.debugger.path = debugger;
        project.config.debugger.interface = debugger_interface;
        project.config.debugger.server = debugger_server;
        project.config.debugger.probe = probe;
        project.config.debugger.gdb = debugger_gdb;
        project.config.flasher.path = flasher;
        project.config.flasher.kind = kind;
        project.config.flasher.interface = flasher_interface;
        project.config.sim.cpu = sim_cpu;
        project.config.sim.period = period;

        // ---- Global terminal shell (terminal.json) ----
        let terminal_shell = dialog.terminal_shell.read(cx).text();
        let mut term_settings = ns_project::TerminalSettings::load();
        let term_changed = term_settings.shell != terminal_shell;
        term_settings.shell = terminal_shell;
        if term_changed {
            if let Err(error) = term_settings.save() {
                self.status = format!("保存终端设置失败: {error}");
                cx.notify();
                return;
            }
        }

        // ---- Global config keys (config.json, `$config["key"]`) ----
        let keys_text = dialog.config_keys.read(cx).text();
        let mut global_keys = ns_project::ConfigKeys::load();
        let mut new_keys = std::collections::BTreeMap::new();
        for line in keys_text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                new_keys.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
        let keys_changed = new_keys != global_keys.keys;
        global_keys.keys = new_keys;
        if keys_changed {
            if let Err(error) = global_keys.save() {
                self.status = format!("保存全局配置键失败: {error}");
                cx.notify();
                return;
            }
        }

        match project.save_config() {
            Ok(()) => {
                self.status = "已保存配置到 project.toml".to_string();
            }
            Err(error) => {
                self.status = format!("保存 project.toml 失败: {error}");
            }
        }
        cx.notify();
    }

    fn cancel_settings(&mut self, cx: &mut Context<Self>) {
        if self.settings_dialog.take().is_some() {
            self.status = "已取消设置编辑".to_string();
            cx.notify();
        }
    }

    /// Open the global toolchain path-override dialog. Overrides are stored in
    /// the user config dir and take precedence over PATH discovery.
    fn open_toolchain(&mut self, cx: &mut Context<Self>) {
        let config = ns_project::ToolchainConfig::load();
        let tool_names: &[&str] = &[
            "arm-none-eabi-gcc",
            "arm-none-eabi-gdb",
            "clang",
            "gcc",
            "rustc",
            "zig",
            "openocd",
            "pyocd",
            "JLinkExe",
        ];
        let fields = tool_names
            .iter()
            .map(|name| {
                let value = config.override_path(name).unwrap_or_default();
                let field = settings_field(
                    "留空=自动发现",
                    value.to_string_lossy().into_owned(),
                    cx,
                );
                (*name, field)
            })
            .collect();
        self.toolchain_dialog = Some(ToolchainDialog { fields });
        self.focus_dialog_on_next_frame = true;
        cx.notify();
    }

    fn save_toolchain(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.toolchain_dialog.take() else {
            return;
        };
        let mut config = ns_project::ToolchainConfig::load();
        for (name, field) in &dialog.fields {
            config.set_override(name, &field.read(cx).text());
        }
        match config.save() {
            Ok(()) => self.status = "已保存全局工具链路径".to_string(),
            Err(error) => self.status = format!("保存工具链配置失败: {error}"),
        }
        cx.notify();
    }

    fn cancel_toolchain(&mut self, cx: &mut Context<Self>) {
        if self.toolchain_dialog.take().is_some() {
            self.status = "已取消工具链编辑".to_string();
            cx.notify();
        }
    }

    /// Open the watchpoint dialog. `initial` prefills the expression (e.g. the
    /// identifier under the cursor); when `expression` is non-empty it edits an
    /// existing watchpoint instead of adding one.
    fn open_watchpoint_dialog(
        &mut self,
        expression: &str,
        initial: &str,
        cx: &mut Context<Self>,
    ) {
        let existing = if expression.is_empty() {
            None
        } else {
            self.breakpoints.find_watchpoint(expression)
        };
        let prefill = existing
            .map(|watchpoint| watchpoint.expression.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| initial.trim().to_string());
        let access = existing.map(|watchpoint| watchpoint.access).unwrap_or_default();
        let field = cx.new(|cx| {
            let mut field = TextField::new("例如 marker 或 obj.field", cx);
            if !prefill.is_empty() {
                field.set_text(prefill, cx);
            }
            field
        });
        self.watchpoint_dialog = Some(WatchpointDialog {
            original: existing.map(|watchpoint| watchpoint.expression.clone()).unwrap_or_default(),
            field,
            access,
        });
        self.context_menu = None;
        self.focus_dialog_on_next_frame = true;
        cx.notify();
    }

    fn save_watchpoint_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.watchpoint_dialog.take() else {
            return;
        };
        let expression = dialog.field.read(cx).text();
        let expression = expression.trim().to_string();
        if expression.is_empty() {
            self.status = "观察点表达式不能为空".to_string();
            cx.notify();
            return;
        }
        // Editing an existing watchpoint renames it: remove the old key first.
        if !dialog.original.is_empty() && dialog.original != expression {
            self.breakpoints.remove_watchpoint(&dialog.original);
        }
        let added = self
            .breakpoints
            .add_watchpoint(&expression, dialog.access);
        self.status = if added {
            format!("已添加观察点 `{expression}`")
        } else {
            format!("观察点 `{expression}` 已存在")
        };
        self.persist_breakpoints();
        self.sync_watchpoints_if_running();
        cx.notify();
    }

    fn cancel_watchpoint_dialog(&mut self, cx: &mut Context<Self>) {
        if self.watchpoint_dialog.take().is_some() {
            self.status = "已取消观察点编辑".to_string();
            cx.notify();
        }
    }

    /// Open the "set variable value" dialog for a variable in the debug panel.
    fn open_set_var_dialog(
        &mut self,
        reference: u64,
        name: &str,
        current: &str,
        cx: &mut Context<Self>,
    ) {
        let current = current.to_string();
        let field = cx.new(move |cx| {
            let mut field = TextField::new("新值", cx);
            if !current.is_empty() {
                field.set_text(&current, cx);
            }
            field
        });
        self.set_var_dialog = Some(SetVarDialog {
            reference,
            name: name.to_string(),
            field,
        });
        self.context_menu = None;
        self.focus_dialog_on_next_frame = true;
        cx.notify();
    }

    fn save_set_var_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.set_var_dialog.take() else {
            return;
        };
        let value = dialog.field.read(cx).text().trim().to_string();
        if value.is_empty() {
            self.status = "变量值不能为空".to_string();
            cx.notify();
            return;
        }
        if let Some(session) = &self.debug_session {
            let _ = session.commands.send(DebugCommand::SetVariable {
                variables_reference: dialog.reference,
                name: dialog.name.clone(),
                value: value.clone(),
            });
        }
        self.status = format!("正在设置 {} = {value}", dialog.name);
        cx.notify();
    }

    fn cancel_set_var_dialog(&mut self, cx: &mut Context<Self>) {
        if self.set_var_dialog.take().is_some() {
            self.status = "已取消变量值编辑".to_string();
            cx.notify();
        }
    }

    fn cycle_watchpoint_access(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.watchpoint_dialog {
            dialog.access = dialog.access.next();
            cx.notify();
        }
    }

    fn remove_watchpoint(&mut self, expression: &str, cx: &mut Context<Self>) {
        if !self.breakpoints.remove_watchpoint(expression) {
            return;
        }
        self.status = format!("已删除观察点 `{expression}`");
        self.persist_breakpoints();
        self.sync_watchpoints_if_running();
        cx.notify();
    }

    fn toggle_watchpoint_enabled(&mut self, expression: &str, cx: &mut Context<Self>) {
        let Some(enabled) = self
            .breakpoints
            .find_watchpoint(expression)
            .map(|watchpoint| !watchpoint.enabled)
        else {
            return;
        };
        self.breakpoints.set_watchpoint_enabled(expression, enabled);
        self.status = if enabled {
            format!("已启用观察点 `{expression}`")
        } else {
            format!("已禁用观察点 `{expression}`")
        };
        self.persist_breakpoints();
        self.sync_watchpoints_if_running();
        cx.notify();
    }

    /// Add a watchpoint from the variables panel (`👁`).
    fn add_watchpoint_from_variable(&mut self, expression: &str, cx: &mut Context<Self>) {
        let expression = expression.trim().to_string();
        if expression.is_empty() {
            return;
        }
        self.breakpoints.add_watchpoint(&expression, WatchAccess::default());
        self.status = format!("已添加观察点 `{expression}`");
        self.persist_breakpoints();
        self.sync_watchpoints_if_running();
        cx.notify();
    }

    /// When a debug session is running, re-resolve and re-arm watchpoints.
    fn sync_watchpoints_if_running(&mut self) {
        if self.debug_session.is_none() {
            return;
        }
        self.begin_watchpoint_resolution();
    }

    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.is_some() {
            if let Some(search) = &self.search {
                window.focus(&search.field.read(cx).focus_handle(cx));
            }
            return;
        }
        let initial = self
            .active_editor()
            .map(|editor| editor.read(cx).selected_text())
            .unwrap_or_default();
        let initial = initial.trim().to_string();
        let initial = if initial.contains('\n') {
            String::new()
        } else {
            initial
        };

        let field = cx.new(|cx| {
            let mut field = TextField::new("搜索项目…", cx);
            if !initial.is_empty() {
                field.set_text(initial, cx);
            }
            field
        });
        let subscription = cx.observe(&field, |this, field, cx| {
            this.refresh_search(field, cx);
        });
        self.search = Some(SearchState {
            field: field.clone(),
            matches: Vec::new(),
            selected: 0,
            _subscription: subscription,
        });
        window.focus(&field.read(cx).focus_handle(cx));
        if !field.read(cx).text().is_empty() {
            self.refresh_search(field.clone(), cx);
        }
        cx.notify();
    }

    fn matches_for(&self, query: &str) -> Option<Vec<SearchMatch>> {
        let project = self.project.as_ref()?;
        Some(search::search_project(&project.root, query, search::MAX_MATCHES))
    }

    fn refresh_search(&mut self, field: Entity<TextField>, cx: &mut Context<Self>) {
        let query = field.read(cx).text();
        let matches = self.matches_for(&query).unwrap_or_default();
        if let Some(search) = &mut self.search {
            search.matches = matches;
            search.selected = 0;
        }
        cx.notify();
    }

    fn close_search(&mut self, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            self.status = "已关闭搜索".to_string();
            cx.notify();
        }
    }

    fn confirm_search(&mut self, cx: &mut Context<Self>) {
        let index = self
            .search
            .as_ref()
            .map(|search| search.selected)
            .unwrap_or(0);
        self.open_search_result(index, cx);
    }

    fn move_search_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else {
            return;
        };
        if search.matches.is_empty() {
            return;
        }
        let len = search.matches.len() as isize;
        let next = (search.selected as isize + delta).rem_euclid(len);
        search.selected = next as usize;
        cx.notify();
    }

    fn open_search_result(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(found) = self
            .search
            .as_ref()
            .and_then(|search| search.matches.get(index))
            .cloned()
        else {
            return;
        };
        let Some(root) = self.project.as_ref().map(|project| project.root.clone()) else {
            return;
        };
        let path = root.join(&found.path);
        if !path.exists() {
            self.status = format!("文件不存在: {}", found.path);
            self.search = None;
            cx.notify();
            return;
        }
        self.open_file(path, cx);
        if let Some(editor) = self.active_editor() {
            editor.update(cx, |editor, cx| editor.set_cursor_line(found.line, cx));
        }
        self.status = format!("搜索跳转 {}:{}", found.path, found.line);
        self.search = None;
        cx.notify();
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            if let Some(palette) = &self.palette {
                window.focus(&palette.field.read(cx).focus_handle(cx));
            }
            return;
        }
        let field = cx.new(|cx| TextField::new("输入命令…", cx));
        let ranked = palette::rank("");
        let subscription = cx.observe(&field, |this, field, cx| {
            this.refresh_palette(field, cx);
        });
        self.palette = Some(PaletteState {
            field: field.clone(),
            ranked,
            selected: 0,
            _subscription: subscription,
        });
        window.focus(&field.read(cx).focus_handle(cx));
        cx.notify();
    }

    fn refresh_palette(&mut self, field: Entity<TextField>, cx: &mut Context<Self>) {
        let query = field.read(cx).text();
        let ranked = palette::rank(&query);
        if let Some(palette) = &mut self.palette {
            palette.ranked = ranked;
            palette.selected = 0;
        }
        cx.notify();
    }

    fn close_palette(&mut self, cx: &mut Context<Self>) {
        if self.palette.take().is_some() {
            cx.notify();
        }
    }

    fn move_palette_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(palette) = &mut self.palette else {
            return;
        };
        if palette.ranked.is_empty() {
            return;
        }
        let len = palette.ranked.len() as isize;
        let next = (palette.selected as isize + delta).rem_euclid(len);
        palette.selected = next as usize;
        cx.notify();
    }

    fn confirm_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(palette) = &self.palette else {
            return;
        };
        let Some(index) = palette.ranked.get(palette.selected).copied() else {
            return;
        };
        let id = palette::COMMANDS[index].id;
        self.close_palette(cx);
        self.execute_palette_command(id, window, cx);
    }

    fn execute_palette_command(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            "new-project" => self.open_wizard(window, cx),
            "open-project" => self.open_project_dialog(cx),
            "save" => self.save_active(cx),
            "build" => self.start_build(false, cx),
            "run" => self.start_build(true, cx),
            "restart-run" => self.restart_run(cx),
            "stop-run" => self.stop_run(cx),
            "debug" => self.start_debug(cx),
            "embedded-debug" => self.start_embedded_debug(cx),
            "stop-debug" => self.stop_debug(cx),
            "run-to-cursor" => self.run_to_cursor(cx),
            "find" => self.open_search(window, cx),
            "toggle-output" => {
                self.show_output = !self.show_output;
                cx.notify();
            }
            "breakpoints" => self.show_breakpoints(cx),
            "git" => self.show_git(cx),
            "static-analysis" => self.start_static_analysis(cx),
            "terminal" => self.show_terminal(cx),
            "serial-monitor" => self.show_serial_monitor(cx),
            "agent" => self.show_agent_panel(cx),
            "settings" => self.open_settings(cx),
            "flash" => self.start_flash(cx),
            "flash-erase" => self.flash_action(FlashAction::Erase, cx),
            "flash-verify" => self.flash_action(FlashAction::Verify, cx),
            "flash-read" => self.flash_action(FlashAction::Read, cx),
            "flash-and-run" => self.flash_and_run(cx),
            "flash-and-debug" => self.flash_and_debug(cx),
            "toolchain" => self.open_toolchain(cx),
            "simulation" => self.start_simulation(cx),
            "clear-breakpoints" => self.clear_all_breakpoints(cx),
            "add-watchpoint" => {
                let initial = self
                    .active_editor()
                    .map(|editor| editor.read(cx).word_under_cursor())
                    .unwrap_or_default();
                self.open_watchpoint_dialog("", &initial, cx);
            }
            "close-tab" => {
                if let Some(index) = self.active_tab {
                    self.close_tab(index, cx);
                }
            }
            "toggle-diagnostics" => {
                self.show_output = true;
                self.output_tab = OutputTab::Problems;
                cx.notify();
            }
            "focus-next-tab" => self.next_tab(true, cx),
            "focus-prev-tab" => self.next_tab(false, cx),
            "go-to-definition" => self.lsp_go_to_definition(cx),
            "find-references" => self.lsp_find_references(cx),
            "hover" => self.lsp_hover(cx),
            "signature-help" => self.lsp_signature_help(cx),
            "inlay-hints" => self.lsp_inlay_hints(cx),
            "semantic-tokens" => self.lsp_semantic_tokens(cx),
            "show-completion" => self.lsp_completion(cx),
            _ => {
                self.status = format!("未知命令 {id}");
                cx.notify();
            }
        }
    }

    /// Resolve the definition under the cursor and jump to it (LSP).
    fn lsp_go_to_definition(&mut self, cx: &mut Context<Self>) {
        let Some((path, line, column)) = self.cursor_location(cx) else {
            self.status = "没有活动编辑器".to_string();
            cx.notify();
            return;
        };
        let Some(server) = &self.lsp else {
            self.status = "语言服务器未连接（检查是否安装了 clangd / rust-analyzer / zls）".to_string();
            cx.notify();
            return;
        };
        let Ok(rx) = server.definition(path.clone(), line, column) else {
            self.status = "无法请求定义（语言服务器不可用）".to_string();
            cx.notify();
            return;
        };
        self.status = "正在解析定义…".to_string();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let response = rx
                .recv_timeout(std::time::Duration::from_secs(8))
                .ok();
            this.update(cx, |this, cx| {
                match response {
                    Some(response) => {
                        if let Some(location) = parse_definition_location(&response) {
                            let target_path = location.path.clone();
                            let target_line = location.line;
                            this.open_file(target_path, cx);
                            if let Some(editor) = this.active_editor() {
                                editor.update(cx, |editor, cx| {
                                    editor.set_cursor_line(target_line, cx)
                                });
                            }
                            this.status = format!("已跳转到定义（{}:{}）", location.path.display(), location.line);
                        } else {
                            this.status = "未找到定义".to_string();
                        }
                    }
                    None => this.status = "定义请求超时".to_string(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Find references under the cursor (LSP) and report the count.
    fn lsp_find_references(&mut self, cx: &mut Context<Self>) {
        let Some((path, line, column)) = self.cursor_location(cx) else {
            self.status = "没有活动编辑器".to_string();
            cx.notify();
            return;
        };
        let Some(server) = &self.lsp else {
            self.status = "语言服务器未连接（检查是否安装了 clangd / rust-analyzer / zls）".to_string();
            cx.notify();
            return;
        };
        let Ok(rx) = server.references(path.clone(), line, column) else {
            self.status = "无法请求引用（语言服务器不可用）".to_string();
            cx.notify();
            return;
        };
        self.status = "正在查找引用…".to_string();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let response = rx
                .recv_timeout(std::time::Duration::from_secs(8))
                .ok();
            this.update(cx, |this, cx| {
                match response {
                    Some(response) => {
                        let locations = parse_location_list(&response);
                        if locations.is_empty() {
                            this.status = "未找到引用".to_string();
                        } else {
                            let count = locations.len();
                            this.references = locations;
                            this.status = format!("找到 {count} 处引用（Shift+F12 查看）");
                        }
                    }
                    None => this.status = "引用请求超时".to_string(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Show the hover tooltip at the cursor (LSP) in the status bar.
    fn lsp_hover(&mut self, cx: &mut Context<Self>) {
        let Some((path, line, column)) = self.cursor_location(cx) else {
            self.status = "没有活动编辑器".to_string();
            cx.notify();
            return;
        };
        let Some(server) = &self.lsp else {
            self.status = "语言服务器未连接（检查是否安装了 clangd / rust-analyzer / zls）".to_string();
            cx.notify();
            return;
        };
        let Ok(rx) = server.hover(path.clone(), line, column) else {
            self.status = "无法请求悬停提示（语言服务器不可用）".to_string();
            cx.notify();
            return;
        };
        self.status = "正在获取悬停提示…".to_string();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let response = rx
                .recv_timeout(std::time::Duration::from_secs(8))
                .ok();
            this.update(cx, |this, cx| {
                match response {
                    Some(response) => {
                        let text = parse_hover_text(&response);
                        if text.is_empty() {
                            this.status = "无悬停提示".to_string();
                        } else {
                            this.status = text;
                        }
                    }
                    None => this.status = "悬停提示请求超时".to_string(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Request signature help at the cursor (LSP) and show it in the status bar.
    fn lsp_signature_help(&mut self, cx: &mut Context<Self>) {
        let Some((path, line, column)) = self.cursor_location(cx) else {
            self.status = "没有活动编辑器".to_string();
            cx.notify();
            return;
        };
        let Some(server) = &self.lsp else {
            self.status = "语言服务器未连接（检查是否安装了 clangd / rust-analyzer / zls）".to_string();
            cx.notify();
            return;
        };
        let Ok(rx) = server.signature_help(path.clone(), line, column) else {
            self.status = "无法请求签名帮助（语言服务器不可用）".to_string();
            cx.notify();
            return;
        };
        self.status = "正在获取签名帮助…".to_string();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let response = rx
                .recv_timeout(std::time::Duration::from_secs(8))
                .ok();
            this.update(cx, |this, cx| {
                match response {
                    Some(response) => {
                        match parse_signature_help(&response) {
                            Some(help) => {
                                this.signature_help = Some(help);
                                this.status = "签名帮助已就绪".to_string();
                            }
                            None => {
                                this.signature_help = None;
                                this.status = "无签名帮助".to_string();
                            }
                        }
                    }
                    None => this.status = "签名帮助请求超时".to_string(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Request semantic tokens for the active file (LSP) and cache them.
    fn lsp_semantic_tokens(&mut self, cx: &mut Context<Self>) {
        let Some((path, _, _)) = self.cursor_location(cx) else {
            return;
        };
        let Some(server) = &self.lsp else {
            return;
        };
        let Ok(rx) = server.semantic_tokens(path) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let response = rx
                .recv_timeout(std::time::Duration::from_secs(8))
                .ok();
            this.update(cx, |this, cx| {
                if let Some(response) = response {
                    let tokens = parse_semantic_tokens(&response);
                    this.semantic_tokens = tokens.clone();
                    if let Some(editor) = this.active_editor() {
                        let spans: Vec<crate::editor::SemanticSpan> = tokens
                            .into_iter()
                            .map(|token| crate::editor::SemanticSpan {
                                line: token.line,
                                start: token.start,
                                length: token.length,
                                token_type: token.token_type,
                            })
                            .collect();
                        editor.update(cx, |editor, cx| {
                            editor.set_semantic_tokens(spans, cx);
                        });
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Request inlay hints for the active file (LSP) and render them.
    fn lsp_inlay_hints(&mut self, cx: &mut Context<Self>) {
        let Some((path, line, column)) = self.cursor_location(cx) else {
            return;
        };
        let Some(server) = &self.lsp else {
            return;
        };
        let Ok(rx) = server.inlay_hints(path.clone(), line, column) else {
            return;
        };
        self.status = "正在请求内联提示…".to_string();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let response = rx
                .recv_timeout(std::time::Duration::from_secs(8))
                .ok();
            this.update(cx, |this, cx| {
                match response {
                    Some(response) => {
                        let hints = parse_inlay_hints(&response);
                        if let Some(editor) = this.active_editor() {
                            let spans: Vec<crate::editor::InlayHint> = hints
                                .into_iter()
                                .map(|hint| crate::editor::InlayHint {
                                    line: hint.line,
                                    start: hint.start,
                                    label: hint.label,
                                })
                                .collect();
                            editor.update(cx, |editor, cx| {
                                editor.set_inlay_hints(spans, cx);
                            });
                        }
                        this.status = "内联提示已更新".to_string();
                    }
                    None => this.status = "内联提示请求超时".to_string(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Request completion at the cursor (LSP); result is shown in the status bar.
    fn lsp_completion(&mut self, cx: &mut Context<Self>) {
        let Some((path, line, column)) = self.cursor_location(cx) else {
            self.status = "没有活动编辑器".to_string();
            cx.notify();
            return;
        };
        let Some(server) = &self.lsp else {
            self.status = "语言服务器未连接（检查是否安装了 clangd / rust-analyzer / zls）".to_string();
            cx.notify();
            return;
        };
        let Ok(rx) = server.completion(path.clone(), line, column) else {
            self.status = "无法请求补全（语言服务器不可用）".to_string();
            cx.notify();
            return;
        };
        self.status = "正在请求补全…".to_string();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let response = rx
                .recv_timeout(std::time::Duration::from_secs(8))
                .ok();
            this.update(cx, |this, cx| {
                match response {
                    Some(response) => {
                        let items = parse_completion_items(&response);
                        if items.is_empty() {
                            this.completion_open = false;
                            this.status = "无补全项".to_string();
                        } else {
                            let count = items.len();
                            this.completions = items;
                            this.completion_open = true;
                            this.completion_selected = 0;
                            this.status = format!("{count} 个补全项");
                        }
                    }
                    None => this.status = "补全请求超时".to_string(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Insert the selected completion item's label at the cursor.
    fn completion_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.completions.get(self.completion_selected) else {
            self.completion_open = false;
            cx.notify();
            return;
        };
        let label = item.label.clone();
        if let Some(editor) = self.active_editor() {
            editor.update(cx, |editor, cx| editor.insert_text(&label, cx));
        }
        self.completion_open = false;
        self.status = "已插入补全".to_string();
        cx.notify();
    }

    /// Move the completion selection by `delta`.
    fn completion_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        if !self.completion_open || self.completions.is_empty() {
            return;
        }
        let len = self.completions.len();
        let current = self.completion_selected as isize;
        let next = (current + delta).rem_euclid(len as isize) as usize;
        self.completion_selected = next;
        cx.notify();
    }

    /// Close the completion popup.
    fn completion_cancel(&mut self, cx: &mut Context<Self>) {
        self.completion_open = false;
        cx.notify();
    }

    /// Current cursor position as (path, 0-based line, 0-based column).
    fn cursor_location(&self, cx: &mut Context<Self>) -> Option<(PathBuf, usize, usize)> {
        let editor = self.active_editor()?;
        let index = self.active_tab?;
        let path = self.tabs.get(index)?.path.clone();
        let (line, column) = editor.read(cx).cursor_line_col();
        // cursor_line_col is 1-based; LSP uses 0-based.
        Some((path, line.saturating_sub(1), column.saturating_sub(1)))
    }

    fn cancel_condition_dialog(&mut self, cx: &mut Context<Self>) {
        if self.condition_dialog.take().is_some() {
            self.status = "已取消条件编辑".to_string();
            cx.notify();
        }
    }

    fn show_breakpoints(&mut self, cx: &mut Context<Self>) {
        self.show_output = true;
        self.output_tab = OutputTab::Breakpoints;
        cx.notify();
    }

    fn open_diagnostic(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(diagnostic) = self.diagnostics.get(index) else {
            return;
        };
        let Some(path) = diagnostic.path.clone() else {
            return;
        };
        let line = diagnostic.line.unwrap_or(1);
        self.open_file(path, cx);
        if let Some(editor) = self.active_editor() {
            editor.update(cx, |editor, cx| editor.set_cursor_line(line, cx));
        }
        cx.notify();
    }

    fn tree_entries(&self) -> Vec<TreeEntry> {
        let Some(project) = &self.project else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        collect_tree(
            &project.root,
            0,
            &self.expanded,
            &mut entries,
        );
        entries
    }

    fn render_topbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let project_name = self
            .project
            .as_ref()
            .map(|project| project.config.project.name.clone())
            .unwrap_or_else(|| "未打开项目".to_string());
        let project_path = self
            .project
            .as_ref()
            .map(|project| project.root.display().to_string())
            .unwrap_or_else(|| "打开或新建一个项目以开始".to_string());

        div()
            .h(px(54.))
            .flex_none()
            .pl(px(78.))
            .pr(px(16.))
            .flex()
            .items_center()
            .justify_between()
            .bg(theme::color(theme::BG_1).opacity(0.82))
            .border_b_1()
            .border_color(theme::alpha(theme::STROKE, 0.12))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(11.))
                    .child(widgets::brand_mark(30.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(theme::TEXT))
                                    .child(project_name),
                            )
                            .child(
                                div()
                                    .max_w(px(330.))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(10.))
                                    .text_color(rgb(theme::TEXT_FAINT))
                                    .child(project_path),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .child(
                        widgets::ghost_button("top-new", "＋ 新建")
                            .on_click(cx.listener(|this, _, window, cx| this.open_wizard(window, cx))),
                    )
                    .child(
                        widgets::ghost_button("top-open", "打开")
                            .on_click(cx.listener(|this, _, _, cx| this.open_project_dialog(cx))),
                    )
                    .child(widgets::ghost_button("top-save", "保存").on_click(
                        cx.listener(|this, _, _, cx| this.save_active(cx)),
                    ))
                    .child(widgets::primary_button("top-build", "构建").on_click(
                        cx.listener(|this, _, _, cx| this.start_build(false, cx)),
                    ))
                    .child(if self.run.running {
                        widgets::ghost_button("top-stop-run", "■ 停止").on_click(
                            cx.listener(|this, _, _, cx| this.stop_run(cx)),
                        )
                    } else {
                        widgets::ghost_button("top-run", "运行").on_click(
                            cx.listener(|this, _, _, cx| this.start_build(true, cx)),
                        )
                    })
                    .when(self.run.started, |style| {
                        style.child(widgets::ghost_button("top-restart-run", "↻ 重新运行").on_click(
                            cx.listener(|this, _, _, cx| this.restart_run(cx)),
                        ))
                    })
                    .child(widgets::ghost_button("top-debug", "调试").on_click(
                        cx.listener(|this, _, _, cx| this.start_debug(cx)),
                    ))
                    .child(widgets::ghost_button("top-flash", "烧写").on_click(
                        cx.listener(|this, _, _, cx| this.start_flash(cx)),
                    ))
                    .child(widgets::ghost_button("top-flash-actions", "擦除").on_click(
                        cx.listener(|this, _, _, cx| this.flash_action(FlashAction::Erase, cx)),
                    ))
                    .child(widgets::ghost_button("top-flash-verify", "校验").on_click(
                        cx.listener(|this, _, _, cx| this.flash_action(FlashAction::Verify, cx)),
                    ))
                    .child(widgets::ghost_button("top-flash-read", "读回").on_click(
                        cx.listener(|this, _, _, cx| this.flash_action(FlashAction::Read, cx)),
                    ))
                    .child(widgets::ghost_button("top-flash-run", "烧写并运行").on_click(
                        cx.listener(|this, _, _, cx| this.flash_and_run(cx)),
                    ))
                    .child(widgets::ghost_button("top-flash-debug", "烧写并调试").on_click(
                        cx.listener(|this, _, _, cx| this.flash_and_debug(cx)),
                    ))
                    .when(self.debug_session.is_some(), |style| {
                        style.child(widgets::ghost_button("top-stop-debug", "停止").on_click(
                            cx.listener(|this, _, _, cx| this.stop_debug(cx)),
                        ))
                    })
                    .child(widgets::ghost_button("top-breakpoints", "断点").on_click(
                        cx.listener(|this, _, _, cx| this.show_breakpoints(cx)),
                    )),
            )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entries = self.tree_entries();
        let active_path = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.path.clone());

        div()
            .w(px(238.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::color(theme::BG_1).opacity(0.56))
            .border_r_1()
            .border_color(theme::alpha(theme::STROKE, 0.1))
            .child(
                div()
                    .h(px(42.))
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme::alpha(theme::STROKE, 0.08))
                    .child(widgets::section_label("项目"))
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(rgb(theme::TEXT_FAINT))
                            .child(format!("{} 项", entries.len())),
                    ),
            )
            .child(
                div()
                    .id("project-tree")
                    .flex_1()
                    .overflow_y_scroll()
                    .py(px(6.))
                    .px(px(6.))
                    .flex()
                    .flex_col()
                    .children(entries.into_iter().enumerate().map(|(entry_index, entry)| {
                        let is_active = active_path.as_ref() == Some(&entry.path);
                        let path = entry.path.clone();
                        let is_dir = entry.is_dir;
                        let icon = if is_dir {
                            if entry.expanded {
                                "▾"
                            } else {
                                "▸"
                            }
                        } else {
                            "·"
                        };
                        div()
                            .id(("tree-item", entry_index))
                            .h(px(26.))
                            .pl(px(6. + entry.depth as f32 * 13.))
                            .pr(px(6.))
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .rounded(px(6.))
                            .bg(if is_active {
                                theme::alpha(theme::ACCENT, 0.1)
                            } else {
                                theme::alpha(0xffffff, 0.0)
                            })
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.07)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if is_dir {
                                    if !this.expanded.remove(&path) {
                                        this.expanded.insert(path.clone());
                                    }
                                } else {
                                    this.open_file(path.clone(), cx);
                                }
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .w(px(13.))
                                    .text_size(px(10.))
                                    .text_color(rgb(if is_dir {
                                        theme::ACCENT
                                    } else {
                                        theme::TEXT_FAINT
                                    }))
                                    .child(icon),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(11.5))
                                    .text_color(if is_active {
                                        rgb(theme::TEXT)
                                    } else if entry.is_dir {
                                        rgb(theme::TEXT_DIM)
                                    } else {
                                        rgb(theme::TEXT_DIM)
                                    })
                                    .child(entry.name),
                            )
                    })),
            )
            .child(
                div()
                    .h(px(38.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(theme::alpha(theme::STROKE, 0.08))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(rgb(theme::TEXT_FAINT))
                            .child("project.toml 驱动工具链"),
                    ),
            )
    }

    fn render_tabbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs: Vec<(usize, String, bool, bool)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let title = tab
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "untitled".to_string());
                let dirty = tab.editor.read(cx).is_dirty();
                (index, title, dirty, self.active_tab == Some(index))
            })
            .collect();

        div()
            .h(px(39.))
            .flex_none()
            .flex()
            .items_center()
            .overflow_hidden()
            .bg(theme::color(theme::BG_1).opacity(0.72))
            .border_b_1()
            .border_color(theme::alpha(theme::STROKE, 0.1))
            .children(tabs.into_iter().map(|(index, title, dirty, active)| {
                div()
                    .id(("tab", index))
                    .h_full()
                    .px(px(13.))
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .border_r_1()
                    .border_color(theme::alpha(theme::STROKE, 0.08))
                    .bg(if active {
                        theme::color(theme::BG_0).opacity(0.85)
                    } else {
                        theme::alpha(0xffffff, 0.0)
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.activate_tab(index, cx)))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(if active {
                                rgb(theme::TEXT)
                            } else {
                                rgb(theme::TEXT_DIM)
                            })
                            .child(title),
                    )
                    .when(dirty, |style| {
                        style.child(
                            div()
                                .size(px(5.))
                                .rounded_full()
                                .bg(theme::color(theme::ACCENT)),
                        )
                    })
                    .child(
                        div()
                            .id(("tab-close", index))
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .text_size(px(10.))
                            .text_color(rgb(theme::TEXT_FAINT))
                            .cursor_pointer()
                            .hover(|style| {
                                style
                                    .text_color(rgb(theme::TEXT))
                                    .bg(theme::alpha(theme::STROKE, 0.12))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.close_tab(index, cx);
                            }))
                            .child("×"),
                    )
            }))
    }

    fn render_editor_area(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active_editor();
        let breadcrumb = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .map(|tab| self.display_path(&tab.path))
            .unwrap_or_default();

        div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .when(active.is_some(), |style| {
                style.child(self.render_tabbar(cx)).child(
                    div()
                        .h(px(26.))
                        .flex_none()
                        .px(px(14.))
                        .flex()
                        .items_center()
                        .bg(theme::color(theme::BG_0).opacity(0.55))
                        .border_b_1()
                        .border_color(theme::alpha(theme::STROKE, 0.06))
                        .child(
                            div()
                                .text_size(px(9.5))
                                .text_color(rgb(theme::TEXT_FAINT))
                                .child(breadcrumb),
                        ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .relative()
                    .overflow_hidden()
                    .child(match active.as_ref() {
                        Some(editor) => {
                            div().flex_1().h_full().overflow_hidden().child(editor.clone()).into_any_element()
                        }
                        None => self.render_empty_editor(cx).into_any_element(),
                    })
                    .when(self.completion_open && !self.completions.is_empty(), |style| {
                        style.child(self.render_completion_popup(cx))
                    })
                    .when(self.signature_help.is_some(), |style| {
                        style.child(self.render_signature_help(cx))
                    })
                    .when(
                        active
                            .as_ref()
                            .map(|editor| editor.read(cx).find_open())
                            .unwrap_or(false),
                        |style| style.child(self.render_find_bar(cx)),
                    ),
            )
    }

    /// Floating signature help panel anchored at the bottom-left of the editor.
    fn render_signature_help(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(help) = &self.signature_help else {
            return div().into_any_element();
        };
        let label = help.label.clone();
        let active = help.active_parameter;
        div()
            .id("signature-help")
            .absolute()
            .left(px(12.))
            .bottom(px(12.))
            .max_w(px(560.))
            .px(px(12.))
            .py(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme::alpha(theme::STROKE, 0.18))
            .bg(theme::color(theme::BG_1).opacity(0.98))
            .shadow_lg()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .flex_none()
                    .text_size(px(10.))
                    .text_color(rgb(theme::ACCENT))
                    .child("ƒ"),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .font_family(theme::MONO_FONT)
                    .text_color(rgb(theme::TEXT))
                    .child(label),
            )
            .when(active.is_some(), |style| {
                let _ = cx;
                style.child(
                    div()
                        .flex_none()
                        .text_size(px(9.5))
                        .text_color(rgb(theme::TEXT_FAINT))
                        .child(format!("参数 {}", active.unwrap_or(0) + 1)),
                )
            })
            .into_any_element()
    }

    /// Floating find/replace bar anchored at the top-right of the editor.
    fn render_find_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(editor) = self.active_editor() else {
            return div().into_any_element();
        };
        let match_count = editor.read(cx).find_match_count();
        let current = editor.read(cx).find_current();
        let query_field = editor.read(cx).find_query_field_entity();
        let replace_field = editor.read(cx).find_replace_field_entity();
        let count_text = if match_count == 0 {
            "无结果".to_string()
        } else {
            format!("{}/{}", current + 1, match_count)
        };

        let mut bar = div()
            .id("find-bar")
            .absolute()
            .top(px(8.))
            .right(px(12.))
            .w(px(420.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(10.))
            .rounded(px(10.))
            .border_1()
            .border_color(theme::alpha(theme::STROKE, 0.18))
            .bg(theme::color(theme::BG_1).opacity(0.98))
            .shadow_lg();

        if let Some(field) = query_field {
            bar = bar.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().flex_1().child(field))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(10.))
                            .text_color(rgb(theme::TEXT_FAINT))
                            .child(count_text),
                    ),
            );
        }
        if let Some(field) = replace_field {
            bar = bar.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().flex_1().child(field)),
            );
        }

        bar = bar.child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .id("find-next")
                        .flex_none()
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .text_size(px(10.5))
                        .cursor_pointer()
                        .bg(theme::alpha(theme::ACCENT, 0.14))
                        .text_color(rgb(theme::ACCENT))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(editor) = this.active_editor() {
                                editor.update(cx, |editor, cx| editor.find_next(false, cx));
                            }
                        }))
                        .child("下一个"),
                )
                .child(
                    div()
                        .id("find-prev")
                        .flex_none()
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .text_size(px(10.5))
                        .cursor_pointer()
                        .bg(theme::alpha(theme::ACCENT, 0.14))
                        .text_color(rgb(theme::ACCENT))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(editor) = this.active_editor() {
                                editor.update(cx, |editor, cx| editor.find_next(true, cx));
                            }
                        }))
                        .child("上一个"),
                )
                .child(
                    div()
                        .id("find-replace")
                        .flex_none()
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .text_size(px(10.5))
                        .cursor_pointer()
                        .bg(theme::alpha(theme::ACCENT, 0.14))
                        .text_color(rgb(theme::ACCENT))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(editor) = this.active_editor() {
                                editor.update(cx, |editor, cx| editor.find_replace_current(cx));
                            }
                        }))
                        .child("替换"),
                )
                .child(
                    div()
                        .id("find-replace-all")
                        .flex_none()
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .text_size(px(10.5))
                        .cursor_pointer()
                        .bg(theme::alpha(theme::ACCENT, 0.14))
                        .text_color(rgb(theme::ACCENT))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(editor) = this.active_editor() {
                                editor.update(cx, |editor, cx| editor.find_replace_all(cx));
                            }
                        }))
                        .child("全部替换"),
                )
                .child(
                    div()
                        .id("find-close")
                        .flex_none()
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .text_size(px(10.5))
                        .cursor_pointer()
                        .text_color(rgb(theme::TEXT_FAINT))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_find_in_file(cx);
                        }))
                        .child("✕"),
                ),
        );

        bar.into_any_element()
    }

    /// Floating completion popup anchored at the bottom-left of the editor.
    fn render_completion_popup(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.completion_selected.min(self.completions.len().saturating_sub(1));
        div()
            .id("completion-popup")
            .absolute()
            .left(px(12.))
            .bottom(px(12.))
            .w(px(360.))
            .max_h(px(260.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::alpha(theme::STROKE, 0.18))
            .bg(theme::color(theme::BG_1).opacity(0.98))
            .shadow_lg()
            .p(px(4.))
            .children(
                self.completions
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let is_selected = index == selected;
                        div()
                            .id(("completion-item", index))
                            .px(px(8.))
                            .py(px(5.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(px(5.))
                            .bg(if is_selected {
                                theme::alpha(theme::ACCENT, 0.16)
                            } else {
                                gpui::transparent_black()
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.completion_selected = index;
                                this.completion_confirm(cx);
                            }))
                            .child(
                                div()
                                    .w(px(70.))
                                    .flex_none()
                                    .text_size(px(9.))
                                    .text_color(rgb(theme::TEXT_FAINT))
                                    .child(item.kind.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .font_family(theme::MONO_FONT)
                                    .text_size(px(11.))
                                    .text_color(if is_selected {
                                        rgb(theme::TEXT)
                                    } else {
                                        rgb(theme::TEXT_DIM)
                                    })
                                    .child(item.label.clone()),
                            )
                            .when(!item.detail.is_empty(), |style| {
                                style.child(
                                    div()
                                        .flex_none()
                                        .max_w(px(160.))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_size(px(9.5))
                                        .text_color(rgb(theme::TEXT_FAINT))
                                        .child(item.detail.clone()),
                                )
                            })
                    }),
            )
    }

    fn render_empty_editor(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(14.))
            .child(
                div()
                    .size(px(84.))
                    .rounded_full()
                    .border_1()
                    .border_dashed()
                    .border_color(theme::alpha(theme::STROKE, 0.22))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(28.))
                    .text_color(theme::alpha(theme::ACCENT, 0.6))
                    .child("⌘"),
            )
            .child(
                div()
                    .text_size(px(14.))
                    .font_family(theme::SERIF_FONT)
                    .text_color(rgb(theme::TEXT_DIM))
                    .child("从左侧选择一个文件"),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child("支持 C · C++ · Rust · Zig · ZR 与普通文本"),
            )
    }

    fn render_output(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let lsp_count: usize = self.lsp_diagnostics.values().map(Vec::len).sum();
        let problem_count = self.diagnostics.len() + lsp_count;
        let shown_output = if self.run.show_run_output {
            &self.run.output
        } else {
            &self.output
        };
        let output_lines: Vec<(usize, String)> = shown_output
            .lines()
            .enumerate()
            .take(1200)
            .map(|(index, line)| (index, line.to_string()))
            .collect();

        div()
            .h(px(212.))
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme::color(theme::BG_1).opacity(0.9))
            .border_t_1()
            .border_color(theme::alpha(theme::STROKE, 0.16))
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
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                output_tab_button(
                                    "out-tab-output",
                                    "构建输出",
                                    self.output_tab == OutputTab::Output,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.output_tab = OutputTab::Output;
                                    cx.notify();
                                })),
                            )
                            .child(
                                output_tab_button(
                                    "out-tab-problems",
                                    &format!("问题 {problem_count}"),
                                    self.output_tab == OutputTab::Problems,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.output_tab = OutputTab::Problems;
                                    cx.notify();
                                })),
                            )
                            .child(
                                output_tab_button(
                                    "out-tab-breakpoints",
                                    &format!("断点 {}", self.breakpoints.len()),
                                    self.output_tab == OutputTab::Breakpoints,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.output_tab = OutputTab::Breakpoints;
                                    cx.notify();
                                })),
                            )
                            .child(
                                output_tab_button(
                                    "out-tab-debug",
                                    "调试",
                                    self.output_tab == OutputTab::Debug,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.output_tab = OutputTab::Debug;
                                    cx.notify();
                                })),
                            )
                            .child(
                                output_tab_button(
                                    "out-tab-git",
                                    "Git",
                                    self.output_tab == OutputTab::Git,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_git(cx);
                                })),
                            )
                            .child(
                                output_tab_button(
                                    "out-tab-terminal",
                                    "终端",
                                    self.output_tab == OutputTab::Terminal,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_terminal(cx);
                                })),
                            )
                            .child(
                                output_tab_button(
                                    "out-tab-serial",
                                    "串口",
                                    self.output_tab == OutputTab::Serial,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_serial_monitor(cx);
                                })),
                            )
                            .child(
                                output_tab_button(
                                    "out-tab-agent",
                                    "Agent",
                                    self.output_tab == OutputTab::Agent,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_agent_panel(cx);
                                })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .max_w(px(460.))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(9.5))
                                    .text_color(rgb(theme::TEXT_FAINT))
                                    .child(self.build_command.clone()),
                            )
                            .child(
                                widgets::icon_button("out-close", "×", 22.)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_output = false;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(match self.output_tab {
                OutputTab::Output => div()
                    .id("build-output-scroll")
                    .flex_1()
                    .overflow_scroll()
                    .p(px(10.))
                    .flex()
                    .flex_col()
                    .children(output_lines.into_iter().map(|(index, line)| {
                        let lower = line.to_ascii_lowercase();
                        let color = if lower.contains("error") || lower.contains("失败") {
                            theme::color(theme::DANGER)
                        } else if lower.contains("warning") || lower.contains("警告") {
                            theme::color(theme::ACCENT)
                        } else if lower.contains("成功") {
                            theme::color(theme::JADE)
                        } else {
                            theme::color(theme::TEXT_DIM)
                        };
                        div()
                            .id(("out-line", index))
                            .h(px(16.))
                            .whitespace_nowrap()
                            .font_family(theme::MONO_FONT)
                            .text_size(px(10.5))
                            .text_color(color)
                            .child(if line.is_empty() {
                                " ".to_string()
                            } else {
                                line
                            })
                    }))
                    .into_any_element(),
                OutputTab::Problems => {
                let mut lsp_rows: Vec<gpui::AnyElement> = Vec::new();
                for (path, diagnostics) in &self.lsp_diagnostics {
                    for diagnostic in diagnostics {
                        let (tone, glyph) = match diagnostic.severity {
                            ns_lsp::LspSeverity::Error => (theme::color(theme::DANGER), "✕"),
                            ns_lsp::LspSeverity::Warning => (theme::color(theme::ACCENT), "!"),
                            ns_lsp::LspSeverity::Information
                            | ns_lsp::LspSeverity::Hint => (theme::color(theme::JADE), "i"),
                        };
                        let click_path = path.clone();
                        let line = diagnostic.line;
                        let location_text = format!("{}:{}:{}", path.display(), diagnostic.line, diagnostic.column);
                        let message = diagnostic.message.clone();
                        let row_id = gpui::SharedString::from(format!(
                            "lsp-problem-{}-{}",
                            path.display(),
                            line
                        ));
                        lsp_rows.push(
                            div()
                                .id(row_id)
                                .px(px(10.))
                                .py(px(7.))
                                .flex()
                                .items_center()
                                .gap(px(9.))
                                .rounded(px(6.))
                                .bg(theme::alpha(0xffffff, 0.015))
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.07)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_file(click_path.clone(), cx);
                                    if let Some(editor) = this.active_editor() {
                                        editor.update(cx, |editor, cx| {
                                            editor.set_cursor_line(line, cx)
                                        });
                                    }
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .w(px(15.))
                                        .text_size(px(11.))
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(tone)
                                        .child(glyph),
                                )
                                .child(
                                    div()
                                        .w(px(250.))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .font_family(theme::MONO_FONT)
                                        .text_size(px(10.5))
                                        .text_color(rgb(theme::TEXT_DIM))
                                        .child(location_text),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_size(px(11.))
                                        .text_color(rgb(theme::TEXT))
                                        .child(message),
                                )
                                .child(
                                    div()
                                        .text_size(px(9.5))
                                        .text_color(tone)
                                        .child("LSP"),
                                )
                                .into_any_element(),
                        );
                    }
                }
                div()
                    .id("build-problems-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .p(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .when(self.diagnostics.is_empty() && self.lsp_diagnostics.is_empty(), |style| {
                        style.child(
                            div()
                                .p(px(12.))
                                .text_size(px(11.))
                                .text_color(rgb(theme::TEXT_FAINT))
                                .child("没有发现错误或警告。"),
                        )
                    })
                    .children(self.diagnostics.iter().enumerate().map(|(index, diagnostic)| {
                        let (tone, glyph) = match diagnostic.severity {
                            Severity::Error => (theme::color(theme::DANGER), "✕"),
                            Severity::Warning => (theme::color(theme::ACCENT), "!"),
                            Severity::Info => (theme::color(theme::JADE), "i"),
                        };
                        div()
                            .id(("problem", index))
                            .px(px(10.))
                            .py(px(7.))
                            .flex()
                            .items_center()
                            .gap(px(9.))
                            .rounded(px(6.))
                            .bg(theme::alpha(0xffffff, 0.015))
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.07)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_diagnostic(index, cx)
                            }))
                            .child(
                                div()
                                    .w(px(15.))
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(tone)
                                    .child(glyph),
                            )
                            .child(
                                div()
                                    .w(px(250.))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .font_family(theme::MONO_FONT)
                                    .text_size(px(10.5))
                                    .text_color(rgb(theme::TEXT_DIM))
                                    .child(diagnostic.location_label()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(11.))
                                    .text_color(rgb(theme::TEXT))
                                    .child(diagnostic.message.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(9.5))
                                    .text_color(tone)
                                    .child(diagnostic.severity.label()),
                            )
                    }))
                    .children(lsp_rows)
                    .into_any_element()
                }
                OutputTab::Breakpoints => {
                    let total = self.breakpoints.len();
                    let enabled = self.breakpoints.enabled_count();
                    let mut rows: Vec<gpui::AnyElement> = Vec::new();
                    let mut current_file: Option<String> = None;
                    for (index, breakpoint) in self.breakpoints.all().iter().enumerate() {
                        if current_file.as_deref() != Some(breakpoint.path.as_str()) {
                            current_file = Some(breakpoint.path.clone());
                            rows.push(
                                div()
                                    .id(("bp-file", index))
                                    .pt(px(6.))
                                    .pb(px(2.))
                                    .px(px(6.))
                                    .font_family(theme::MONO_FONT)
                                    .text_size(px(9.5))
                                    .text_color(rgb(theme::TEXT_FAINT))
                                    .child(breakpoint.path.clone())
                                    .into_any_element(),
                            );
                        }
                        let toggle_path = breakpoint.path.clone();
                        let open_path = breakpoint.path.clone();
                        let remove_path = breakpoint.path.clone();
                        let line = breakpoint.line;
                        let is_enabled = breakpoint.enabled;
                        let kind_text = if breakpoint.kind == BreakpointKind::Conditional
                            && !breakpoint.condition.is_empty()
                        {
                            format!("条件 · {}", breakpoint.condition)
                        } else if breakpoint.kind == BreakpointKind::Log
                            && !breakpoint.log_message.is_empty()
                        {
                            format!("日志 · {}", breakpoint.log_message)
                        } else {
                            breakpoint.kind.label().to_string()
                        };
                        let summary = if !breakpoint.hit_count.is_empty() {
                            format!("{kind_text} · 第{}次", breakpoint.hit_count)
                        } else {
                            kind_text
                        };
                        let state_label = breakpoint.state_label();
                        let state_color = match state_label {
                            "已验证" => theme::color(theme::JADE),
                            "命中" => theme::color(theme::CINNABAR),
                            "未绑定" => theme::color(theme::CINNABAR),
                            "未同步" | "失效" => theme::color(theme::ACCENT),
                            _ => theme::color(theme::TEXT_FAINT),
                        };
                        rows.push(
                            div()
                                .id(("breakpoint", index))
                                .px(px(10.))
                                .py(px(6.))
                                .flex()
                                .items_center()
                                .gap(px(9.))
                                .rounded(px(6.))
                                .bg(theme::alpha(0xffffff, 0.015))
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.07)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_breakpoint(open_path.clone(), line, cx);
                                }))
                                .child(
                                    div()
                                        .id(("bp-toggle", index))
                                        .size(px(15.))
                                        .rounded(px(4.))
                                        .border_1()
                                        .border_color(if is_enabled {
                                            theme::alpha(theme::CINNABAR, 0.9)
                                        } else {
                                            theme::alpha(theme::STROKE, 0.35)
                                        })
                                        .bg(if is_enabled {
                                            theme::alpha(theme::CINNABAR, 0.9)
                                        } else {
                                            theme::alpha(theme::BG_1, 0.2)
                                        })
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(px(10.))
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(if is_enabled {
                                            rgb(0xffffff)
                                        } else {
                                            rgb(theme::TEXT_FAINT)
                                        })
                                        .child(if is_enabled { "✓" } else { "" })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.toggle_breakpoint_enabled(&toggle_path, line, cx);
                                        })),
                                )
                                .child(
                                    div()
                                        .w(px(60.))
                                        .font_family(theme::MONO_FONT)
                                        .text_size(px(10.5))
                                        .text_color(if is_enabled {
                                            rgb(theme::TEXT_DIM)
                                        } else {
                                            rgb(theme::TEXT_FAINT)
                                        })
                                        .child(format!("行 {}", line)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_size(px(10.5))
                                        .text_color(rgb(theme::TEXT_FAINT))
                                        .child(summary),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .px(px(7.))
                                        .py(px(2.))
                                        .rounded_full()
                                        .border_1()
                                        .border_color(state_color.opacity(0.45))
                                        .bg(state_color.opacity(0.12))
                                        .text_size(px(8.5))
                                        .text_color(state_color)
                                        .child(state_label),
                                )
                                .when(!breakpoint.reason.is_empty(), |style| {
                                    style.child(
                                        div()
                                            .max_w(px(160.))
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .text_size(px(9.))
                                            .text_color(theme::color(theme::CINNABAR))
                                            .child(breakpoint.reason.clone()),
                                    )
                                })
                                .child(
                                    widgets::icon_button(("bp-remove", index), "×", 20.)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.remove_breakpoint(&remove_path, line, cx);
                                        })),
                                )
                                .into_any_element(),
                        );
                    }
                    div()
                        .id("breakpoints-scroll")
                        .flex_1()
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
                                        .text_color(rgb(theme::TEXT_FAINT))
                                        .child(format!("{total} 个断点 · {enabled} 个已启用")),
                                )
                                .when(total > 0, |style| {
                                    style.child(widgets::ghost_button("bp-clear-all", "清除全部")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.clear_all_breakpoints(cx);
                                        })))
                                }),
                        )
                        .child(
                            div()
                                .id("breakpoints-list-scroll")
                                .flex_1()
                                .overflow_y_scroll()
                                .p(px(8.))
                                .flex()
                                .flex_col()
                                .gap(px(4.))
                                .when(self.breakpoints.is_empty(), |style| {
                                    style.child(
                                        div()
                                            .p(px(12.))
                                            .text_size(px(11.))
                                            .text_color(rgb(theme::TEXT_FAINT))
                                            .child("还没有断点。在编辑器左侧行号栏点击，或按 F9 添加；Cmd/Ctrl+Shift+F9 清除全部。"),
                                    )
                                })
                                .children(rows),
                        )
                        .child(self.render_watchpoint_panel(cx))
                        .into_any_element()
                }
                OutputTab::Debug => self.render_debug(cx).into_any_element(),
                OutputTab::Git => self.render_git(cx).into_any_element(),
                OutputTab::Terminal => self
                    .terminal
                    .clone()
                    .map(|terminal| {
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .child(terminal)
                            .into_any_element()
                    })
                    .unwrap_or_else(|| {
                        div()
                            .p(px(12.))
                            .text_size(px(11.))
                            .text_color(rgb(theme::TEXT_FAINT))
                            .child("终端未启动")
                            .into_any_element()
                    }),
                OutputTab::Serial => self
                    .serial
                    .clone()
                    .map(|serial| div().flex_1().overflow_hidden().child(serial).into_any_element())
                    .unwrap_or_else(|| {
                        div()
                            .p(px(12.))
                            .text_size(px(11.))
                            .text_color(rgb(theme::TEXT_FAINT))
                            .child("串口助手未启动")
                            .into_any_element()
                    }),
                OutputTab::Agent => self.render_agent_panel(cx).into_any_element(),
            })
    }
    /// Git panel: changed files, diff of the selected file, and a commit box.
    fn render_git(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let in_repo = self.git.status.in_repo;
        if !in_repo {
            return div()
                .id("git-empty")
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgb(theme::TEXT_DIM))
                                .child("当前目录不是 Git 仓库"),
                        )
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(rgb(theme::TEXT_FAINT))
                                .child("在项目根目录运行 git init 后刷新。"),
                        ),
                )
                .into_any_element();
        }

        let branch = self.git.status.branch.clone();
        let changes = self.git.status.changes;
        let files = self.git.status.files.clone();
        let selected = self.git.selected.clone();
        let diff_text = self.git.diff.clone();
        let error = self.git.error.clone();
        let feedback = self.git.feedback.clone();
        let mut file_rows: Vec<gpui::AnyElement> = Vec::new();
        for (index, file) in files.iter().enumerate() {
            let path = file.path.clone();
            let is_selected = selected.as_deref() == Some(path.as_str());
            let label = file.label();
            let color = if file.is_untracked() {
                theme::color(theme::ACCENT)
            } else if file.is_deleted() {
                theme::color(theme::CINNABAR)
            } else {
                theme::color(theme::JADE)
            };
            let staged = file.staged != ' ';
            let select_path = path.clone();
            let stage_path = path.clone();
            let display_path = path.clone();
            file_rows.push(
                div()
                    .id(("git-file", index))
                    .px(px(8.))
                    .py(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .rounded(px(5.))
                    .bg(if is_selected {
                        theme::alpha(theme::ACCENT, 0.09)
                    } else {
                        theme::alpha(0xffffff, 0.0)
                    })
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.08)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.git_select(select_path.clone(), cx);
                    }))
                    .child(
                        div()
                            .w(px(20.))
                            .text_size(px(10.))
                            .font_family(theme::MONO_FONT)
                            .text_color(color)
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(11.))
                            .text_color(if is_selected {
                                rgb(theme::TEXT)
                            } else {
                                rgb(theme::TEXT_DIM)
                            })
                            .child(display_path.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(3.))
                            .child(
                                div()
                                    .id(("git-stage", index))
                                    .px(px(6.))
                                    .py(px(2.))
                                    .rounded(px(4.))
                                    .bg(theme::alpha(theme::STROKE, 0.10))
                                    .text_size(px(9.))
                                    .text_color(rgb(theme::TEXT_DIM))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::alpha(theme::ACCENT, 0.16)))
                                    .child(if staged { "取消暂存" } else { "暂存" })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if staged {
                                            this.git_unstage(stage_path.clone(), cx);
                                        } else {
                                            this.git_stage(stage_path.clone(), cx);
                                        }
                                    })),
                            ),
                    )
                    .into_any_element(),
            );
        }

        // Commit box.
        let commit_field = self
            .git
            .message
            .clone()
            .unwrap_or_else(|| cx.new(|cx| TextField::new("提交信息…", cx)));
        let commit_field_for_render = commit_field.clone();
        let commit_box = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .flex_1()
                    .child(commit_field_for_render.clone()),
            )
            .child(
                widgets::primary_button("git-commit", "提交")
                    .on_click(cx.listener(|this, _, _, cx| this.git_commit(cx))),
            );

        let status_line = if let Some(error) = &error {
            div()
                .text_size(px(10.))
                .text_color(rgb(theme::DANGER))
                .child(error.clone())
                .into_any_element()
        } else if let Some(feedback) = &feedback {
            div()
                .text_size(px(10.))
                .text_color(rgb(theme::JADE))
                .child(feedback.clone())
                .into_any_element()
        } else {
            div()
                .text_size(px(10.))
                .text_color(rgb(theme::TEXT_FAINT))
                .child(format!("分支 {branch} · {changes} 个变更"))
                .into_any_element()
        };
        let history = self.git.history.clone();
        let branches = self.git.branches.clone();
        let show_history = self.git.show_history;
        let history_toggle = div()
            .id("git-history-toggle")
            .px(px(10.))
            .py(px(4.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.06)))
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(if show_history { "▾" } else { "▸" }),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme::TEXT_DIM))
                    .child(format!(
                        "历史 · 分支（{} 提交 / {} 分支）",
                        history.len(),
                        branches.len()
                    )),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.git.show_history = !this.git.show_history;
                cx.notify();
            }));

        let history_section = show_history
            .then(|| {
                div()
                    .id("git-history")
                    .h(px(150.))
                    .flex_none()
                    .overflow_y_scroll()
                    .border_t_1()
                    .border_color(theme::alpha(theme::STROKE, 0.08))
                    .p(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .children(history.into_iter().map(|commit| {
                        div()
                            .px(px(8.))
                            .py(px(3.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(10.5))
                                    .text_color(rgb(theme::TEXT))
                                    .child(commit.subject.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .font_family(theme::MONO_FONT)
                                    .text_color(rgb(theme::TEXT_FAINT))
                                    .child(commit.hash.clone()),
                            )
                            .into_any_element()
                    }))
                    .into_any_element()
            })
            .unwrap_or_else(|| div().into_any_element());

        div()
            .id("git-panel")
            .flex_1()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(10.))
                    .py(px(7.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme::alpha(theme::STROKE, 0.08))
                    .child(status_line)
                    .child(commit_box),
            )
            .child(history_toggle)
            .child(history_section)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .min_h(px(0.))
                    .child(
                        div()
                            .id("git-files")
                            .w(px(300.))
                            .flex_none()
                            .overflow_y_scroll()
                            .p(px(6.))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .when(files.is_empty(), |style| {
                                style.child(
                                    div()
                                        .p(px(10.))
                                        .text_size(px(11.))
                                        .text_color(rgb(theme::TEXT_FAINT))
                                        .child("工作区干净，没有变更。"),
                                )
                            })
                            .children(file_rows),
                    )
                    .child(
                        div()
                            .id("git-diff")
                            .flex_1()
                            .overflow_scroll()
                            .p(px(10.))
                            .font_family(theme::MONO_FONT)
                            .text_size(px(10.))
                            .text_color(rgb(theme::TEXT_DIM))
                            .child(if diff_text.is_empty() {
                                "选择一个文件查看差异。".to_string()
                            } else {
                                diff_text
                            }),
                    ),
            )
            .into_any_element()
    }

    /// The agent panel (任务/子代理管理).
    fn render_agent_panel(&self, _cx: &mut Context<Self>) -> gpui::AnyElement {
        self.agent
            .clone()
            .map(|agent| div().flex_1().overflow_hidden().child(agent).into_any_element())
            .unwrap_or_else(|| {
                div()
                    .p(px(12.))
                    .text_size(px(11.))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child("Agent 面板未初始化")
                    .into_any_element()
            })
    }


    /// The watchpoint (data breakpoint) section of the breakpoints panel.
    fn render_watchpoint_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let watchpoints = self.breakpoints.watchpoints();
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for (index, watchpoint) in watchpoints.iter().enumerate() {
            let expression = watchpoint.expression.clone();
            let is_enabled = watchpoint.enabled;
            let access_label = watchpoint.access.label().to_string();
            let state_label = watchpoint.state_label();
            let state_color = match state_label {
                "已验证" => theme::color(theme::JADE),
                "命中" => theme::color(theme::CINNABAR),
                "未绑定" => theme::color(theme::CINNABAR),
                "未同步" => theme::color(theme::ACCENT),
                _ => theme::color(theme::TEXT_FAINT),
            };
            rows.push(
                div()
                    .id(("watchpoint", index))
                    .px(px(10.))
                    .py(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .rounded(px(6.))
                    .bg(theme::alpha(0xffffff, 0.015))
                    .child(
                        div()
                            .id(("wp-toggle", index))
                            .size(px(15.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(if is_enabled {
                                theme::alpha(theme::CINNABAR, 0.9)
                            } else {
                                theme::alpha(theme::STROKE, 0.35)
                            })
                            .bg(if is_enabled {
                                theme::alpha(theme::CINNABAR, 0.9)
                            } else {
                                theme::alpha(theme::BG_1, 0.2)
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(10.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(if is_enabled {
                                rgb(0xffffff)
                            } else {
                                rgb(theme::TEXT_FAINT)
                            })
                            .child(if is_enabled { "✓" } else { "" })
                            .on_click({
                                let expression = expression.clone();
                                cx.listener(move |this, _, _, cx| {
                                    this.toggle_watchpoint_enabled(&expression, cx);
                                })
                            }),
                    )
                    .child(
                        div()
                            .id(("wp-access", index))
                            .w(px(48.))
                            .flex_none()
                            .rounded_full()
                            .border_1()
                            .border_color(theme::alpha(theme::ACCENT, 0.45))
                            .bg(theme::alpha(theme::ACCENT, 0.1))
                            .text_size(px(8.5))
                            .text_color(rgb(theme::ACCENT_3))
                            .flex()
                            .items_center()
                            .justify_center()
                            .py(px(2.))
                            .cursor_pointer()
                            .on_click({
                                let expression = expression.clone();
                                cx.listener(move |this, _, _, cx| {
                                    if let Some(watchpoint) =
                                        this.breakpoints.find_watchpoint(&expression)
                                    {
                                        let access = watchpoint.access.next();
                                        this.breakpoints.add_watchpoint(&expression, access);
                                        this.persist_breakpoints();
                                        this.sync_watchpoints_if_running();
                                    }
                                    cx.notify();
                                })
                            })
                            .child(access_label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_family(theme::MONO_FONT)
                            .text_size(px(10.5))
                            .text_color(if is_enabled {
                                rgb(theme::TEXT)
                            } else {
                                rgb(theme::TEXT_FAINT)
                            })
                            .child(watchpoint.expression.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px(px(7.))
                            .py(px(2.))
                            .rounded_full()
                            .border_1()
                            .border_color(state_color.opacity(0.45))
                            .bg(state_color.opacity(0.12))
                            .text_size(px(8.5))
                            .text_color(state_color)
                            .child(state_label),
                    )
                    .when(!watchpoint.reason.is_empty(), |style| {
                        style.child(
                            div()
                                .max_w(px(160.))
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(px(9.))
                                .text_color(theme::color(theme::CINNABAR))
                                .child(watchpoint.reason.clone()),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .gap(px(2.))
                            .child(
                                widgets::icon_button(("wp-edit", index), "✎", 20.)
                                    .on_click({
                                        let expression = expression.clone();
                                        cx.listener(move |this, _, _, cx| {
                                            this.open_watchpoint_dialog(&expression, "", cx);
                                        })
                                    }),
                            )
                            .child(
                                widgets::icon_button(("wp-remove", index), "×", 20.)
                                    .on_click({
                                        let expression = expression.clone();
                                        cx.listener(move |this, _, _, cx| {
                                            this.remove_watchpoint(&expression, cx);
                                        })
                                    }),
                            ),
                    )
                    .into_any_element(),
            );
        }
        let count = watchpoints.len();
        let enabled = watchpoints.iter().filter(|watchpoint| watchpoint.enabled).count();
        div()
            .id("watchpoint-panel")
            .h(px(140.))
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(theme::alpha(theme::STROKE, 0.08))
            .child(
                div()
                    .h(px(30.))
                    .flex_none()
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(theme::TEXT_FAINT))
                            .child(format!("观察点（数据断点） · {count} 个 · {enabled} 个已启用")),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .child(
                                widgets::ghost_button("wp-add", "添加观察点…").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.open_watchpoint_dialog("", "", cx);
                                    }),
                                ),
                            )
                            .when(count > 0, |style| {
                                style.child(
                                    widgets::ghost_button("wp-clear-all", "清除全部").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.breakpoints.clear_watchpoints();
                                            this.persist_breakpoints();
                                            this.sync_watchpoints_if_running();
                                            this.status = "已清除全部观察点".to_string();
                                            cx.notify();
                                        }),
                                    ),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .id("watchpoint-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .p(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .when(count == 0, |style| {
                        style.child(
                            div()
                                .p(px(12.))
                                .text_size(px(11.))
                                .text_color(rgb(theme::TEXT_FAINT))
                                .child("没有观察点。右键源代码选择「添加观察点（数据断点）…」，或在变量面板点击 👁。"),
                        )
                    })
                    .children(rows),
            )
            .into_any_element()
    }

    fn render_debug(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (status_label, status_color) = (
            self.debug.status.label(),
            match &self.debug.status {
                DebugStatus::Idle => theme::color(theme::TEXT_FAINT),
                DebugStatus::Running => theme::color(theme::ACCENT),
                DebugStatus::Stopped { .. } => theme::color(theme::JADE),
                DebugStatus::Exited => theme::color(theme::TEXT_FAINT),
            },
        );
        let resume_label = if matches!(self.debug.status, DebugStatus::Stopped { .. }) {
            "继续"
        } else {
            "暂停"
        };
        let active_frame = self.debug.active_frame;
        let session_active = self.debug_session.is_some();
        let console_empty = self.debug.console.is_empty();

        div()
            .id("debug-panel")
            .flex_1()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(30.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(10.))
                    .border_b_1()
                    .border_color(theme::alpha(theme::STROKE, 0.08))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                widgets::ghost_button("dbg-resume", resume_label)
                                    .on_click(cx.listener(|this, _, _, cx| this.continue_debug(cx))),
                            )
                            .child(
                                widgets::ghost_button("dbg-step-over", "步过").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.debug_step(DebugStep::Over, cx)
                                    }),
                                ),
                            )
                            .child(
                                widgets::ghost_button("dbg-step-in", "步入").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.debug_step(DebugStep::In, cx)
                                    }),
                                ),
                            )
                            .child(
                                widgets::ghost_button("dbg-step-out", "跳出").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.debug_step(DebugStep::Out, cx)
                                    }),
                                ),
                            )
                            .when(session_active, |style| {
                                style.child(
                                    widgets::ghost_button("dbg-stop", "停止").on_click(
                                        cx.listener(|this, _, _, cx| this.stop_debug(cx)),
                                    ),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(3.))
                                    .child(view_tab_pill("dbg-tab-vars", "变量", self.debug.view_tab == DebugViewTab::Variables)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.set_view_tab(DebugViewTab::Variables, cx)
                                        })))
                                    .child(view_tab_pill("dbg-tab-memory", "内存", self.debug.view_tab == DebugViewTab::Memory)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.set_view_tab(DebugViewTab::Memory, cx)
                                        })))
                                    .child(view_tab_pill("dbg-tab-disasm", "反汇编", self.debug.view_tab == DebugViewTab::Disassembly)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.set_view_tab(DebugViewTab::Disassembly, cx)
                                        })))
                                    .child(view_tab_pill("dbg-tab-trace", "跟踪", self.debug.view_tab == DebugViewTab::Trace)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.set_view_tab(DebugViewTab::Trace, cx)
                                        })))
                                    .child(view_tab_pill("dbg-tab-device", "设备", self.debug.view_tab == DebugViewTab::Device)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.set_view_tab(DebugViewTab::Device, cx)
                                        })))
                                    .child(view_tab_pill("dbg-tab-sim", "仿真", self.debug.view_tab == DebugViewTab::Simulator)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.set_view_tab(DebugViewTab::Simulator, cx)
                                        }))),
                            )
                            .child(div().size(px(7.)).rounded_full().bg(status_color))
                            .child(
                                div()
                                    .text_size(px(9.5))
                                    .text_color(rgb(theme::TEXT_FAINT))
                                    .child(status_label),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .child(
                        // 调用栈
                        div()
                            .id("debug-frames-scroll")
                            .w(px(270.))
                            .flex_none()
                            .h_full()
                            .flex()
                            .flex_col()
                            .border_r_1()
                            .border_color(theme::alpha(theme::STROKE, 0.08))
                            .child(
                                div()
                                    .px(px(10.))
                                    .pt(px(7.))
                                    .pb(px(3.))
                                    .child(widgets::section_label("调用栈")),
                            )
                            .child(
                                div()
                                    .id("debug-frames-list")
                                    .flex_1()
                                    .overflow_y_scroll()
                                    .px(px(6.))
                                    .pb(px(6.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .when(self.debug.frames.is_empty(), |style| {
                                        style.child(
                                            div()
                                                .p(px(10.))
                                                .text_size(px(11.))
                                                .text_color(rgb(theme::TEXT_FAINT))
                                                .child("没有调用栈。命中断点后显示。"),
                                        )
                                    })
                                    .children(
                                        self.debug.frames.iter().enumerate().map(|(index, frame)| {
                                            let active = index == active_frame;
                                            let location = frame
                                                .file
                                                .as_deref()
                                                .map(|file| {
                                                    let name = Path::new(file)
                                                        .file_name()
                                                        .map(|name| name.to_string_lossy().to_string())
                                                        .unwrap_or_else(|| file.to_string());
                                                    match frame.line {
                                                        Some(line) => format!("{name}:{line}"),
                                                        None => name,
                                                    }
                                                })
                                                .unwrap_or_default();
                                            div()
                                                .id(("dbg-frame", index))
                                                .px(px(8.))
                                                .py(px(5.))
                                                .rounded(px(6.))
                                                .cursor_pointer()
                                                .bg(if active {
                                                    theme::alpha(theme::JADE, 0.14)
                                                } else {
                                                    gpui::transparent_black()
                                                })
                                                .hover(|style| {
                                                    style.bg(theme::alpha(theme::STROKE, 0.07))
                                                })
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.select_debug_frame(index, cx)
                                                }))
                                                .child(
                                                    div()
                                                        .overflow_hidden()
                                                        .text_ellipsis()
                                                        .whitespace_nowrap()
                                                        .font_family(theme::MONO_FONT)
                                                        .text_size(px(10.5))
                                                        .text_color(rgb(theme::TEXT))
                                                        .child(frame.name.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .overflow_hidden()
                                                        .text_ellipsis()
                                                        .whitespace_nowrap()
                                                        .text_size(px(9.))
                                                        .text_color(rgb(theme::TEXT_FAINT))
                                                        .child(location),
                                                )
                                        })),
                            ),
                    )
                    .child(match self.debug.view_tab {
                        DebugViewTab::Variables => {
                            self.render_debug_variables_column(cx)
                        }
                        DebugViewTab::Memory => self.render_debug_memory_column(cx),
                        DebugViewTab::Disassembly => {
                            self.render_debug_disassembly_column(cx)
                        }
                        DebugViewTab::Trace => self.render_debug_trace_column(cx),
                        DebugViewTab::Device => self.render_debug_device_column(cx),
                        DebugViewTab::Simulator => self.render_simulator_column(cx),
                    })
                    .child(
                        // 控制台
                        div()
                            .id("debug-console-scroll")
                            .flex_1()
                            .h_full()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .px(px(10.))
                                    .pt(px(7.))
                                    .pb(px(3.))
                                    .child(widgets::section_label("控制台")),
                            )
                            .child(
                                div()
                                    .id("debug-console-list")
                                    .flex_1()
                                    .overflow_y_scroll()
                                    .px(px(10.))
                                    .pb(px(6.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(1.))
                                    .when(console_empty, |style| {
                                        style.child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(rgb(theme::TEXT_FAINT))
                                                .child("按 F5 或点击顶部「调试」构建并启动调试会话（lldb-dap）。"),
                                        )
                                    })
                                    .children(self.debug.console.iter().enumerate().map(
                                        |(index, line)| {
                                            let color = if line.starts_with('✕') {
                                                theme::color(theme::DANGER)
                                            } else if line.starts_with('●') {
                                                theme::color(theme::JADE)
                                            } else if line.starts_with('▶') {
                                                theme::color(theme::ACCENT)
                                            } else if line.starts_with("! ") {
                                                theme::color(theme::ACCENT)
                                            } else {
                                                theme::color(theme::TEXT_DIM)
                                            };
                                            div()
                                                .id(("dbg-console", index))
                                                .whitespace_nowrap()
                                                .font_family(theme::MONO_FONT)
                                                .text_size(px(9.5))
                                                .text_color(color)
                                                .child(line.clone())
                                        },
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .px(px(10.))
                            .py(px(6.))
                            .border_t_1()
                            .border_color(theme::alpha(theme::STROKE, 0.08))
                            .when(self.debug.console_input.is_none(), |style| {
                                style.child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(rgb(theme::TEXT_FAINT))
                                        .child("调试会话未运行"),
                                )
                            })
                            .children(
                                self.debug
                                    .console_input
                                    .iter()
                                    .map(|field| div().w_full().child(field.clone())),
                            ),
                    ),
            )
    }

    fn render_debug_variables_column(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active_scope = self.debug.active_scope;
        div()
            .id("debug-vars-scroll")
            .w(px(390.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme::alpha(theme::STROKE, 0.08))
            .child(
                div()
                    .px(px(10.))
                    .pt(px(7.))
                    .pb(px(3.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(widgets::section_label("变量"))
                    .child(
                        div()
                            .flex()
                            .gap(px(4.))
                            .children(self.debug.scopes.iter().enumerate().map(
                                |(index, scope)| {
                                    let active = index == active_scope;
                                    div()
                                        .id(("dbg-scope", index))
                                        .px(px(8.))
                                        .py(px(3.))
                                        .rounded(px(9.))
                                        .text_size(px(9.))
                                        .cursor_pointer()
                                        .bg(if active {
                                            theme::alpha(theme::ACCENT, 0.18)
                                        } else {
                                            gpui::transparent_black()
                                        })
                                        .text_color(rgb(theme::TEXT_DIM))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.select_debug_scope(index, cx)
                                        }))
                                        .child(scope.name.clone())
                                },
                            )),
                    ),
            )
            .child(
                div()
                    .px(px(10.))
                    .pt(px(6.))
                    .pb(px(4.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(widgets::section_label("监视"))
                    .children(
                        self.debug.watches.iter().enumerate().map(|(index, watch)| {
                            div()
                                .id(("dbg-watch", index))
                                .px(px(6.))
                                .py(px(3.))
                                .rounded(px(6.))
                                .flex()
                                .items_baseline()
                                .gap(px(6.))
                                .font_family(theme::MONO_FONT)
                                .text_size(px(10.5))
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(rgb(theme::ACCENT_3))
                                        .child(watch.expression.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_color(rgb(theme::TEXT_DIM))
                                        .child(if watch.value.is_empty() {
                                            "…".to_string()
                                        } else {
                                            watch.value.clone()
                                        }),
                                )
                                .child(
                                    div()
                                        .id(("dbg-watch-del", index))
                                        .flex_none()
                                        .text_size(px(10.))
                                        .text_color(rgb(theme::TEXT_FAINT))
                                        .cursor_pointer()
                                        .hover(|style| style.text_color(rgb(theme::DANGER)))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.debug.remove_watch(index);
                                            cx.notify();
                                        }))
                                        .child("✕")
                                )
                        }),
                    )
                    .children(
                        self.debug
                            .watch_input
                            .iter()
                            .map(|field| div().w_full().child(field.clone())),
                    ),
            )
            .child(
                div()
                    .id("debug-variables-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(6.))
                    .pb(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .when(self.debug.variables.is_empty(), |style| {
                        style.child(
                            div()
                                .p(px(10.))
                                .text_size(px(11.))
                                .text_color(rgb(theme::TEXT_FAINT))
                                .child("没有变量。命中断点后显示。"),
                        )
                    })
                    .children(self.variable_rows(&self.debug.variables, 0, 0, &mut 0, cx)),
            )
            .into_any_element()
    }

    fn render_debug_memory_column(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let address = self.debug.memory.address;
        let bytes = &self.debug.memory.bytes;
        let read_size = self.debug.memory.read_size;
        let address_text = self.debug.memory.error.clone().unwrap_or_default();
        let has_error = !address_text.is_empty();

        let mut hex_rows: Vec<gpui::AnyElement> = Vec::new();
        for (row_index, chunk) in bytes.chunks(16).enumerate() {
            let line_address = address + (row_index as u64) * 16;
            let hex = chunk
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let ascii: String = chunk
                .iter()
                .map(|byte| {
                    if (32..127).contains(byte) {
                        char::from(*byte)
                    } else {
                        '.'
                    }
                })
                .collect();
            hex_rows.push(
                div()
                    .id(("dbg-mem-row", row_index))
                    .whitespace_nowrap()
                    .font_family(theme::MONO_FONT)
                    .text_size(px(9.5))
                    .text_color(rgb(theme::TEXT_DIM))
                    .child(format!("{line_address:#010x}  {hex:<47} │ {ascii}"))
                    .into_any_element(),
            );
        }

        div()
            .id("debug-memory")
            .w(px(460.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme::alpha(theme::STROKE, 0.08))
            .child(
                div()
                    .px(px(10.))
                    .pt(px(7.))
                    .pb(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(widgets::section_label("内存"))
                            .child(
                                div().flex().gap(px(4.)).children(
                                    [(64u64, "64B"), (128, "128B"), (256, "256B")]
                                        .into_iter()
                                        .map(|(size, label)| {
                                            memory_size_pill(
                                                ("mem-size", size as usize),
                                                label,
                                                read_size == size,
                                            )
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.set_memory_read_size(size, cx)
                                            }))
                                        }),
                                ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                self.debug
                                    .memory
                                    .address_input
                                    .clone()
                                    .map(|field| div().flex_1().child(field).into_any_element())
                                    .unwrap_or_else(|| {
                                        div().flex_1().into_any_element()
                                    }),
                            )
                            .child(
                                widgets::primary_button("dbg-mem-read", "读取").on_click(
                                    cx.listener(|this, _, _, cx| this.read_memory(cx)),
                                ),
                            ),
                    ),
            )
            .when(has_error, |style| {
                style.child(
                    div()
                        .px(px(10.))
                        .pb(px(4.))
                        .text_size(px(9.5))
                        .text_color(rgb(theme::DANGER))
                        .child(address_text),
                )
            })
            .child(
                div()
                    .id("debug-memory-dump")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(10.))
                    .pb(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .when(hex_rows.is_empty(), |style| {
                        style.child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(theme::TEXT_FAINT))
                                .child("输入十六进制地址后点击「读取」，例如 0x1000003e8。"),
                        )
                    })
                    .children(hex_rows),
            )
            .into_any_element()
    }

    fn render_debug_disassembly_column(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let pc = self.debug.pc;
        let instructions = &self.debug.disassembly;

        div()
            .id("debug-disasm")
            .w(px(460.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme::alpha(theme::STROKE, 0.08))
            .child(
                div()
                    .px(px(10.))
                    .pt(px(7.))
                    .pb(px(6.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(widgets::section_label("反汇编"))
                    .child(
                        widgets::ghost_button("dbg-disasm-btn", "从 PC 反汇编").on_click(
                            cx.listener(|this, _, _, cx| this.disassemble_at_pc(cx)),
                        ),
                    ),
            )
            .child(
                div()
                    .px(px(10.))
                    .pb(px(6.))
                    .text_size(px(9.5))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(match pc {
                        Some(pc) => format!("pc = {pc:#x}"),
                        None => "程序未暂停".to_string(),
                    }),
            )
            .child(
                div()
                    .id("debug-disasm-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(6.))
                    .pb(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .when(instructions.is_empty(), |style| {
                        style.child(
                            div()
                                .p(px(10.))
                                .text_size(px(11.))
                                .text_color(rgb(theme::TEXT_FAINT))
                                .child("没有反汇编。命中断点后点击「从 PC 反汇编」。"),
                        )
                    })
                    .children(
                        instructions
                            .iter()
                            .enumerate()
                            .flat_map(|(index, instruction)| {
                                let mut row_elements: Vec<gpui::AnyElement> = Vec::new();
                                let is_pc = pc == Some(instruction.address);
                                let previous_source = index
                                    .checked_sub(1)
                                    .and_then(|previous| instructions.get(previous))
                                    .and_then(|previous| {
                                        previous
                                            .file
                                            .as_ref()
                                            .map(|file| (file.clone(), previous.line))
                                    });
                                let current_source = instruction
                                    .file
                                    .as_ref()
                                    .map(|file| (file.clone(), instruction.line));
                                let starts_new_source = match (&previous_source, &current_source) {
                                    (None, Some(_)) => true,
                                    (Some(previous), Some(current)) => previous != current,
                                    _ => false,
                                };
                                if starts_new_source
                                    && let Some((file, line)) = current_source
                                    && let Some(source_text) =
                                        self.source_line_text(&file, line.unwrap_or(1), cx)
                                {
                                    let source_path = file.clone();
                                    let source_line = line.unwrap_or(1);
                                    let source_name = Path::new(&file)
                                        .file_name()
                                        .map(|name| name.to_string_lossy().to_string())
                                        .unwrap_or_else(|| file.clone());
                                    row_elements.push(
                                        div()
                                            .id(("dbg-disasm-source", index))
                                            .px(px(8.))
                                            .py(px(3.))
                                            .rounded(px(4.))
                                            .mt(px(6.))
                                            .flex()
                                            .items_center()
                                            .gap(px(10.))
                                            .cursor_pointer()
                                            .hover(|style| {
                                                style.bg(theme::alpha(theme::STROKE, 0.08))
                                            })
                                            .on_click(cx.listener({
                                                let path = source_path.clone();
                                                let line = source_line;
                                                move |this, _, _, cx| {
                                                    this.open_file(PathBuf::from(&path), cx);
                                                    if let Some(editor) = this.active_editor() {
                                                        editor.update(cx, |editor, cx| {
                                                            editor.set_cursor_line(line, cx)
                                                        });
                                                    }
                                                }
                                            }))
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .font_family(theme::MONO_FONT)
                                                    .text_size(px(9.))
                                                    .text_color(rgb(theme::JADE))
                                                    .child(format!("{source_name}:{source_line}")),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .overflow_hidden()
                                                    .text_ellipsis()
                                                    .whitespace_nowrap()
                                                    .text_size(px(10.))
                                                    .text_color(rgb(theme::TEXT_FAINT))
                                                    .child(source_text),
                                            )
                                            .into_any_element(),
                                    );
                                }
                                row_elements.push(
                                    div()
                                        .id(("dbg-disasm-row", index))
                                        .px(px(8.))
                                        .py(px(2.))
                                        .rounded(px(4.))
                                        .flex()
                                        .items_baseline()
                                        .gap(px(10.))
                                        .overflow_hidden()
                                        .bg(if is_pc {
                                            theme::alpha(theme::JADE, 0.14)
                                        } else {
                                            gpui::transparent_black()
                                        })
                                        .child(
                                            div()
                                                .flex_none()
                                                .font_family(theme::MONO_FONT)
                                                .text_size(px(10.))
                                                .text_color(rgb(theme::ACCENT_3))
                                                .child(format!("{:#x}", instruction.address)),
                                        )
                                        .child(
                                            div()
                                                .font_family(theme::MONO_FONT)
                                                .text_size(px(10.))
                                                .text_color(if is_pc {
                                                    rgb(theme::TEXT)
                                                } else {
                                                    rgb(theme::TEXT_DIM)
                                                })
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .whitespace_nowrap()
                                                .child(instruction.instruction.clone()),
                                        )
                                        .into_any_element(),
                                );
                                row_elements
                            }),
                    )
            )
            .into_any_element()
    }

    fn render_debug_trace_column(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let trace = &self.debug.trace;
        let config = &trace.config;
        let support = trace.support;

        let row_field = |id: &'static str, label: &str, field: &Option<Entity<TextField>>| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .w(px(110.))
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(rgb(theme::TEXT_DIM))
                        .child(label.to_string()),
                )
                .child(
                    field
                        .clone()
                        .map(|field| {
                            div()
                                .id(id)
                                .flex_1()
                                .bg(theme::alpha(theme::STROKE, 0.06))
                                .rounded(px(6.))
                                .px(px(8.))
                                .py(px(3.))
                                .child(field)
                                .into_any_element()
                        })
                        .unwrap_or_else(|| div().flex_1().into_any_element()),
                )
                .into_any_element()
        };

        div()
            .id("debug-trace")
            .w(px(460.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme::alpha(theme::STROKE, 0.08))
            .child(
                div()
                    .px(px(10.))
                    .pt(px(7.))
                    .pb(px(6.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(widgets::section_label("跟踪窗口 (SWO/SWV)"))
                    .child(
                        div()
                            .flex()
                            .gap(px(4.))
                            .children(
                                vec![
                                    widgets::icon_button("dbg-trace-mode", "●", 18.)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.trace_cycle_mode(cx);
                                        }))
                                        .into_any_element(),
                                    if trace.armed {
                                        widgets::ghost_button("dbg-trace-stop", "停用")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.trace_disarm(cx)
                                            }))
                                            .into_any_element()
                                    } else {
                                        widgets::primary_button("dbg-trace-arm", "启用")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.trace_arm(cx)
                                            }))
                                            .into_any_element()
                                    },
                                ],
                            ),
                    ),
            )
            .child(
                div()
                    .px(px(10.))
                    .pb(px(6.))
                    .text_size(px(9.5))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(match trace.armed {
                        true => format!("已启用 · {}", config.mode.label()),
                        false => "未启用".to_string(),
                    }),
            )
            .when(support.is_some(), |style| {
                style.child(
                    div()
                        .px(px(10.))
                        .pb(px(6.))
                        .text_size(px(9.5))
                        .text_color(rgb(theme::ACCENT))
                        .child(support.unwrap_or("")),
                )
            })
            .when(trace.error.is_some(), |style| {
                style.child(
                    div()
                        .px(px(10.))
                        .pb(px(4.))
                        .text_size(px(9.5))
                        .text_color(rgb(theme::DANGER))
                        .child(trace.error.clone().unwrap_or_default()),
                )
            })
            .child(
                div()
                    .id("debug-trace-body")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(10.))
                    .pb(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(row_field("dbg-trace-port", "ITM 端口", &trace.port_input))
                    .child(row_field("dbg-trace-prescaler", "SWO 分频", &trace.prescaler_input))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .w(px(110.))
                                    .flex_none()
                                    .text_size(px(10.))
                                    .text_color(rgb(theme::TEXT_DIM))
                                    .child("数据跟踪"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(
                                        widgets::pill(
                                            if config.data_trace { "开" } else { "关" },
                                            if config.data_trace {
                                                theme::color(theme::JADE)
                                            } else {
                                                theme::color(theme::TEXT_FAINT)
                                            },
                                        )
                                        .id("dbg-trace-data-pill")
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.trace_toggle_data(cx);
                                        })),
                                    )
                                    .when(config.data_trace, |style| {
                                        style.child(
                                            div()
                                                .text_size(px(9.5))
                                                .text_color(rgb(theme::TEXT_FAINT))
                                                .child(format!(
                                                    "比较器 {}",
                                                    config.data_comparator
                                                )),
                                        )
                                    }),
                            ),
                    )
                    .when(config.data_trace, |style| {
                        style.child(row_field("dbg-trace-address", "监视地址", &trace.address_input))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .w(px(110.))
                                    .flex_none()
                                    .text_size(px(10.))
                                    .text_color(rgb(theme::TEXT_DIM))
                                    .child("运行模式"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(10.))
                                    .text_color(rgb(theme::TEXT))
                                    .child(config.mode.label()),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(rgb(theme::TEXT_FAINT))
                            .child("DWT 数据跟踪监视地址写入；ITM 端口重定向 printf。"),
                    ),
            )
            .into_any_element()
    }

    /// Device register view: peripheral registers from the target's SVD.
    fn render_debug_device_column(&self, _cx: &mut Context<Self>) -> gpui::AnyElement {
        let device = self.device_for_project();

        let mut rows: Vec<gpui::AnyElement> = Vec::new();

        match device {
            Some(device) => {
                let peripherals = device.sorted_peripherals();
                if peripherals.is_empty() {
                    rows.push(
                        div()
                            .p(px(12.))
                            .text_size(px(11.))
                            .text_color(rgb(theme::TEXT_FAINT))
                            .child("该目标没有已定义的 SVD 外设。")
                            .into_any_element(),
                    );
                }
                for peripheral in peripherals {
                    rows.push(
                        div()
                            .id(gpui::SharedString::from(format!(
                                "dev-periph-{}",
                                peripheral.name
                            )))
                            .pt(px(8.))
                            .pb(px(3.))
                            .px(px(4.))
                            .text_size(px(9.5))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme::color(theme::ACCENT))
                            .child(format!(
                                "{}  @ 0x{:08X}",
                                peripheral.name,
                                peripheral.base_address
                            ))
                            .into_any_element(),
                    );
                    for register in &peripheral.registers {
                        let field_summary: String = register
                            .fields
                            .iter()
                            .map(|field| {
                                format!("{}[{}:{}]", field.name, field.bit_offset + field.bit_width - 1, field.bit_offset)
                            })
                            .collect::<Vec<_>>()
                            .join(" · ");
                        rows.push(
                            div()
                                .id(("dev-reg", register.address))
                                .px(px(4.))
                                .py(px(2.))
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .w(px(120.))
                                        .flex_none()
                                        .font_family(theme::MONO_FONT)
                                        .text_size(px(10.))
                                        .text_color(rgb(theme::TEXT_DIM))
                                        .child(format!("0x{:08X}", register.address)),
                                )
                                .child(
                                    div()
                                        .w(px(110.))
                                        .flex_none()
                                        .font_family(theme::MONO_FONT)
                                        .text_size(px(10.))
                                        .text_color(rgb(theme::TEXT))
                                        .child(register.name.clone()),
                                )
                                .child(
                                    div()
                                        .w(px(90.))
                                        .flex_none()
                                        .font_family(theme::MONO_FONT)
                                        .text_size(px(10.))
                                        .text_color(rgb(theme::TEXT_FAINT))
                                        .child(format!("0x{:08X}", register.reset_value)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_size(px(9.5))
                                        .text_color(rgb(theme::TEXT_FAINT))
                                        .child(field_summary),
                                )
                                .into_any_element(),
                        );
                    }
                }
            }
            None => {
                let target = self
                    .project
                    .as_ref()
                    .map(|project| project.config.project.target.clone())
                    .unwrap_or_default();
                rows.push(
                    div()
                        .p(px(12.))
                        .text_size(px(11.))
                        .text_color(rgb(theme::TEXT_FAINT))
                        .child(if target.is_empty() {
                            "项目未设置目标设备；在「项目设置」或向导中选择 MCU 以显示外设寄存器。".to_string()
                        } else {
                            format!("目标设备「{target}」不在内置设备库中，或 SVD 解析失败（stm32f103c8 / stm32f407vg / stm32g431rb / nrf52840 / rp2040 / esp32c3）。")
                        })
                        .into_any_element(),
                );
            }
        }

        div()
            .id("debug-device-scroll")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .p(px(6.))
            .flex()
            .flex_col()
            .gap(px(2.))
            .children(rows)
            .into_any_element()
    }

    /// Resolve the target device for the loaded project.
    ///
    /// Prefers an explicit `svd` source (built-in catalog key or external file
    /// path); otherwise falls back to deriving the built-in device from the
    /// project `target`.
    fn device_for_project(&self) -> Option<ns_device::Device> {
        let project = self.project.as_ref()?;
        let svd = project.config.project.svd.trim().to_string();
        if !svd.is_empty() {
            if ns_device::catalog_entry(&svd).is_some() {
                return ns_device::device_for_target(&svd);
            }
            let path = project.root.join(&svd);
            return ns_device::load_svd_file(&path).ok();
        }
        let target = project.config.project.target.trim().to_string();
        if target.is_empty() {
            return None;
        }
        ns_device::device_for_target(&target)
    }


    /// Built-in simulator view: controls, registers, PC, cycle count, UART.
    fn render_simulator_column(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut rows: Vec<gpui::AnyElement> = Vec::new();

        // Control bar.
        rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    widgets::ghost_button("sim-load", "加载产物并运行")
                        .on_click(cx.listener(|this, _, _, cx| this.start_simulation(cx))),
                )
                .child(
                    widgets::ghost_button("sim-step", "单步")
                        .on_click(cx.listener(|this, _, _, cx| this.sim_step(cx))),
                )
                .child(
                    widgets::ghost_button("sim-run", "运行")
                        .on_click(cx.listener(|this, _, _, cx| this.sim_run(cx))),
                )
                .child(
                    widgets::ghost_button("sim-reset", "复位")
                        .on_click(cx.listener(|this, _, _, cx| this.sim_reset(cx))),
                )
                .child(
                    widgets::ghost_button("sim-stop", "停止")
                        .on_click(cx.listener(|this, _, _, cx| this.sim_stop(cx))),
                )
        .into_any_element(),
        );

        // Status line.
        rows.push(
            div()
                .text_size(px(10.))
                .text_color(rgb(theme::TEXT_DIM))
                .child(if self.sim.status.is_empty() {
                    "内建指令级仿真器（RV32I）。点击「加载产物并运行」把构建产物载入仿真器。".to_string()
                } else {
                    self.sim.status.clone()
                })
                .into_any_element(),
        );

        if let Some(core) = &self.sim.core {
            // PC + cycles.
            rows.push(
                div()
                    .flex()
                    .gap(px(16.))
                    .text_size(px(10.))
                    .text_color(rgb(theme::TEXT))
                    .child(format!("PC: 0x{:08x}", core.pc))
                    .child(format!("周期: {}", core.cycle_count))
                    .child(if core.halted { "状态: 已停止" } else { "状态: 运行中" })
                    .into_any_element(),
            );

            // Register file (x0..x31), 4 columns.
            let mut reg_rows: Vec<gpui::AnyElement> = Vec::new();
            for chunk in (0..ns_sim::REG_COUNT).collect::<Vec<_>>().chunks(4) {
                let mut cells: Vec<gpui::AnyElement> = Vec::new();
                for index in chunk {
                    let value = core.read_reg(*index);
                    cells.push(
                        div()
                            .flex_1()
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(px(4.))
                            .bg(theme::alpha(theme::STROKE, 0.05))
                            .text_size(px(9.5))
                            .child(format!("{}  0x{:08x}", ns_sim::reg_name(*index), value))
                            .into_any_element(),
                    );
                }
                reg_rows.push(div().flex().gap(px(4.)).children(cells).into_any_element());
            }
            rows.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .children(reg_rows)
                    .into_any_element(),
            );

            // UART output.
            let uart: String = String::from_utf8_lossy(&core.peripherals.outputs).into_owned();
            rows.push(
                div()
                    .text_size(px(9.5))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child(if uart.is_empty() {
                        "UART 输出: （空）".to_string()
                    } else {
                        format!("UART 输出: {uart}")
                    })
                    .into_any_element(),
            );
        } else {
            rows.push(
                div()
                    .text_size(px(10.))
                    .text_color(rgb(theme::TEXT_FAINT))
                    .child("未加载程序。先构建项目，再点「加载产物并运行」。")
                    .into_any_element(),
            );
        }

        div()
            .id("simulator-column")
            .p(px(10.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .children(rows)
            .into_any_element()
    }

    fn render_statusbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (dot, label) = match &self.build_state {
            BuildState::Idle => (theme::color(theme::TEXT_FAINT), "就绪".to_string()),
            BuildState::Running => (theme::color(theme::ACCENT), "构建中…".to_string()),
            BuildState::Success(duration) => (
                theme::color(theme::JADE),
                format!("构建成功 {:.2}s", duration.as_secs_f64()),
            ),
            BuildState::Failed(duration) => (
                theme::color(theme::DANGER),
                format!("构建失败 {:.2}s", duration.as_secs_f64()),
            ),
        };
        let cursor = self
            .active_editor()
            .map(|editor| editor.read(cx).cursor_line_col());
        let language = self
            .project
            .as_ref()
            .map(|project| project.config.project.language.to_string())
            .unwrap_or_else(|| "—".to_string());
        let target = self
            .project
            .as_ref()
            .map(|project| {
                if project.config.project.target.is_empty() {
                    "未指定目标".to_string()
                } else {
                    project.config.project.target.clone()
                }
            })
            .unwrap_or_else(|| "—".to_string());

        div()
            .h(px(27.))
            .flex_none()
            .px(px(12.))
            .flex()
            .items_center()
            .justify_between()
            .bg(theme::color(theme::BG_1).opacity(0.86))
            .border_t_1()
            .border_color(theme::alpha(theme::STROKE, 0.12))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .child(widgets::status_dot(dot, 5.))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(theme::TEXT_DIM))
                            .child(label),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(theme::TEXT_FAINT))
                            .child(format!("· {}", self.status)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .child(status_item("目标", target))
                    .child(status_item("语言", language))
                    .child(status_item(
                        "光标",
                        cursor
                            .map(|(line, column)| format!("{line}:{column}"))
                            .unwrap_or_else(|| "—".to_string()),
                    ))
                    .child(status_item("工具链", "project.toml".to_string()))
                    .child(status_item(
                        "断点",
                        self.breakpoints.enabled_count().to_string(),
                    ))
                    .when(self.debug_session.is_some(), |style| {
                        style.child(status_item("调试", self.debug.status.label()))
                    })
                    .when(self.project.is_some(), |style| {
                        let git_label = if self.git.status.in_repo {
                            format!(
                                "{} · {} 变更",
                                self.git.status.branch,
                                self.git.status.changes
                            )
                        } else {
                            "非 Git 仓库".to_string()
                        };
                        style.child(
                            div()
                                .id("status-git")
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .cursor_pointer()
                                .hover(|s| s.opacity(0.8))
                                .on_click(cx.listener(|this, _, _, cx| this.show_git(cx)))
                                .child(status_item("Git", git_label)),
                        )
                    })
                    .when(self.run.started, |style| {
                        let label = if self.run.running {
                            format!("运行中 · {}", self.run.command)
                        } else {
                            format!(
                                "已退出 ({})",
                                self.run.exit_code
                                    .map(|code| code.to_string())
                                    .unwrap_or_else(|| "?".to_string())
                            )
                        };
                        style.child(status_item("运行", label))
                    }),
            )
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let recents: Vec<RecentProject> = self.recents.iter().take(5).cloned().collect();
        div()
            .flex_1()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(32.))
            .child(
                div()
                    .w(px(720.))
                    .flex()
                    .flex_col()
                    .gap(px(22.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(18.))
                            .child(widgets::brand_mark(62.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(7.))
                                    .child(
                                        div()
                                            .text_size(px(27.))
                                            .font_family(theme::SERIF_FONT)
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(rgb(theme::TEXT))
                                            .child("ns-embed-ide"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.5))
                                            .text_color(rgb(theme::TEXT_DIM))
                                            .child("商量嵌入式开发环境 · 配置即文件，工具链开放可替换"),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(theme::TEXT_DIM))
                            .child(
                                "面向 C / C++ / Rust / Zig / ZR 的跨平台嵌入式 IDE。\
                                 在 project.toml 中声明语言、编译器、仿真器、调试器与烧写器，\
                                 让 IDE 不硬编码任何工具路径。",
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                widgets::primary_button("welcome-new", "新建项目").on_click(
                                    cx.listener(|this, _, window, cx| this.open_wizard(window, cx)),
                                ),
                            )
                            .child(
                                widgets::ghost_button("welcome-open", "打开已有项目").on_click(
                                    cx.listener(|this, _, _, cx| this.open_project_dialog(cx)),
                                ),
                            )
                            .child(widgets::pill("Rust + GPUI", theme::color(theme::ACCENT)))
                            .child(widgets::pill("project.toml", theme::color(theme::JADE))),
                    )
                    .child(
                        div()
                            .rounded(px(14.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.14))
                            .bg(theme::color(theme::BG_1).opacity(0.55))
                            .overflow_hidden()
                            .child(
                                div()
                                    .h(px(38.))
                                    .px(px(15.))
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .border_b_1()
                                    .border_color(theme::alpha(theme::STROKE, 0.08))
                                    .child(widgets::section_label("最近项目"))
                                    .child(
                                        div()
                                            .text_size(px(9.5))
                                            .text_color(rgb(theme::TEXT_FAINT))
                                            .child(format!("{} 个", recents.len())),
                                    ),
                            )
                            .when(recents.is_empty(), |style| {
                                style.child(
                                    div()
                                        .px(px(16.))
                                        .py(px(22.))
                                        .text_size(px(11.5))
                                        .text_color(rgb(theme::TEXT_FAINT))
                                        .child("还没有最近项目。新建一个项目，或从磁盘打开已有工程。"),
                                )
                            })
                            .children(recents.into_iter().enumerate().map(|(index, recent)| {
                                let path = recent.path.clone();
                                div()
                                    .id(("recent", index))
                                    .px(px(15.))
                                    .py(px(10.))
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(16.))
                                    .when(index > 0, |style| {
                                        style
                                            .border_t_1()
                                            .border_color(theme::alpha(theme::STROKE, 0.06))
                                    })
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.06)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_project_path(path.clone(), cx)
                                    }))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap(px(2.))
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .text_color(rgb(theme::TEXT))
                                                    .child(recent.name),
                                            )
                                            .child(
                                                div()
                                                    .max_w(px(540.))
                                                    .overflow_hidden()
                                                    .text_ellipsis()
                                                    .whitespace_nowrap()
                                                    .font_family(theme::MONO_FONT)
                                                    .text_size(px(9.5))
                                                    .text_color(rgb(theme::TEXT_FAINT))
                                                    .child(recent.path.display().to_string()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.)
                                            )
                                            .text_color(theme::alpha(theme::ACCENT, 0.8))
                                            .child("打开 →"),
                                    )
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(16.))
                            .child(feature_note("构建", "解析 project.toml 并调用外部编译器"))
                            .child(feature_note("编辑", "语法高亮 · 标签页 · 基础编辑"))
                            .child(feature_note("下一步", "调试、烧写与仿真适配器")),
                    )
                    .when(self.status != "就绪" && !self.status.is_empty(), |style| {
                        style.child(
                            div()
                                .rounded(px(10.))
                                .px(px(13.))
                                .py(px(9.))
                                .bg(theme::alpha(theme::CINNABAR, 0.12))
                                .border_1()
                                .border_color(theme::alpha(theme::CINNABAR, 0.25))
                                .text_size(px(11.))
                                .text_color(rgb(theme::TEXT_DIM))
                                .child(self.status.clone()),
                        )
                    }),
            )
    }

    fn render_project(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .h_full()
            .flex()
            .overflow_hidden()
            .child(self.render_sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(self.render_editor_area(cx))
                    .when(self.show_output, |style| {
                        style.child(self.render_output(cx))
                    }),
            )
    }
}

impl Render for IdeApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_on_next_frame {
            self.focus_on_next_frame = false;
            if let Some(editor) = self.active_editor() {
                window.focus(&editor.read(cx).focus_handle(cx));
            } else if self.wizard.is_none() {
                window.focus(&self.focus_handle);
            }
        }
        if self.focus_terminal {
            self.focus_terminal = false;
            if let Some(terminal) = self.terminal.as_ref() {
                window.focus(&terminal.read(cx).focus_handle());
            }
        }
        if self.focus_dialog_on_next_frame {
            self.focus_dialog_on_next_frame = false;
            if let Some(dialog) = &self.condition_dialog {
                window.focus(&dialog.field.read(cx).focus_handle(cx));
            } else if let Some(dialog) = &self.watchpoint_dialog {
                window.focus(&dialog.field.read(cx).focus_handle(cx));
            } else if let Some(dialog) = &self.set_var_dialog {
                window.focus(&dialog.field.read(cx).focus_handle(cx));
            } else if let Some(dialog) = &self.settings_dialog {
                window.focus(&dialog.name.read(cx).focus_handle(cx));
            } else if let Some(dialog) = &self.toolchain_dialog {
                if let Some((_, field)) = dialog.fields.first() {
                    window.focus(&field.read(cx).focus_handle(cx));
                }
            }
        }

        let mut root = div()
            .size_full()
            .relative()
            .overflow_hidden()
            .flex()
            .flex_col()
            .bg(theme::app_background())
            .font_family(theme::UI_FONT)
            .text_color(rgb(theme::TEXT))
            .track_focus(&self.focus_handle)
            .key_context(if self.wizard.is_some() {
                "Wizard"
            } else if self.condition_dialog.is_some()
                || self.watchpoint_dialog.is_some()
                || self.settings_dialog.is_some()
                || self.toolchain_dialog.is_some()
            {
                "Dialog"
            } else if self.search.is_some() {
                "Search"
            } else if self.palette.is_some() {
                "Palette"
            } else {
                "App"
            })
            .on_action(cx.listener(|this, _: &NewProject, window, cx| {
                this.open_wizard(window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenProject, _, cx| {
                this.open_project_dialog(cx)
            }))
            .on_action(cx.listener(|this, _: &Save, _, cx| this.save_active(cx)))
            .on_action(cx.listener(|this, _: &Build, _, cx| this.start_build(false, cx)))
            .on_action(cx.listener(|this, _: &Run, _, cx| this.start_build(true, cx)))
            .on_action(cx.listener(|this, _: &ClearBreakpoints, _, cx| {
                this.clear_all_breakpoints(cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleOutput, _, cx| {
                this.show_output = !this.show_output;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &CloseTab, _, cx| {
                if let Some(index) = this.active_tab {
                    this.close_tab(index, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NextTab, _, cx| this.next_tab(true, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, _, cx| this.next_tab(false, cx)))
            .on_action(cx.listener(|this, _: &Settings, _, cx| this.open_settings(cx)))
            .on_action(cx.listener(|this, _: &Flash, _, cx| this.start_flash(cx)))
            .on_action(cx.listener(|this, _: &QuitApp, _, cx| {
                if let Some(session) = this.debug_session.take() {
                    let _ = session.commands.send(DebugCommand::Stop);
                }
                this.save_session(cx);
                cx.quit()
            }))
            .on_action(cx.listener(|this, _: &WizardNext, _, cx| {
                if this.wizard.is_some() {
                    this.wizard_next(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &WizardBack, _, cx| {
                if this.wizard.is_some() {
                    this.wizard_back(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Palette, window, cx| this.open_palette(window, cx)))
            .on_action(cx.listener(|this, _: &PaletteConfirm, window, cx| {
                this.confirm_palette(window, cx)
            }))
            .on_action(cx.listener(|this, _: &PaletteSelectUp, _, cx| {
                this.move_palette_selection(-1, cx)
            }))
            .on_action(cx.listener(|this, _: &PaletteSelectDown, _, cx| {
                this.move_palette_selection(1, cx)
            }))
            .on_action(cx.listener(|this, _: &FindInProject, window, cx| {
                this.open_search(window, cx)
            }))
            .on_action(cx.listener(|this, _: &FindInFile, window, cx| {
                this.find_in_file(window, cx)
            }))
            .on_action(cx.listener(|this, _: &SearchConfirm, _, cx| this.confirm_search(cx)))
            .on_action(cx.listener(|this, _: &SearchSelectUp, _, cx| {
                this.move_search_selection(-1, cx)
            }))
            .on_action(cx.listener(|this, _: &SearchSelectDown, _, cx| {
                this.move_search_selection(1, cx)
            }))
            .on_action(cx.listener(|this, _: &ConditionDialogSave, _, cx| {
                if this.condition_dialog.is_some() {
                    this.save_condition_dialog(cx);
                } else if this.watchpoint_dialog.is_some() {
                    this.save_watchpoint_dialog(cx);
                } else if this.set_var_dialog.is_some() {
                    this.save_set_var_dialog(cx);
                } else if this.settings_dialog.is_some() {
                } else if this.toolchain_dialog.is_some() {
                    this.save_toolchain(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Debug, _, cx| this.start_debug(cx)))
            .on_action(cx.listener(|this, _: &StopDebug, _, cx| this.stop_debug(cx)))
            .on_action(cx.listener(|this, _: &DebugStepOver, _, cx| {
                this.debug_step(DebugStep::Over, cx)
            }))
            .on_action(cx.listener(|this, _: &DebugStepIn, _, cx| {
                this.debug_step(DebugStep::In, cx)
            }))
            .on_action(cx.listener(|this, _: &DebugStepOut, _, cx| {
                this.debug_step(DebugStep::Out, cx)
            }))
            .on_action(cx.listener(|this, _: &RunToCursor, _, cx| {
                this.run_to_cursor(cx)
            }))
            .on_action(cx.listener(|this, _: &EscapeModal, _, cx| {
                if this.wizard.is_some() {
                    this.wizard_cancel(cx);
                } else if this.condition_dialog.is_some() {
                    this.cancel_condition_dialog(cx);
                } else if this.watchpoint_dialog.is_some() {
                    this.cancel_watchpoint_dialog(cx);
                } else if this.set_var_dialog.is_some() {
                    this.cancel_set_var_dialog(cx);
                } else if this.settings_dialog.is_some() {
                    this.cancel_settings(cx);
                } else if this.toolchain_dialog.is_some() {
                    this.cancel_toolchain(cx);
                } else if this.search.is_some() {
                    this.close_search(cx);
                } else if this.palette.is_some() {
                    this.close_palette(cx);
                } else if this.context_menu.is_some() {
                    this.context_menu = None;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &GoToDefinition, _, cx| {
                this.lsp_go_to_definition(cx)
            }))
            .on_action(cx.listener(|this, _: &FindReferences, _, cx| {
                this.lsp_find_references(cx)
            }))
            .on_action(cx.listener(|this, _: &Hover, _, cx| this.lsp_hover(cx)))
            .on_action(cx.listener(|this, _: &SignatureHelp, _, cx| {
                this.lsp_signature_help(cx)
            }))
            .on_action(cx.listener(|this, _: &ShowCompletion, _, cx| {
                this.lsp_completion(cx)
            }))
            .on_action(cx.listener(|this, _: &PaletteSelectUp, _, cx| {
                if this.completion_open {
                    this.completion_move(-1, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PaletteSelectDown, _, cx| {
                if this.completion_open {
                    this.completion_move(1, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PaletteConfirm, _, cx| {
                if this.completion_open {
                    this.completion_confirm(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &EscapeModal, _, cx| {
                if this.completion_open {
                    this.completion_cancel(cx);
                }
            }))
            .child(
                div()
                    .absolute()
                    .top(px(-180.))
                    .right(px(-120.))
                    .size(px(520.))
                    .rounded_full()
                    .bg(theme::alpha(theme::ACCENT, 0.045)),
            )
            .child(
                div()
                    .absolute()
                    .bottom(px(-240.))
                    .left(px(-140.))
                    .size(px(520.))
                    .rounded_full()
                    .bg(theme::alpha(theme::JADE, 0.035)),
            )
            .child(self.render_topbar(cx))
            .child(if self.project.is_some() {
                self.render_project(cx).into_any_element()
            } else {
                self.render_welcome(cx).into_any_element()
            })
            .child(self.render_statusbar(cx));

        if let Some(wizard) = &self.wizard {
            root = root.child(wizard.render(cx));
        }

        if let Some(menu) = self.context_menu.clone() {
            let exists = self.breakpoints.find(&menu.path, menu.line).is_some();
            let enabled = self
                .breakpoints
                .find(&menu.path, menu.line)
                .map(|breakpoint| breakpoint.enabled)
                .unwrap_or(true);
            let mut items: Vec<gpui::AnyElement> = Vec::new();
            let menu_path = menu.path.clone();
            let menu_line = menu.line;
            let item = |id: &'static str, label: &'static str, danger: bool| {
                div()
                    .id(id)
                    .px(px(10.))
                    .py(px(7.))
                    .rounded(px(6.))
                    .text_size(px(11.))
                    .text_color(if danger {
                        rgb(theme::CINNABAR)
                    } else {
                        rgb(theme::TEXT)
                    })
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.12)))
                    .child(label)
            };

            if !exists {
                let path = menu_path.clone();
                items.push(
                    item("ctx-add-breakpoint", "添加断点", false)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.add_breakpoint(&path, menu_line, cx);
                        }))
                        .into_any_element(),
                );
            } else {
                let path = menu_path.clone();
                items.push(
                    item(
                        "ctx-toggle-breakpoint",
                        if enabled { "禁用断点" } else { "启用断点" },
                        false,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.context_menu = None;
                        this.toggle_breakpoint_enabled(&path, menu_line, cx);
                    }))
                    .into_any_element(),
                );
                let path = menu_path.clone();
                items.push(
                    item("ctx-edit-condition", "编辑条件…", false)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_condition_dialog(&path, menu_line, cx);
                        }))
                        .into_any_element(),
                );
                let hardware = self
                    .breakpoints
                    .find(&menu_path, menu_line)
                    .map(|breakpoint| breakpoint.kind == ns_project::BreakpointKind::Hardware)
                    .unwrap_or(false);
                let path = menu_path.clone();
                items.push(
                    item(
                        "ctx-toggle-hardware",
                        if hardware {
                            "改用软件断点"
                        } else {
                            "改用硬件断点"
                        },
                        false,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.context_menu = None;
                        this.toggle_breakpoint_hardware(&path, menu_line, cx);
                    }))
                    .into_any_element(),
                );
                let path = menu_path.clone();
                items.push(
                    item("ctx-remove-breakpoint", "删除断点", true)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.context_menu = None;
                            this.remove_breakpoint(&path, menu_line, cx);
                        }))
                        .into_any_element(),
                );
            }
            let watch_word = menu.word.clone();
            let path = menu_path.clone();
            items.push(
                item("ctx-add-watchpoint", "添加观察点（数据断点）…", false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let initial = if watch_word.is_empty() {
                            this.active_editor()
                                .map(|editor| editor.read(cx).word_under_cursor())
                                .unwrap_or_default()
                        } else {
                            watch_word.clone()
                        };
                        this.open_watchpoint_dialog("", &initial, cx);
                    }))
                    .into_any_element(),
            );
            items.push(
                item("ctx-clear-file", "清除本文件断点", true)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.clear_file_breakpoints(&path, cx);
                    }))
                    .into_any_element(),
            );

            root = root.child(
                div()
                    .id("breakpoint-menu-backdrop")
                    .absolute()
                    .inset_0()
                    .on_mouse_down(MouseButton::Left, cx.listener(
                        |this, _: &MouseDownEvent, _, cx| {
                            this.context_menu = None;
                            cx.notify();
                        },
                    ))
                    .on_mouse_down(MouseButton::Right, cx.listener(
                        |this, _: &MouseDownEvent, _, cx| {
                            this.context_menu = None;
                            cx.notify();
                        },
                    ))
                    .child(
                        div()
                            .absolute()
                            .left(menu.x)
                            .top(menu.y)
                            .w(px(220.))
                            .rounded(px(10.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(rgb(theme::PANEL_SOLID))
                            .shadow(vec![gpui::BoxShadow {
                                color: gpui::hsla(0., 0., 0., 0.5),
                                offset: gpui::point(px(0.), px(4.)),
                                blur_radius: px(16.),
                                spread_radius: px(0.),
                            }])
                            .p(px(5.))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .children(items),
                    ),
            );
        }

        if let Some(dialog) = &self.condition_dialog {
            let path = dialog.path.clone();
            let line = dialog.line;
            let field = dialog.field.clone();
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme::color(0x000000).opacity(0.45))
                    .child(
                        div()
                            .w(px(460.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(rgb(theme::PANEL_SOLID))
                            .p(px(18.))
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .text_size(px(14.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(theme::TEXT))
                                            .child("编辑条件断点"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(rgb(theme::TEXT_FAINT))
                                            .child(format!(
                                                "{path}:{line} · 表达式为真时暂停；留空则恢复为普通行断点"
                                            )),
                                    ),
                            )
                            .child(div().child(field))
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(8.))
                                    .child(widgets::ghost_button("cond-cancel", "取消").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.cancel_condition_dialog(cx)
                                        }),
                                    ))
                                    .child(widgets::primary_button("cond-save", "保存").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.save_condition_dialog(cx)
                                        }),
                                    )),
                            ),
                    ),
            );
        }

        if let Some(dialog) = &self.watchpoint_dialog {
            let field = dialog.field.clone();
            let access = dialog.access;
            let editing = !dialog.original.is_empty();
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme::color(0x000000).opacity(0.45))
                    .child(
                        div()
                            .w(px(460.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(rgb(theme::PANEL_SOLID))
                            .p(px(18.))
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .text_size(px(14.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(theme::TEXT))
                                            .child(if editing {
                                                "编辑观察点（数据断点）"
                                            } else {
                                                "添加观察点（数据断点）"
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(rgb(theme::TEXT_FAINT))
                                            .child("监视表达式，例如 marker 或 obj.field；调试器在表达式被读取/写入时暂停。"),
                                    ),
                            )
                            .child(div().child(field))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .text_size(px(10.))
                                            .text_color(rgb(theme::TEXT_FAINT))
                                            .child("访问类型"),
                                    )
                                    .child(
                                        div()
                                            .id("wp-dialog-access")
                                            .px(px(12.))
                                            .py(px(4.))
                                            .rounded_full()
                                            .border_1()
                                            .border_color(theme::alpha(theme::ACCENT, 0.45))
                                            .bg(theme::alpha(theme::ACCENT, 0.1))
                                            .text_size(px(10.))
                                            .text_color(rgb(theme::ACCENT_3))
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.cycle_watchpoint_access(cx);
                                            }))
                                            .child(access.label()),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(8.))
                                    .child(widgets::ghost_button("wp-cancel", "取消").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.cancel_watchpoint_dialog(cx)
                                        }),
                                    ))
                                    .child(
                                        widgets::primary_button("wp-save", "保存").on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.save_watchpoint_dialog(cx)
                                            }),
                                        ),
                                    ),
                            ),
                    ),
            );
        }

        if let Some(dialog) = &self.set_var_dialog {
            let field = dialog.field.clone();
            let name = dialog.name.clone();
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme::color(0x000000).opacity(0.45))
                    .child(
                        div()
                            .w(px(460.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(rgb(theme::PANEL_SOLID))
                            .p(px(18.))
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .text_size(px(14.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(theme::TEXT))
                                            .child("修改变量值"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(rgb(theme::TEXT_FAINT))
                                            .child(format!("变量 `{name}` 的新值")),
                                    ),
                            )
                            .child(div().child(field))
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(8.))
                                    .child(widgets::ghost_button("setvar-cancel", "取消").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.cancel_set_var_dialog(cx)
                                        }),
                                    ))
                                    .child(widgets::primary_button("setvar-save", "设置").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.save_set_var_dialog(cx)
                                        }),
                                    )),
                            ),
                    ),
            );
        }

        if let Some(dialog) = &self.settings_dialog {
            let name = dialog.name.clone();
            let version = dialog.version.clone();
            let language = dialog.language.clone();
            let target = dialog.target.clone();
            let project_compiler = dialog.project_compiler.clone();
            let project_simulator = dialog.project_simulator.clone();
            let project_debugger = dialog.project_debugger.clone();
            let project_flasher = dialog.project_flasher.clone();
            let compiler = dialog.compiler.clone();
            let compiler_std = dialog.compiler_std.clone();
            let compiler_opt = dialog.compiler_opt.clone();
            let compiler_defines = dialog.compiler_defines.clone();
            let compiler_includes = dialog.compiler_includes.clone();
            let build_config = dialog.build_config.clone();
            let out_dir = dialog.out_dir.clone();
            let debugger = dialog.debugger.clone();
            let debugger_interface = dialog.debugger_interface.clone();
            let debugger_server = dialog.debugger_server.clone();
            let debugger_probe = dialog.debugger_probe.clone();
            let debugger_gdb = dialog.debugger_gdb.clone();
            let flasher = dialog.flasher.clone();
            let flasher_kind = dialog.flasher_kind.clone();
            let flasher_interface = dialog.flasher_interface.clone();
            let sim_cpu = dialog.sim_cpu.clone();
            let sim_period = dialog.sim_period.clone();
            let terminal_shell = dialog.terminal_shell.clone();
            let config_keys = dialog.config_keys.clone();
            let row = |label: &'static str, field: Entity<TextField>| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .w(px(110.))
                            .text_size(px(11.))
                            .text_color(rgb(theme::TEXT_FAINT))
                            .child(label),
                    )
                    .child(div().flex_1().child(field))
                    .into_any_element()
            };
            let section = |title: &'static str| {
                div()
                    .mt(px(6.))
                    .pt(px(8.))
                    .border_t_1()
                    .border_color(theme::alpha(theme::STROKE, 0.1))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme::ACCENT))
                    .child(title)
                    .into_any_element()
            };
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme::color(0x000000).opacity(0.45))
                    .child(
                        div()
                            .w(px(640.))
                            .h(px(560.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(rgb(theme::PANEL_SOLID))
                            .p(px(18.))
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .text_size(px(14.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(theme::TEXT))
                                            .child("项目设置"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(rgb(theme::TEXT_FAINT))
                                            .child("编辑 project.toml 全部字段与全局设置；保存后立即写入配置文件。"),
                                    ),
                            )
                            .child(
                                div()
                                    .id("settings-scroll")
                                    .flex_1()
                                    .overflow_y_scroll()
                                    .flex()
                                    .flex_col()
                                    .gap(px(10.))
                                    .child(section("项目 [project]"))
                                    .child(row("名称", name))
                                    .child(row("版本", version))
                                    .child(row("语言", language))
                                    .child(row("目标 MCU", target))
                                    .child(row("编译器", project_compiler))
                                    .child(row("仿真器", project_simulator))
                                    .child(row("调试器", project_debugger))
                                    .child(row("烧写器", project_flasher))
                                    .child(section("编译器 [compiler]"))
                                    .child(row("编译器路径", compiler))
                                    .child(row("标准", compiler_std))
                                    .child(row("优化", compiler_opt))
                                    .child(row("宏定义", compiler_defines))
                                    .child(row("头文件目录", compiler_includes))
                                    .child(section("构建 [build]"))
                                    .child(row("配置", build_config))
                                    .child(row("输出目录", out_dir))
                                    .child(section("调试器 [debugger]"))
                                    .child(row("调试器路径", debugger))
                                    .child(row("接口", debugger_interface))
                                    .child(row("服务器/探针", debugger_server))
                                    .child(row("探针类型", debugger_probe))
                                    .child(row("GDB 客户端", debugger_gdb))
                                    .child(section("烧写器 [flasher]"))
                                    .child(row("烧写器路径", flasher))
                                    .child(row("类型", flasher_kind))
                                    .child(row("接口", flasher_interface))
                                    .child(section("仿真 [sim]"))
                                    .child(row("CPU", sim_cpu))
                                    .child(row("周期", sim_period))
                                    .child(section("全局设置（跨项目）"))
                                    .child(row("终端 shell", terminal_shell))
                                    .child(
                                        div()
                                            .text_size(px(9.))
                                            .text_color(rgb(theme::TEXT_FAINT))
                                            .child("留空=平台默认（Windows: cmd，Linux/macOS: bash），保存到 terminal.json。"),
                                    )
                                    .child(row("全局配置键", config_keys))
                                    .child(
                                        div()
                                            .text_size(px(9.))
                                            .text_color(rgb(theme::TEXT_FAINT))
                                            .child("每行 key=value，供 $config[\"key\"] 在路径/命令中引用，保存到 config.json。"),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(8.))
                                    .child(
                                        widgets::ghost_button("settings-cancel", "取消").on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.cancel_settings(cx)
                                            }),
                                        ),
                                    )
                                    .child(
                                        widgets::primary_button("settings-save", "保存").on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.save_settings(cx)
                                            }),
                                        ),
                                    ),
                            ),
                    ),
            );
        }

        if let Some(dialog) = &self.toolchain_dialog {
            let fields = dialog.fields.clone();
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme::color(0x000000).opacity(0.45))
                    .child(
                        div()
                            .w(px(560.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(rgb(theme::PANEL_SOLID))
                            .p(px(18.))
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .text_size(px(14.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(theme::TEXT))
                                            .child("全局工具链路径"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(rgb(theme::TEXT_FAINT))
                                            .child("覆盖自动发现；留空 = 从 PATH 探测。保存在用户配置目录，跨项目共享。"),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(10.))
                                    .children(fields.into_iter().map(|(name, field)| {
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(10.))
                                            .child(
                                                div()
                                                    .w(px(150.))
                                                    .text_size(px(11.))
                                                    .text_color(rgb(theme::TEXT_FAINT))
                                                    .child(name),
                                            )
                                            .child(div().flex_1().child(field))
                                    })),
                            )
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(8.))
                                    .child(
                                        widgets::ghost_button("tc-cancel", "取消").on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.cancel_toolchain(cx)
                                            }),
                                        ),
                                    )
                                    .child(
                                        widgets::primary_button("tc-save", "保存").on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.save_toolchain(cx)
                                            }),
                                        ),
                                    ),
                            ),
                    ),
            );
        }

        if self.search.is_some() {
            let (field, matches, selected, query) = {
                let search = self.search.as_ref().unwrap();
                (
                    search.field.clone(),
                    search.matches.clone(),
                    search.selected,
                    search.field.read(cx).text(),
                )
            };
            let match_count = matches.len();
            let rows: Vec<gpui::AnyElement> = matches
                .iter()
                .enumerate()
                .map(|(index, found)| {
                    let active = index == selected;
                    let tag = match found.origin {
                        MatchOrigin::FileName => "文件".to_string(),
                        MatchOrigin::Content => format!("行 {}", found.line),
                    };
                    div()
                        .id(("search-result", index))
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .cursor_pointer()
                        .bg(if active {
                            theme::alpha(theme::ACCENT, 0.16)
                        } else {
                            gpui::transparent_black()
                        })
                        .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.07)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_search_result(index, cx)
                        }))
                        .child(
                            div()
                                .w(px(48.))
                                .flex_none()
                                .text_size(px(9.))
                                .text_color(if active {
                                    rgb(theme::ACCENT_3)
                                } else {
                                    rgb(theme::TEXT_FAINT)
                                })
                                .child(tag),
                        )
                        .child(
                            div()
                                .w(px(230.))
                                .flex_none()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .font_family(theme::MONO_FONT)
                                .text_size(px(10.5))
                                .text_color(if active {
                                    rgb(theme::TEXT)
                                } else {
                                    rgb(theme::TEXT_DIM)
                                })
                                .child(found.path.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .font_family(theme::MONO_FONT)
                                .text_size(px(10.5))
                                .text_color(rgb(theme::TEXT_FAINT))
                                .child(found.line_text.clone()),
                        )
                        .into_any_element()
                })
                .collect();

            root = root.child(
                div()
                    .id("search-backdrop")
                    .absolute()
                    .inset_0()
                    .flex()
                    .justify_center()
                    .pt(px(90.))
                    .bg(theme::color(0x000000).opacity(0.45))
                    .on_mouse_down(MouseButton::Left, cx.listener(
                        |this, _: &MouseDownEvent, _, cx| {
                            this.close_search(cx);
                        },
                    ))
                    .child(
                        div()
                            .id("search-panel")
                            .w(px(640.))
                            .h(px(420.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(rgb(theme::PANEL_SOLID))
                            .shadow(vec![gpui::BoxShadow {
                                color: gpui::hsla(0., 0., 0., 0.5),
                                offset: gpui::point(px(0.), px(5.)),
                                blur_radius: px(18.),
                                spread_radius: px(0.),
                            }])
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .on_mouse_down(MouseButton::Left, cx.listener(
                                |this, _: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.context_menu = None;
                                    cx.notify();
                                },
                            ))
                            .child(
                                div()
                                    .px(px(12.))
                                    .pt(px(12.))
                                    .pb(px(8.))
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .child(
                                        div()
                                            .w(px(18.))
                                            .flex_none()
                                            .text_size(px(12.))
                                            .text_color(rgb(theme::ACCENT_3))
                                            .child("搜"),
                                    )
                                    .child(div().flex_1().child(field))
                                    .child(
                                        div()
                                            .text_size(px(9.))
                                            .text_color(rgb(theme::TEXT_FAINT))
                                            .child(match match_count {
                                                0 if query.trim().is_empty() => String::new(),
                                                0 => "无结果".to_string(),
                                                n => format!("{n} 个结果"),
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .h(px(1.))
                                    .bg(theme::alpha(theme::STROKE, 0.12)),
                            )
                            .child(
                                div()
                                    .id("search-results")
                                    .flex_1()
                                    .overflow_y_scroll()
                                    .p(px(8.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .when(match_count == 0, |style| {
                                        style.child(
                                            div()
                                                .p(px(14.))
                                                .text_size(px(11.))
                                                .text_color(rgb(theme::TEXT_FAINT))
                                                .child(if query.trim().is_empty() {
                                                    "输入关键词搜索项目内的文件名与文件内容。"
                                                } else {
                                                    "没有找到匹配的结果。"
                                                }),
                                        )
                                    })
                                    .children(rows),
                            )
                            .child(
                                div()
                                    .px(px(12.))
                                    .py(px(8.))
                                    .border_t_1()
                                    .border_color(theme::alpha(theme::STROKE, 0.12))
                                    .text_size(px(9.))
                                    .text_color(rgb(theme::TEXT_FAINT))
                                    .child("↑↓ 选择 · Enter 跳转 · Esc 关闭"),
                            ),
                    ),
            );
        }

        if self.palette.is_some() {
            let (field, ranked, selected, _query) = {
                let palette = self.palette.as_ref().unwrap();
                (
                    palette.field.clone(),
                    palette.ranked.clone(),
                    palette.selected,
                    palette.field.read(cx).text(),
                )
            };
            let command: CommandEntry = palette::COMMANDS[ranked.get(selected).copied().unwrap_or(0)];
            let rows: Vec<gpui::AnyElement> = ranked
                .iter()
                .enumerate()
                .map(|(index, &command_index)| {
                    let entry = &palette::COMMANDS[command_index];
                    let active = index == selected;
                    div()
                        .id(("palette-row", index))
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(12.))
                        .cursor_pointer()
                        .bg(if active {
                            theme::alpha(theme::ACCENT, 0.16)
                        } else {
                            gpui::transparent_black()
                        })
                        .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.07)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.close_palette(cx);
                            this.execute_palette_command(entry.id, window, cx)
                        }))
                        .child(
                            div()
                                .font_family(theme::UI_FONT)
                                .text_size(px(11.))
                                .text_color(if active {
                                    rgb(theme::TEXT)
                                } else {
                                    rgb(theme::TEXT_DIM)
                                })
                                .child(entry.title),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_family(theme::MONO_FONT)
                                .text_size(px(9.))
                                .text_color(rgb(theme::TEXT_FAINT))
                                .child(entry.keys),
                        )
                        .into_any_element()
                })
                .collect();
            let _ = command;

            root = root.child(
                div()
                    .id("palette-backdrop")
                    .absolute()
                    .inset_0()
                    .flex()
                    .justify_center()
                    .pt(px(90.))
                    .bg(theme::color(0x000000).opacity(0.45))
                    .on_mouse_down(MouseButton::Left, cx.listener(
                        |this, _: &MouseDownEvent, _, cx| {
                            this.close_palette(cx);
                        },
                    ))
                    .child(
                        div()
                            .id("palette-panel")
                            .w(px(540.))
                            .max_h(px(430.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(rgb(theme::PANEL_SOLID))
                            .shadow(vec![gpui::BoxShadow {
                                color: gpui::hsla(0., 0., 0., 0.5),
                                offset: gpui::point(px(0.), px(5.)),
                                blur_radius: px(18.),
                                spread_radius: px(0.),
                            }])
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .on_mouse_down(MouseButton::Left, cx.listener(
                                |this, _: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.context_menu = None;
                                    cx.notify();
                                },
                            ))
                            .child(
                                div()
                                    .px(px(12.))
                                    .pt(px(12.))
                                    .pb(px(8.))
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .child(
                                        div()
                                            .w(px(18.))
                                            .flex_none()
                                            .text_size(px(12.))
                                            .text_color(rgb(theme::ACCENT_3))
                                            .child("命"),
                                    )
                                    .child(div().flex_1().child(field))
                                    .child(
                                        div()
                                            .text_size(px(9.))
                                            .text_color(rgb(theme::TEXT_FAINT))
                                            .child(format!("{} 条命令", rows.len())),
                                    ),
                            )
                            .child(
                                div().h(px(1.)).bg(theme::alpha(theme::STROKE, 0.12)),
                            )
                            .child(
                                div()
                                    .id("palette-results")
                                    .flex_1()
                                    .overflow_y_scroll()
                                    .p(px(8.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .when(rows.is_empty(), |style| {
                                        style.child(
                                            div()
                                                .p(px(14.))
                                                .text_size(px(11.))
                                                .text_color(rgb(theme::TEXT_FAINT))
                                                .child("没有匹配的命令。"),
                                        )
                                    })
                                    .children(rows),
                            )
                            .child(
                                div()
                                    .px(px(12.))
                                    .py(px(8.))
                                    .border_t_1()
                                    .border_color(theme::alpha(theme::STROKE, 0.12))
                                    .text_size(px(9.))
                                    .text_color(rgb(theme::TEXT_FAINT))
                                    .child("↑↓ 选择 · Enter 执行 · Esc 关闭"),
                            ),
                    ),
            );
        }
        root
    }
}

fn collect_tree(
    dir: &Path,
    depth: usize,
    expanded: &HashSet<PathBuf>,
    entries: &mut Vec<TreeEntry>,
) {
    if depth > 8 || entries.len() > 500 {
        return;
    }
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = read_dir
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            if path.is_dir() {
                !matches!(name.as_str(), ".git" | "build" | "target" | "node_modules" | ".idea")
                    && !name.starts_with('.')
            } else {
                name != ".DS_Store"
            }
        })
        .collect();

    children.sort_by(|a, b| {
        let a_dir = a.is_dir();
        let b_dir = b.is_dir();
        b_dir.cmp(&a_dir).then_with(|| {
            a.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase()
                .cmp(&b.file_name().unwrap_or_default().to_string_lossy().to_lowercase())
        })
    });

    for path in children {
        let is_dir = path.is_dir();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let is_expanded = expanded.contains(&path);
        entries.push(TreeEntry {
            path: path.clone(),
            name,
            is_dir,
            depth,
            expanded: is_expanded,
        });
        if is_dir && is_expanded {
            collect_tree(&path, depth + 1, expanded, entries);
        }
    }
}

fn output_tab_button(
    id: impl Into<ElementId>,
    label: &str,
    active: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(24.))
        .px(px(10.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .bg(if active {
            theme::alpha(theme::ACCENT, 0.12)
        } else {
            theme::alpha(0xffffff, 0.0)
        })
        .text_size(px(10.5))
        .text_color(if active {
            rgb(theme::TEXT)
        } else {
            rgb(theme::TEXT_FAINT)
        })
        .cursor_pointer()
        .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.08)))
        .child(label.to_string())
}

fn status_item(label: &str, value: String) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(5.))
        .child(
            div()
                .text_size(px(9.5))
                .text_color(rgb(theme::TEXT_FAINT))
                .child(label.to_string()),
        )
        .child(
            div()
                .text_size(px(10.))
                .text_color(rgb(theme::TEXT_DIM))
                .child(value),
        )
}

fn feature_note(title: &str, description: &str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(theme::TEXT_DIM))
                .child(title.to_string()),
        )
        .child(
            div()
                .text_size(px(9.5))
                .text_color(rgb(theme::TEXT_FAINT))
                .child(description.to_string()),
        )
}

fn view_tab_pill(id: &'static str, label: &'static str, active: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px(px(8.))
        .py(px(3.))
        .rounded(px(9.))
        .text_size(px(9.))
        .cursor_pointer()
        .bg(if active {
            theme::alpha(theme::ACCENT, 0.18)
        } else {
            gpui::transparent_black()
        })
        .text_color(if active {
            rgb(theme::TEXT)
        } else {
            rgb(theme::TEXT_FAINT)
        })
        .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.09)))
        .child(label)
}

fn memory_size_pill(id: (&'static str, usize), label: &'static str, active: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px(px(8.))
        .py(px(3.))
        .rounded(px(9.))
        .text_size(px(9.))
        .cursor_pointer()
        .bg(if active {
            theme::alpha(theme::ACCENT, 0.18)
        } else {
            gpui::transparent_black()
        })
        .text_color(if active {
            rgb(theme::TEXT)
        } else {
            rgb(theme::TEXT_FAINT)
        })
        .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.09)))
        .child(label)
}

fn parse_hex_address(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u64::from_str_radix(hex, 16).ok()
}

/// Parse a single `textDocument/definition` response into a location.
fn parse_definition_location(response: &serde_json::Value) -> Option<ns_lsp::Location> {
    let result = response.get("result")?;
    let location = result.as_object()?;
    let uri = location.get("uri")?.as_str()?;
    let path = uri.strip_prefix("file://").map(std::path::PathBuf::from)?;
    let range = location.get("range")?;
    let start = range.get("start")?;
    let line = start.get("line")?.as_u64()? as usize;
    let column = start.get("character")?.as_u64()? as usize;
    Some(ns_lsp::Location {
        path,
        line: line + 1,
        column: column + 1,
    })
}

/// Parse a `textDocument/references` response into a list of locations.
fn parse_location_list(response: &serde_json::Value) -> Vec<ns_lsp::Location> {
    let result = response.get("result");
    let Some(items) = result.and_then(|result| result.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let uri = item.get("uri")?.as_str()?;
            let path = uri.strip_prefix("file://").map(std::path::PathBuf::from)?;
            let range = item.get("range")?;
            let start = range.get("start")?;
            let line = start.get("line")?.as_u64()? as usize;
            let column = start.get("character")?.as_u64()? as usize;
            Some(ns_lsp::Location {
                path,
                line: line + 1,
                column: column + 1,
            })
        })
        .collect()
}

/// Extract the hover text from a `textDocument/hover` response.
fn parse_hover_text(response: &serde_json::Value) -> String {
    let result = response.get("result");
    let Some(contents) = result.and_then(|result| result.get("contents")) else {
        return String::new();
    };
    if let Some(value) = contents.get("value").and_then(|value| value.as_str()) {
        return value.to_string();
    }
    if let Some(value) = contents.as_str() {
        return value.to_string();
    }
    if let Some(array) = contents.as_array() {
        return array
            .iter()
            .filter_map(|item| item.get("value").and_then(|value| value.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}

/// Parse a `textDocument/completion` response into completion items.
fn parse_completion_items(response: &serde_json::Value) -> Vec<ns_lsp::CompletionItem> {
    let result = response.get("result");
    let Some(items) = result.and_then(|result| {
        result
            .get("items")
            .and_then(|items| items.as_array())
            .or_else(|| result.as_array())
    }) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let label = item.get("label")?.as_str()?.to_string();
            let detail = item.get("detail").and_then(|value| value.as_str()).unwrap_or("").to_string();
            let kind = item
                .get("kind")
                .and_then(|value| value.as_i64())
                .map(|kind| completion_kind(kind))
                .unwrap_or_default();
            Some(ns_lsp::CompletionItem { label, detail, kind })
        })
        .collect()
}

/// Map an LSP completion item kind code to a human-readable label.
fn completion_kind(kind: i64) -> String {
    let label = match kind {
        1 => "Text",
        2 => "Method",
        3 => "Function",
        4 => "Constructor",
        5 => "Field",
        6 => "Variable",
        7 => "Class",
        8 => "Interface",
        9 => "Module",
        10 => "Property",
        11 => "Unit",
        12 => "Value",
        13 => "Enum",
        14 => "Keyword",
        15 => "Snippet",
        16 => "Color",
        17 => "File",
        18 => "Reference",
        19 => "Folder",
        20 => "EnumMember",
        21 => "Constant",
        22 => "Struct",
        23 => "Event",
        24 => "Operator",
        25 => "TypeParameter",
        _ => "Text",
    };
    label.to_string()
}

/// Signature help shown at the cursor (LSP `textDocument/signatureHelp`).
struct SignatureHelpInfo {
    /// Human-readable signature line(s).
    label: String,
    /// 0-based index of the active parameter, if the server reports one.
    active_parameter: Option<usize>,
}

/// A single decoded semantic token (LSP `semanticTokens/full`).
#[derive(Clone)]
struct SemanticToken {
    /// 0-based line.
    line: usize,
    /// 0-based start character.
    start: usize,
    /// Length in UTF-16 code units.
    length: usize,
    /// Semantic token type index.
    token_type: usize,
}

/// Parse a `textDocument/signatureHelp` response.
fn parse_signature_help(response: &serde_json::Value) -> Option<SignatureHelpInfo> {
    let result = response.get("result")?;
    if result.is_null() {
        return None;
    }
    let signatures = result.get("signatures")?.as_array()?;
    let signature = signatures.first()?;
    let label = signature
        .get("label")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if label.is_empty() {
        return None;
    }
    let active_parameter = result
        .get("activeParameter")
        .and_then(|value| value.as_u64())
        .map(|index| index as usize);
    Some(SignatureHelpInfo {
        label,
        active_parameter,
    })
}

/// Parse a `textDocument/semanticTokens/full` response into absolute tokens.
fn parse_semantic_tokens(response: &serde_json::Value) -> Vec<SemanticToken> {
    let result = response.get("result");
    let Some(data) = result.and_then(|result| result.get("data")).and_then(|data| data.as_array()) else {
        return Vec::new();
    };
    let mut tokens = Vec::new();
    let mut line = 0usize;
    let mut start = 0usize;
    let mut chunk = data.chunks_exact(5);
    for group in &mut chunk {
        let delta_line = group[0].as_u64().unwrap_or(0) as usize;
        let delta_start = group[1].as_u64().unwrap_or(0) as usize;
        let length = group[2].as_u64().unwrap_or(0) as usize;
        let token_type = group[3].as_u64().unwrap_or(0) as usize;
        if delta_line == 0 {
            start += delta_start;
        } else {
            line += delta_line;
            start = delta_start;
        }
        tokens.push(SemanticToken {
            line,
            start,
            length,
            token_type,
        });
    }
    tokens
}

/// A single decoded inlay hint (LSP `textDocument/inlayHint`).
struct InlayHintInfo {
    /// 0-based line.
    line: usize,
    /// 0-based start character.
    start: usize,
    /// Label to render.
    label: String,
}

/// Parse a `textDocument/inlayHint` response into absolute hints.
fn parse_inlay_hints(response: &serde_json::Value) -> Vec<InlayHintInfo> {
    let Some(items) = response
        .get("result")
        .and_then(|result| result.as_array())
    else {
        return Vec::new();
    };
    let mut hints = Vec::new();
    for item in items {
        let Some(line) = item
            .pointer("/position/line")
            .and_then(|value| value.as_u64())
        else {
            continue;
        };
        let Some(start) = item
            .pointer("/position/character")
            .and_then(|value| value.as_u64())
        else {
            continue;
        };
        let label = item
            .get("label")
            .and_then(|label| label.as_str())
            .map(|label| label.to_string())
            .or_else(|| {
                item.get("label")
                    .and_then(|label| label.as_array())
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|part| part.get("value").and_then(|v| v.as_str()))
                            .collect::<Vec<_>>()
                            .join("")
                    })
            })
            .unwrap_or_default();
        if label.is_empty() {
            continue;
        }
        hints.push(InlayHintInfo {
            line: line as usize,
            start: start as usize,
            label,
        });
    }
    hints
}


/// Human-readable name for a project language.
fn language_name(language: &Language) -> &'static str {
    match language {
        Language::C => "C",
        Language::Cpp => "C++",
        Language::Rust => "Rust",
        Language::Zig => "Zig",
        Language::ZR => "ZR",
    }
}
