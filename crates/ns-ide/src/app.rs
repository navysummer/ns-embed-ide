use std::collections::HashSet;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

use futures::StreamExt;

use gpui::{
    Context, Div, ElementId, Entity, FocusHandle, Focusable, FontWeight, KeyBinding,
    MouseButton, MouseDownEvent, PathPromptOptions, Pixels, Render, Stateful, Subscription,
    Window, actions, div, prelude::*, px, rgb,
};
use ns_build::{BuildOutcome, Diagnostic, Severity};
use ns_debug::{
    self, DebugBreakpoint, DebugBreakpointLocation, DebugCommand, DebugConfig, DebugMessage,
    Frame, Scope, TraceConfig, Variable, WatchpointRequest, find_probe, package_support,
};
use ns_project::{
    BreakpointKind, BreakpointStore, Language, Project, RecentProject, WatchAccess,
    load_recents, remember_project,
};

use crate::editor::{BreakpointContextMenu, BreakpointToggled, Editor};
use crate::palette::{self, CommandEntry};
use crate::search::{self, MatchOrigin, SearchMatch};
use crate::text_field::TextField;
use crate::theme;
use crate::widgets;
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
        FindInProject,
        SearchConfirm,
        SearchSelectUp,
        SearchSelectDown,
        Palette,
        PaletteConfirm,
        PaletteSelectUp,
        PaletteSelectDown,
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
        KeyBinding::new("enter", ConditionDialogSave, Some("Dialog")),
        KeyBinding::new("f5", Debug, None),
        KeyBinding::new("shift-f5", StopDebug, None),
        KeyBinding::new("f10", DebugStepOver, None),
        KeyBinding::new("f11", DebugStepIn, None),
        KeyBinding::new("shift-f11", DebugStepOut, None),
        KeyBinding::new("cmd-shift-f", FindInProject, None),
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
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputTab {
    Output,
    Problems,
    Breakpoints,
    Debug,
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
    search: Option<SearchState>,
    palette: Option<PaletteState>,
    focus_dialog_on_next_frame: bool,
    pub wizard: Option<Wizard>,
    focus_handle: FocusHandle,
    focus_on_next_frame: bool,
    pending_run: bool,
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
            search: None,
            palette: None,
            focus_dialog_on_next_frame: false,
            wizard: None,
            focus_handle: cx.focus_handle(),
            focus_on_next_frame: true,
            pending_run: false,
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
        self.tabs.clear();
        self.active_tab = None;
        self.context_menu = None;
        self.condition_dialog = None;
        self.watchpoint_dialog = None;
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
        self.status = format!("已打开项目 {name}");

        self.open_file(entry, cx);
        self.focus_on_next_frame = true;
        cx.notify();
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
                let language = Language::from_path(&path);
                let breakpoint_key = self.breakpoint_key(&path);
                let breakpoint_lines = self.breakpoints.lines(&breakpoint_key);
                let editor = cx.new(|cx| {
                    let mut editor = Editor::new(text, language, cx);
                    editor.set_breakpoints(breakpoint_lines, cx);
                    editor
                });
                let subscription = cx.observe(&editor, |_this, _, cx| cx.notify());
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
                self.status = format!("打开 {}", self.display_path(&path));
                self.focus_on_next_frame = true;
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
        self.tabs.remove(index);
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
        cx.notify();
    }

    fn activate_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            self.active_tab = Some(index);
            self.focus_on_next_frame = true;
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
        self.pending_run = false;
        if self.pending_debug {
            self.pending_debug = false;
            if outcome.success {
                self.launch_debug(cx);
                return;
            }
            self.status = "构建失败，调试会话未启动".to_string();
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

                self.debug = DebugView {
                    status: DebugStatus::Running,
                    synced_files,
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
                self.show_output = true;
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
                let line = match type_name {
                    Some(type_name) => format!("({type_name}) {expression} = {result}"),
                    None => format!("{expression} = {result}"),
                };
                self.debug.push_console(line);
            }
            DebugMessage::Terminated => {
                self.debug.status = DebugStatus::Exited;
                self.debug.push_console("■ 调试会话已结束".to_string());
                self.debug_session = None;
                self.debug.watchpoint_resolution_pending = false;
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
        depth: usize,
        next_id: &mut usize,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let mut rows = Vec::new();
        for variable in variables {
            let row_id = *next_id;
            *next_id += 1;
            rows.push(self.variable_row(variable, depth, row_id, cx));
            let reference = variable.variables_reference;
            if reference > 0 && self.debug.expanded.contains(&reference) {
                if let Some(children) = self.debug.children.get(&reference) {
                    rows.extend(self.variable_rows(children, depth + 1, next_id, cx));
                }
            }
        }
        rows
    }

    fn variable_row(
        &self,
        variable: &Variable,
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
        if let Some(session) = self.debug_session.take() {
            let _ = session.commands.send(DebugCommand::Stop);
        }
        self.debug.status = DebugStatus::Exited;
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
            "debug" => self.start_debug(cx),
            "stop-debug" => self.stop_debug(cx),
            "find" => self.open_search(window, cx),
            "toggle-output" => {
                self.show_output = !self.show_output;
                cx.notify();
            }
            "breakpoints" => self.show_breakpoints(cx),
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
            _ => {
                self.status = format!("未知命令 {id}");
                cx.notify();
            }
        }
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
                    .child(widgets::ghost_button("top-run", "运行").on_click(
                        cx.listener(|this, _, _, cx| this.start_build(true, cx)),
                    ))
                    .child(widgets::ghost_button("top-debug", "调试").on_click(
                        cx.listener(|this, _, _, cx| this.start_debug(cx)),
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
            .child(match active {
                Some(editor) => div().flex_1().overflow_hidden().child(editor).into_any_element(),
                None => self.render_empty_editor(cx).into_any_element(),
            })
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
        let problem_count = self.diagnostics.len();
        let output_lines: Vec<(usize, String)> = self
            .output
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
                OutputTab::Problems => div()
                    .id("build-problems-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .p(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .when(self.diagnostics.is_empty(), |style| {
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
                    .into_any_element(),
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
                    .children(self.variable_rows(&self.debug.variables, 0, &mut 0, cx)),
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
                        .child(label),
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
        if self.focus_dialog_on_next_frame {
            self.focus_dialog_on_next_frame = false;
            if let Some(dialog) = &self.condition_dialog {
                window.focus(&dialog.field.read(cx).focus_handle(cx));
            } else if let Some(dialog) = &self.watchpoint_dialog {
                window.focus(&dialog.field.read(cx).focus_handle(cx));
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
            } else if self.condition_dialog.is_some() || self.watchpoint_dialog.is_some() {
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
            .on_action(cx.listener(|this, _: &QuitApp, _, cx| {
                if let Some(session) = this.debug_session.take() {
                    let _ = session.commands.send(DebugCommand::Stop);
                }
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
            .on_action(cx.listener(|this, _: &EscapeModal, _, cx| {
                if this.wizard.is_some() {
                    this.wizard_cancel(cx);
                } else if this.condition_dialog.is_some() {
                    this.cancel_condition_dialog(cx);
                } else if this.watchpoint_dialog.is_some() {
                    this.cancel_watchpoint_dialog(cx);
                } else if this.search.is_some() {
                    this.close_search(cx);
                } else if this.palette.is_some() {
                    this.close_palette(cx);
                } else if this.context_menu.is_some() {
                    this.context_menu = None;
                    cx.notify();
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
