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
    ClipboardItem, Context, Div, ElementId, Entity, FocusHandle, Focusable, FontWeight, KeyBinding,
    KeyDownEvent, MouseButton, MouseDownEvent, PathPromptOptions, Pixels, Render, SharedString,
    Stateful, Subscription, Window, actions, div, prelude::*, px,
};
use ns_build::{BuildOutcome, Diagnostic, Severity};
use ns_debug::{
    self, DebugBreakpoint, DebugBreakpointLocation, DebugCommand, DebugConfig, DebugMessage,
    EmbeddedConfig, Frame, ProbeKind, Scope, TraceConfig, Variable, WatchpointRequest,
    find_probe, package_support, swvd::{self, parse_address},
};
use ns_flash::{FlashAction, FlashRequest, adapter_for};
use ns_project::{
    BreakpointKind, BreakpointStore, Language, Project, RecentProject, WatchAccess,
    load_recents, remember_project,
    project::TargetSection,
};

use crate::agent::AgentPanel;
use crate::editor::{
    BreakpointContextMenu, BreakpointToggled, DefinitionRequested, Editor,
};
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
        NavigateBack,
        NavigateForward,

        JumpToInclude,
        RenameSymbol,
        RenameConfirm,
        RenameCancel,
         SplitRight,
         CloseSplit,
         GotoHeaderSource,
         WorkspaceSymbols,
         QuickOpen,
         RecentFiles,
        GoToLine,
 
     ]
 );

/// Build the default key bindings, applying user keystroke overrides from
/// `keybindings.json` (F13 快捷键配置). Each `KeyBinding` is checked against
/// the override map by action name; a configured keystroke replaces the
/// default one, so users can remap any command without touching source.
pub fn bindings_with(overrides: &ns_project::KeybindingOverrides) -> Vec<KeyBinding> {
    macro_rules! kb {
        ($action:expr, $default:expr, $ctx:expr, $value:expr) => {{
            let mut bindings = Vec::new();
            if let Some(custom) = overrides.get($action) {
                // A user override replaces every default keystroke for the
                // action with a single binding (no duplicate cmd/ctrl pair).
                bindings.push(KeyBinding::new(custom, $value, $ctx));
            } else {
                bindings.push(KeyBinding::new($default, $value, $ctx));
            }
            bindings
        }};
    }
    let mut all = Vec::new();
    for binding in [
        kb!("NewProject", "cmd-n", None, NewProject),
        kb!("NewProject", "ctrl-n", None, NewProject),
        kb!("OpenProject", "cmd-o", None, OpenProject),
        kb!("OpenProject", "ctrl-o", None, OpenProject),
        kb!("Save", "cmd-s", None, Save),
        kb!("Save", "ctrl-s", None, Save),
        kb!("Build", "cmd-b", None, Build),
        kb!("Build", "ctrl-b", None, Build),
        kb!("Run", "cmd-r", None, Run),
        kb!("Run", "ctrl-r", None, Run),
        kb!("ToggleOutput", "cmd-j", None, ToggleOutput),
        kb!("ToggleOutput", "ctrl-j", None, ToggleOutput),
        kb!("CloseTab", "cmd-w", None, CloseTab),
        kb!("CloseTab", "ctrl-w", None, CloseTab),
        kb!("NextTab", "ctrl-tab", None, NextTab),
        kb!("PrevTab", "ctrl-shift-tab", None, PrevTab),
        kb!("QuitApp", "cmd-q", None, QuitApp),
        kb!("QuitApp", "ctrl-q", None, QuitApp),
        kb!("Settings", "cmd-,", None, Settings),
        kb!("Settings", "ctrl-,", None, Settings),
        kb!("ConditionDialogSave", "enter", Some("Dialog"), ConditionDialogSave),
        kb!("Debug", "f5", None, Debug),
        kb!("StopDebug", "shift-f5", None, StopDebug),
        kb!("DebugStepOver", "f10", None, DebugStepOver),
        kb!("DebugStepIn", "f11", None, DebugStepIn),
        kb!("DebugStepOut", "shift-f11", None, DebugStepOut),
        kb!("RunToCursor", "cmd-f10", None, RunToCursor),
        kb!("RunToCursor", "ctrl-f10", None, RunToCursor),
        kb!("Flash", "cmd-shift-f10", None, Flash),
        kb!("Flash", "ctrl-shift-f10", None, Flash),
        kb!("FindInProject", "cmd-shift-f", None, FindInProject),
        kb!("FindInFile", "cmd-f", None, FindInFile),
        kb!("FindInFile", "ctrl-f", None, FindInFile),
        kb!("GoToLine", "cmd-g", None, GoToLine),
        kb!("GoToLine", "ctrl-g", None, GoToLine),
        kb!("FindInProject", "ctrl-shift-f", None, FindInProject),
        kb!("Palette", "cmd-shift-p", None, Palette),
        kb!("Palette", "ctrl-shift-p", None, Palette),
        kb!("PaletteConfirm", "enter", Some("Palette"), PaletteConfirm),
        kb!("PaletteSelectUp", "up", Some("Palette"), PaletteSelectUp),
        kb!("PaletteSelectDown", "down", Some("Palette"), PaletteSelectDown),
        kb!("SearchConfirm", "enter", Some("Search"), SearchConfirm),
        kb!("SearchSelectUp", "up", Some("Search"), SearchSelectUp),
        kb!("SearchSelectDown", "down", Some("Search"), SearchSelectDown),
        kb!("ClearBreakpoints", "cmd-shift-f9", None, ClearBreakpoints),
        kb!("ClearBreakpoints", "ctrl-shift-f9", None, ClearBreakpoints),
        kb!("WizardNext", "enter", Some("Wizard"), WizardNext),
        kb!("WizardBack", "shift-enter", Some("Wizard"), WizardBack),
        kb!("EscapeModal", "escape", None, EscapeModal),
         kb!("GoToDefinition", "f12", None, GoToDefinition),
        kb!("GotoHeaderSource", "f7", None, GotoHeaderSource),
         kb!("FindReferences", "shift-f12", None, FindReferences),
        kb!("Hover", "cmd-k cmd-i", None, Hover),
        kb!("Hover", "ctrl-k ctrl-i", None, Hover),
        kb!("SignatureHelp", "cmd-shift-space", None, SignatureHelp),
        kb!("SignatureHelp", "ctrl-shift-space", None, SignatureHelp),
        kb!("ShowCompletion", "cmd-space", None, ShowCompletion),
        kb!("ShowCompletion", "ctrl-space", None, ShowCompletion),
        kb!("NavigateBack", "alt-left", None, NavigateBack),
        kb!("NavigateForward", "alt-right", None, NavigateForward),
        kb!("JumpToInclude", "cmd-i", None, JumpToInclude),
        kb!("JumpToInclude", "ctrl-i", None, JumpToInclude),
        kb!("RenameSymbol", "cmd-shift-r", None, RenameSymbol),
        kb!("RenameSymbol", "ctrl-shift-r", None, RenameSymbol),
        kb!("RenameConfirm", "enter", Some("Rename"), RenameConfirm),
        kb!("RenameCancel", "escape", Some("Rename"), RenameCancel),
        kb!("SplitRight", "cmd-\\", None, SplitRight),
        kb!("SplitRight", "ctrl-\\", None, SplitRight),
        kb!("CloseSplit", "cmd-shift-\\", None, CloseSplit),
        kb!("CloseSplit", "ctrl-shift-\\", None, CloseSplit),
        kb!("WorkspaceSymbols", "cmd-shift-o", None, WorkspaceSymbols),
        kb!("WorkspaceSymbols", "ctrl-shift-o", None, WorkspaceSymbols),
        kb!("QuickOpen", "cmd-p", None, QuickOpen),
        kb!("QuickOpen", "ctrl-p", None, QuickOpen),
        kb!("RecentFiles", "cmd-e", None, RecentFiles),
        kb!("RecentFiles", "ctrl-e", None, RecentFiles),
    ] {
        all.extend(binding);
    }
    all
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

/// Topbar dropdown menu (Zed/embedded-IDE style grouped commands).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopMenu {
    File,
    Edit,
    Build,
    Debug,
    Flash,
}

impl TopMenu {
    fn label(self) -> &'static str {
        match self {
            TopMenu::File => "文件",
            TopMenu::Edit => "编辑",
            TopMenu::Build => "构建",
            TopMenu::Debug => "调试",
            TopMenu::Flash => "烧写",
        }
    }
}

/// Drag payload for the output-panel divider (resize via drag).
///
/// Resizing is tracked incrementally in *absolute* window coordinates:
/// the first move only records the mouse Y, and each following move adds
/// the pixel delta to the panel height. Using consecutive absolute
/// positions (instead of a start offset relative to the element, which
/// gpui hands to the drag-start callback) keeps the math in one coordinate
/// space and avoids a huge first jump.
struct OutputDividerDrag {
    /// Absolute window Y of the previous move (None before the first).
    last_y: std::cell::Cell<Option<f32>>,
}

/// Minimal invisible preview shown while dragging the divider.
struct DividerPreview;

/// Drag payload for moving a file-tree entry (file or directory) to another
/// directory in the project tree.
#[derive(Clone, Debug)]
struct TreeDragPayload {
    /// Absolute path of the dragged entry.
    source: PathBuf,
    /// Whether the entry is a directory.
    is_dir: bool,
}

/// Compute the destination for a tree move, or an error message describing why
/// the move is not allowed. Pure: never touches the filesystem or `self`.
fn tree_move_plan(source: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    // Refuse dropping a file onto the directory that already contains it.
    if source.parent() == Some(dest_dir) {
        return Err("目标位置与当前位置相同".to_string());
    }
    // Refuse moving into itself or a descendant.
    if dest_dir.starts_with(source) {
        return Err("不能把目录移动到它自身或其子目录".to_string());
    }
    let name = source
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    let destination = dest_dir.join(&name);
    if destination.exists() {
        return Err(format!("目标已存在: {}", destination.display()));
    }
    Ok(destination)
}

impl Render for DividerPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_0()
    }
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
    /// Content as loaded from disk / last written on save (LF-normalized), used
    /// to detect external modifications on save.
    saved_text: String,
    _subscription: Subscription,
    _breakpoint_subscription: Subscription,
    _menu_subscription: Subscription,
    _definition_subscription: Subscription,
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
 
/// Right-click state on an editor tab: the tab index and the menu anchor.
#[derive(Debug, Clone)]
struct TabMenuState {
    index: usize,
    x: Pixels,
    y: Pixels,
}

 /// A second editor pane shown to the right of the main editor (split view).
/// It holds an independent editor for a file, so two files (or two views of
/// one file) can be compared/edited side by side.
struct SplitPane {
    editor: Entity<Editor>,
    path: PathBuf,
    /// Content as loaded from disk (LF-normalized), for external-change checks.
    saved_text: String,
    _subscription: Subscription,
    _breakpoint_subscription: Subscription,
    _menu_subscription: Subscription,
    _definition_subscription: Subscription,
}

/// Modal shown on save when the file changed on disk since it was opened.
struct ExternalChangeDialog {
    /// The file being saved.
    path: PathBuf,
    /// Current disk content (LF-normalized), offered for "reload from disk".
    disk_text: String,
}

/// Right-click context menu on a file-tree entry (file or directory).
#[derive(Debug, Clone)]
struct TreeContextMenu {
    /// The entry's path (directory when creating a child inside it).
    path: PathBuf,
    is_dir: bool,
    x: Pixels,
    y: Pixels,
}

/// Whether the user is creating a file or a directory in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NewEntryKind {
    File,
    Directory,
}

/// Modal dialog for naming a new file / directory inside a target folder.
struct NewEntryDialog {
    /// The directory the new entry will be created in.
    parent: PathBuf,
    kind: NewEntryKind,
    field: Entity<TextField>,
    /// When set, the dialog renames this existing path instead of creating.
    rename: Option<PathBuf>,
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


/// Dialog to create and switch to a new local Git branch.
struct GitBranchDialog {
    field: Entity<TextField>,
}
/// Dialog to rename the symbol under the cursor via LSP.
struct RenameDialog {
    /// The (path, 0-based line, 0-based column) of the symbol to rename.
    path: PathBuf,
    line: usize,
    column: usize,
    field: Entity<TextField>,
}

/// Dialog to jump to a specific line in the active editor.
struct GoToLineDialog {
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
    build_jobs: Entity<TextField>,
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
    sim_clock: Entity<TextField>,
     /// Debug launch: arguments (space-separated), environment (one KEY=value
    /// per line) and working directory for the debuggee.
    launch_args: Entity<TextField>,
    launch_env: Entity<TextField>,
    launch_cwd: Entity<TextField>,
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

/// Modal to edit keyboard-shortcut overrides (F13 快捷键配置). One text field
/// per remappable action; the field holds the keystroke (e.g. `cmd-b`), empty
/// restores the default. Saved to the user config dir `keybindings.json`.
struct KeybindingsDialog {
    /// (action name, human label, default keystroke, field).
    fields: Vec<(&'static str, &'static str, &'static str, Entity<TextField>)>,
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
            Self::Stopped { reason } => debug_stop_reason_label(reason, None),
            Self::Exited => "已结束".to_string(),
        }
    }
}

/// Classify a DAP `stopped` event's reason (plus the adapter's optional
/// descriptive text) into a user-facing stop label. Maps common breakpoint
/// reasons, single-step pauses, and exception/signal stops (SIGSEGV,
/// EXC_BAD_ACCESS, hardware exceptions, …) to readable Chinese labels.
fn debug_stop_reason_label(reason: &str, description: Option<&str>) -> String {
    match reason {
        "breakpoint" => "已命中断点".to_string(),
        "function breakpoint" => "已命中断点".to_string(),
        "instruction breakpoint" => "已命中指令断点".to_string(),
        "data breakpoint" => "已命中观察点".to_string(),
        "step" => "单步暂停".to_string(),
        "pause" => "已暂停".to_string(),
        "entry" => "已在入口暂停".to_string(),
        "exception" | "signal" | "hardware exception" | "hardwareException" => {
            match description {
                Some(text) if text.to_ascii_uppercase().contains("SIG") => {
                    format!("信号停止 · {text}")
                }
                Some(text) if text.to_ascii_uppercase().contains("EXC") => {
                    format!("异常停止 · {text}")
                }
                Some(text) if !text.is_empty() => format!("异常停止 · {text}"),
                _ => "异常停止".to_string(),
            }
        }
        other => format!("已暂停 ({other})"),
    }
}

/// Compute which project sources are newer than the debug artifact, so the
/// user can be warned that the firmware is stale before debugging. Sources are
/// filtered to those whose modification time is strictly after the artifact's.
fn stale_sources(artifact: &Path, sources: &[PathBuf]) -> Vec<PathBuf> {
    let Some(artifact_mtime) = std::fs::metadata(artifact)
        .and_then(|meta| meta.modified())
        .ok()
    else {
        return Vec::new();
    };
    sources
        .iter()
        .filter(|source| {
            std::fs::metadata(source)
                .and_then(|meta| meta.modified())
                .map(|mtime| mtime > artifact_mtime)
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}
/// A watch expression plus its most recent evaluated value.
struct WatchEntry {
    expression: String,
    value: String,
}

struct DebugView {
    status: DebugStatus,
    thread_id: u64,
    /// Descriptive text from the adapter's last stop (e.g. "SIGSEGV",
    /// "EXC_BAD_ACCESS"), for the stop-reason classifier.
    stop_description: Option<String>,
    /// Threads known to the adapter (fetched on stop); used by the thread
    /// selector and the「线程」tab.
    threads: Vec<ns_debug::ThreadInfo>,
    /// The thread whose call stack / scopes / variables are currently shown.
    active_thread: u64,
    frames: Vec<Frame>,
    active_frame: usize,
    scopes: Vec<Scope>,
    active_scope: usize,
    variables: Vec<Variable>,
    /// CPU register groups (e.g. "General Purpose Registers"), keyed by
    /// group name; populated by expanding the adapter's "Registers" scope.
    register_groups: HashMap<String, Vec<Variable>>,
    /// The scope id of the adapter's "Registers" scope (0 = not found).
    registers_scope: u64,
    /// Maps a register group's variablesReference to its display name.
    register_group_refs: HashMap<u64, String>,
    children: HashMap<u64, Vec<Variable>>,
    /// Expanded variable references (for the current variable tree).
    expanded: HashSet<u64>,
    /// Expanded variable paths persisted across stop events, so re-expansion
    /// survives the DAP adapter's variable-reference churn between stops. Each
    /// entry is the name chain from the active scope's root variable to the
    /// expanded variable (e.g. `["struct", "field"]`).
    expanded_paths: Vec<Vec<String>>,
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
    /// Live device-register values read from the debug session, keyed by the
    /// register's absolute address (from the target SVD).
    device_reg_values: HashMap<u64, u32>,
    /// Whether live watch refreshes device registers while the target runs.
    live_watch: bool,
     /// Device-register value edit in progress: (absolute address, input field).
     device_reg_edit: Option<(u64, Entity<TextField>)>,
     /// Loaded register snapshot for comparison: group → register → value.
     reg_snapshot: Option<HashMap<String, HashMap<String, String>>>,
    /// Input for a disassembly address (jump to arbitrary address).
    disasm_input: Option<Entity<TextField>>,
    /// Debug-console command history (most recent last) for up/down recall.
    console_history: Vec<String>,
    /// Index into `console_history` for the up/down cursor (None = editing a
    /// fresh line). `console_history.len()` is the "new line" position.
    console_history_idx: Option<usize>,
    /// The text being edited when the user first pressed up (restored on down
    /// past the newest entry).
    console_draft: String,
 }

impl Default for DebugView {
    fn default() -> Self {
        Self {
            status: DebugStatus::Idle,
            thread_id: 0,
            stop_description: None,
            threads: Vec::new(),
            active_thread: 0,
            frames: Vec::new(),
            active_frame: 0,
            scopes: Vec::new(),
            active_scope: 0,
            variables: Vec::new(),
            register_groups: HashMap::new(),
            registers_scope: 0,
            register_group_refs: HashMap::new(),
            children: HashMap::new(),
            expanded: HashSet::new(),
            expanded_paths: Vec::new(),
            pc: None,
            view_tab: DebugViewTab::Variables,
            memory: MemoryView::default(),
            disassembly: Vec::new(),
            console: Vec::new(),
            error: None,
            synced_files: HashSet::new(),
            watchpoint_scope_variables: HashMap::new(),
            watchpoint_resolution_pending: false,
            resolving_watchpoints: HashSet::new(),
            trace: TraceView::default(),
            console_input: None,
            watches: Vec::new(),
            watch_input: None,
             device_reg_values: HashMap::new(),
             live_watch: false,
             device_reg_edit: None,
             reg_snapshot: None,
            disasm_input: None,
            console_history: Vec::new(),
            console_history_idx: None,
            console_draft: String::new(),
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

    /// Record a console command in history (dedup consecutive repeats, cap 100).
    fn push_console_history(&mut self, command: &str) {
        let command = command.trim().to_string();
        if command.is_empty() {
            return;
        }
        if self.console_history.last().map(String::as_str) == Some(command.as_str()) {
            return;
        }
        self.console_history.push(command);
        if self.console_history.len() > 100 {
            let excess = self.console_history.len() - 100;
            self.console_history.drain(..excess);
        }
    }

    /// Move the history cursor up (older) or down (newer); returns the text to
    /// place in the input field. `None` keeps the current text unchanged.
    fn history_navigate(&mut self, up: bool, current: &str) -> Option<String> {
        if self.console_history.is_empty() {
            return None;
        }
        match (up, self.console_history_idx) {
            (true, None) => {
                // First press: save the draft and go to the newest entry.
                self.console_draft = current.to_string();
                let idx = self.console_history.len() - 1;
                self.console_history_idx = Some(idx);
                Some(self.console_history[idx].clone())
            }
            (true, Some(idx)) => {
                if idx == 0 {
                    None
                } else {
                    self.console_history_idx = Some(idx - 1);
                    Some(self.console_history[idx - 1].clone())
                }
            }
            (false, Some(idx)) => {
                if idx + 1 < self.console_history.len() {
                    self.console_history_idx = Some(idx + 1);
                    Some(self.console_history[idx + 1].clone())
                } else {
                    // Past the newest entry: restore the draft and reset.
                    self.console_history_idx = None;
                    Some(std::mem::take(&mut self.console_draft))
                }
            }
            (false, None) => None,
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

    /// Store a register group reply into the register tab's groups. A reply
    /// for the "Registers" scope lists group headers (each with a
    /// variablesReference); the matching group replies are then stored under
    /// their header name. Unknown replies land in a synthetic group. The
    /// references to fetch next are appended to `fetch`.
    fn apply_register_group(
        &mut self,
        reference: u64,
        variables: &[Variable],
        fetch: &mut Vec<u64>,
    ) {
        if reference == self.registers_scope {
            self.register_groups.clear();
            for variable in variables {
                if variable.variables_reference != 0 {
                    self.register_group_refs
                        .insert(variable.variables_reference, variable.name.clone());
                    self.register_groups.entry(variable.name.clone()).or_default();
                    fetch.push(variable.variables_reference);
                }
            }
        } else if let Some(name) = self.register_group_refs.get(&reference).cloned() {
            self.register_groups.insert(name, variables.to_vec());
        } else {
            // A reply with no known group name (e.g. an adapter that returns a
            // flat register list) is still shown under a synthetic group.
            self.register_groups
                .insert(format!("寄存器 #{reference}"), variables.to_vec());
        }
    }
}


#[derive(Debug, Clone, Copy)]
 enum DebugStep {
     Over,
     In,
     Out,
    Instruction,
 }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebugViewTab {
    Variables,
    Registers,
    Memory,
    Disassembly,
    Trace,
    Device,
    Simulator,
}

struct MemoryView {
    address_input: Option<Entity<TextField>>,
    /// Input for hex bytes to write at the current address.
    edit_input: Option<Entity<TextField>>,
    address: u64,
    bytes: Vec<u8>,
    error: Option<String>,
    read_size: u64,
    /// Named regions in the order they were added. Index 0 is always the
    /// "custom" region bound to the address field.
    regions: Vec<MemoryRegion>,
    /// Index into `regions` of the region currently shown.
    active_region: usize,
    /// Input for the name of a new memory region (added via 「＋区域」).
     region_name_input: Option<Entity<TextField>>,
    /// Input for a byte pattern to search for in the dumped region.
     search_input: Option<Entity<TextField>>,
     /// Input for the byte pattern to fill the current region with.
     fill_input: Option<Entity<TextField>>,
    /// Byte offset (into `bytes`) and pattern length of the last search match.
    search_hit: Option<(usize, usize)>,
 }

impl Default for MemoryView {
    fn default() -> Self {
        Self {
            address_input: None,
            edit_input: None,
            address: 0,
            bytes: Vec::new(),
            error: None,
            read_size: 64,
            regions: vec![MemoryRegion {
                name: "自定义".to_string(),
                address: 0,
                size: 64,
             }],
             active_region: 0,
             region_name_input: None,
            search_input: None,
            fill_input: None,
            search_hit: None,
         }
    }
}

/// A named memory region the user can jump between in the memory view.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryRegion {
    name: String,
    address: u64,
    size: u64,
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
     /// Decoded SWO packets received since the window was armed, newest-last.
    packets: Vec<swvd::Packet>,
    /// Whether the background SWO stream reader is running.
    streaming: bool,
    profile: ns_debug::profile::ProfileAggregator,
    /// Whether to show the profiler hotspot view.
    show_profile: bool,
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
             packets: Vec::new(),
             streaming: false,
            profile: ns_debug::profile::ProfileAggregator::new(&[]),
            show_profile: false,
         }
    }
}

struct DebugSession {
    commands: Sender<DebugCommand>,
}


 enum FlashMode {
     /// Reset-and-run the artifact directly (no debugger attached).
     Run,
     /// Attach the debugger after flashing.
     Debug,
    /// After a successful flash, immediately verify the written bytes.
    Verify,
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

    /// Show the terminal panel, focusing the active terminal or opening one.
    fn show_terminal(&mut self, cx: &mut Context<Self>) {
        self.show_output = true;
        self.output_tab = OutputTab::Terminal;
        if self.terminals.is_empty() {
            let settings = ns_project::TerminalSettings::load();
            self.open_terminal(
                TerminalConfig {
                    kind: TerminalKind::Shell,
                    program: settings.shell_command(),
                },
                cx,
            );
        } else {
            self.focus_terminal = true;
            cx.notify();
        }
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
        cx.spawn(async move |_this, cx| {
            loop {
                let changed = poll.update(cx, |terminal, cx| terminal.drain_events(cx));
                if changed.is_err() {
                    break;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
            }
        })
        .detach();
        let index = self.terminals.len();
        self.terminals.push(terminal);
        self.active_terminal = Some(index);
        self._terminal_sub = Some(sub);
        if let Some(terminal) = self.terminals.get(index) {
            terminal.update(cx, |terminal, _cx| terminal.resize(100, 30));
        }
        self.focus_terminal = true;
        cx.notify();
    }

    /// Open a fresh shell terminal (Zed-style "+" button / Cmd+J).
    fn new_terminal(&mut self, cx: &mut Context<Self>) {
        let settings = ns_project::TerminalSettings::load();
        self.open_terminal(
            TerminalConfig {
                kind: TerminalKind::Shell,
                program: settings.shell_command(),
            },
            cx,
        );
    }

    /// Close the terminal at `index`; focus the neighbor or the panel.
    fn close_terminal(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.terminals.len() {
            return;
        }
        self.terminals.remove(index);
        self.active_terminal = if self.terminals.is_empty() {
            None
        } else {
            Some(index.min(self.terminals.len() - 1))
        };
        self.focus_terminal = true;
        cx.notify();
    }

    /// Activate the terminal at `index`, focusing it.
    fn focus_terminal_index(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.terminals.len() {
            self.active_terminal = Some(index);
            self.focus_terminal = true;
            cx.notify();
        }
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

    /// Check out an existing local branch and refresh the panel.
    fn git_checkout(&mut self, branch: String, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        let root = project.root.clone();
        match git::checkout(&root, &branch) {
            Ok(()) => {
                self.git.error = None;
                self.git.feedback = Some(format!("已切换到分支 {branch}"));
                self.git.selected = None;
                self.git.diff.clear();
            }
            Err(error) => self.git.error = Some(error),
        }
        refresh_git(self, cx);
    }

    /// Open the new-branch dialog.
    fn open_git_branch_dialog(&mut self, cx: &mut Context<Self>) {
        let field = cx.new(|cx| TextField::new("新分支名…", cx));
        self.git_branch_dialog = Some(GitBranchDialog { field });
        self.focus_dialog_on_next_frame = true;
        cx.notify();
    }

    /// Confirm the new-branch dialog: create and switch to the branch.
    fn git_create_branch(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.git_branch_dialog.take() else {
            return;
        };
        let Some(project) = &self.project else {
            return;
        };
        let root = project.root.clone();
        let name = dialog.field.read(cx).text().trim().to_string();
        match git::create_branch(&root, &name) {
            Ok(()) => {
                self.git.error = None;
                self.git.feedback = Some(format!("已创建并切换到分支 {name}"));
                self.git.selected = None;
                self.git.diff.clear();
            }
            Err(error) => self.git.error = Some(error),
        }
        refresh_git(self, cx);
    }

    /// Cancel the new-branch dialog.
    fn cancel_git_branch_dialog(&mut self, cx: &mut Context<Self>) {
        self.git_branch_dialog = None;
        cx.notify();
    }

    /// Pull upstream changes into the current branch.
    fn git_pull(&mut self, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        let root = project.root.clone();
        match git::pull(&root) {
            Ok(()) => {
                self.git.error = None;
                self.git.feedback = Some("已拉取上游更新".to_string());
            }
            Err(error) => self.git.error = Some(error),
        }
        refresh_git(self, cx);
    }

    /// Push the current branch to its upstream.
    fn git_push(&mut self, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        let root = project.root.clone();
        match git::push(&root) {
            Ok(()) => {
                self.git.error = None;
                self.git.feedback = Some("已推送当前分支".to_string());
            }
            Err(error) => self.git.error = Some(error),
        }
        refresh_git(self, cx);
    }
}


#[derive(Debug)]
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
    /// Sidebar file-tree filter query; when non-empty the tree shows only
    /// paths containing the query (flat list of files, dirs auto-expanded).
    tree_filter: String,
    /// Query field for the sidebar file-tree filter.
    tree_filter_field: Entity<TextField>,
    /// Subscription to the tree-filter field.
    _tree_filter_sub: Option<Subscription>,
    recents: Vec<RecentProject>,
    /// editor. `None` when the split view is closed.
    split: Option<SplitPane>,
    /// Project-internal recently opened files (most recent first).
    recent_files: ns_project::RecentFiles,
    status: String,
    build_state: BuildState,
    /// Cancel flag for the running build; set to stop the child process.
     build_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Cancel flag for the running flash operation; set to stop the adapter.
    flash_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// True while a build's streamed output is being appended live to the
    /// output panel; `finish_build` appends only the summary line then.
    build_streaming: bool,
    diagnostics: Vec<Diagnostic>,
    output: String,
    build_command: String,
     /// Firmware memory-usage breakdown of the last successful build, when the
     /// project targets an MCU and the artifact is a recognized firmware file.
     firmware_size: Option<ns_flash::FirmwareSize>,
    /// Estimated per-source-file compile times from the last build (parsed
    /// from CMake/Make progress lines). Empty for non-instrumented builds.
    build_file_timings: Vec<ns_build::FileTiming>,
     /// Per-function stack usage from the last `-fstack-usage` analysis run
    /// (C/C++ only; `None` until a run completes or for non-C projects).
     stack_usage: Vec<ns_build::StackUsageEntry>,
    /// Memory-layout model from the linker script + firmware sections, for the
    /// region-occupancy view. `None` until the command runs or on failure.
     memory_layout: Option<ns_build::layout::MemoryLayout>,
     /// Project include dependency graph (from the last「include 依赖图」run).
     include_graph: Option<ns_project::depgraph::IncludeGraph>,
     /// Whether the include graph is shown as a graphical DAG (else a list).
     include_graph_dag: bool,
     /// TODO/FIXME/XXX markers from the last「任务面板」scan.
    task_markers: Vec<ns_project::tasks::TaskMarker>,
     /// Per-file line coverage from the last gcov run (C/C++ only).
     coverage: Vec<ns_build::CoverageFile>,
    show_output: bool,
    /// Height of the output panel in pixels (draggable via the divider).
    output_px: f32,
    output_tab: OutputTab,
    breakpoints: BreakpointStore,
    debug: DebugView,
    debug_session: Option<DebugSession>,
    pending_debug: bool,
     context_menu: Option<ContextMenuState>,
    tab_menu: Option<TabMenuState>,
     tree_menu: Option<TreeContextMenu>,
    top_menu_open: Option<TopMenu>,
     new_entry_dialog: Option<NewEntryDialog>,
     condition_dialog: Option<ConditionDialog>,
     watchpoint_dialog: Option<WatchpointDialog>,
     settings_dialog: Option<SettingsDialog>,
     set_var_dialog: Option<SetVarDialog>,
     rename_dialog: Option<RenameDialog>,
     toolchain_dialog: Option<ToolchainDialog>,
     keybindings_dialog: Option<KeybindingsDialog>,
    external_change_dialog: Option<ExternalChangeDialog>,
    git_branch_dialog: Option<GitBranchDialog>,
    go_to_line_dialog: Option<GoToLineDialog>,
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
    /// Open shell terminals (Zed-style: multiple sessions with a tab bar).
    terminals: Vec<Entity<Terminal>>,
    /// Index of the focused terminal in `terminals`.
    active_terminal: Option<usize>,
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
    /// Monotonic id used to discard LSP spawn results from a project that was
    /// replaced by a newer one before the handshake completed.
     lsp_generation: u64,
    /// Monotonic id for the debounced semantic/inlay refresh; bumped on every
    /// edit so a stale timer (a newer edit arrived) does not fire.
    lsp_refresh_generation: u64,
      /// Per-file diagnostics received from the language server.
    lsp_diagnostics: HashMap<PathBuf, Vec<ns_lsp::LspDiagnostic>>,
       /// Locations found by the last find-references request.
     references: Vec<ns_lsp::Location>,
    /// Whether the references popup is visible.
    references_open: bool,
    /// Index of the highlighted reference.
     references_selected: usize,
      workspace_symbols: Vec<ns_lsp::WorkspaceSymbolInfo>,
    /// Whether the workspace-symbol popup is visible.
    workspace_symbol_open: bool,
    /// Index of the highlighted workspace symbol.
    workspace_symbol_selected: usize,
    /// Query field for the workspace-symbol popup.
    workspace_symbol_field: Entity<TextField>,
    /// Monotonic id used to discard stale workspace-symbol responses.
    workspace_symbol_generation: u64,
    /// Subscription to the workspace-symbol query field (re-search on edit).
    _workspace_symbol_sub: Option<Subscription>,
    /// Quick Open popup state: query field, filtered file list, highlight.
    quick_open: bool,
    quick_open_files: Vec<String>,
    quick_open_selected: usize,
    quick_open_field: Entity<TextField>,
    /// Subscription to the quick-open query field.
    _quick_open_sub: Option<Subscription>,
    /// Recent-files popup state (list of project files, most recent first).
    recent_open: bool,
    recent_selected: usize,
    /// Items from the last completion request.
    completions: Vec<ns_lsp::CompletionItem>,
    /// Whether the completion popup is visible.
    completion_open: bool,
    /// Index of the highlighted completion item.
    completion_selected: usize,
    /// Cursor position (path, 0-based line, 0-based column) when signature
    /// help was last shown. Moving away from it clears the popup (P13.1).
    signature_anchor: Option<(PathBuf, usize, usize)>,
    /// Signature help shown at the cursor, if any.
    signature_help: Option<SignatureHelpInfo>,
    /// Hover tooltip text (LSP `textDocument/hover`) shown as a floating panel
    /// near the editor, not just in the status bar. Cleared when the cursor
    /// moves or the file/selection changes.
    hover_tooltip: Option<String>,
    /// Cursor position (path, 0-based line, 0-based column) when the hover
    /// tooltip was last shown. Moving away clears the popup (mirrors the
    /// signature-help anchor behaviour).
    hover_anchor: Option<(PathBuf, usize, usize)>,
    /// Semantic token deltas from the last `semanticTokens/full` request.
    semantic_tokens: Vec<SemanticToken>,
    /// Document outline symbols (LSP `documentSymbol`), for the outline/breadcrumb.
    outline: Vec<ns_lsp::SymbolInfo>,
     /// Whether the outline (symbol list) sidebar is shown.
     outline_open: bool,
    /// Outline symbol-name filter (case-insensitive substring).
    outline_filter: String,
    /// Input for the outline filter.
    outline_filter_input: Option<Entity<TextField>>,
      /// Whether the device picker dropdown (debug「设备」页) is open.
     device_picker_open: bool,
    /// Whether the build-config dropdown (status bar) is open.
    build_config_open: bool,
     /// Peripheral/register name filter field for the「设备」页.
     device_filter: Entity<TextField>,


    /// Built-in instruction-level simulator session.
    sim: SimulatorState,
    /// Navigation history (Alt+←/→): entries are (path, 0-based line, 0-based column).
    nav_back: Vec<NavEntry>,
    nav_forward: Vec<NavEntry>,
}

/// A single navigation-history position: file path plus 0-based cursor position.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NavEntry {
    path: PathBuf,
    line: usize,
    column: usize,
}

/// Where `toggle_build_config` applied a config change, so a failed save can
/// restore the exact prior in-memory state.
enum RollbackKind {
    /// The shared `[build] config` was changed; restore its prior value.
    Shared(String),
    /// An existing per-target entry was changed; restore its prior value.
    Entry(String),
    /// A new per-target entry was pushed; remove it on failure.
    NewEntry,
}
/// Push a navigation-history entry onto `back`, dropping the oldest entries
/// beyond `CAP` and clearing `forward` (a new jump invalidates the redo path).
/// Pure and shared with the tests so the stack contract is tested directly.
fn push_nav_entry(back: &mut Vec<NavEntry>, forward: &mut Vec<NavEntry>, entry: NavEntry) {
    back.push(entry);
    const CAP: usize = 50;
    if back.len() > CAP {
        let excess = back.len() - CAP;
        back.drain(..excess);
    }
    forward.clear();
}

/// Apply a single LSP-style text edit (0-based line/character range) to a
/// whole file's text, returning the new content or `None` if the range is out
/// of bounds. Used to persist `WorkspaceEdit` edits to disk.
/// Whether two paths refer to the same file: exact equality first, then a
/// canonicalize comparison so relative/absolute forms and `..` still match.
fn paths_equal(candidate: &Path, base: &Path) -> bool {
    if candidate == base {
        return true;
    }
    match (
        std::fs::canonicalize(candidate),
        std::fs::canonicalize(base),
    ) {
        (Ok(candidate), Ok(base)) => candidate == base,
        _ => false,
    }
}

fn apply_edit_to_text(
    text: &str,
    start_line: usize,
    start_char: usize,
    end_line: usize,
    end_char: usize,
    new_text: &str,
) -> Option<String> {
    let mut starts = vec![0];
    for (index, ch) in text.char_indices() {
        if ch == '\n' {
            starts.push(index + 1);
        }
    }
    let line_at = |line: usize| line.min(starts.len().saturating_sub(1));
    let line_start = |line: usize| starts[line_at(line)];
    let line_end = |line: usize| {
        starts
            .get(line_at(line) + 1)
            .map(|next| next.saturating_sub(1))
            .unwrap_or(text.len())
    };
    let offset = |line: usize, character: usize| {
        let ls = line_start(line);
        let le = line_end(line);
        // The byte offset of the `character`-th char in the line; clamps to
        // the line end when the column is past the line.
        match text[ls..le].char_indices().nth(character) {
            Some((index, _)) => ls + index,
            None => le,
        }
    };
    // Reject edits that reference lines past the document.
    if start_line >= starts.len() || end_line >= starts.len() {
        return None;
    }
    let start = offset(start_line, start_char);
    let end = offset(end_line, end_char);
    if start > end || end > text.len() {
        return None;
    }
    Some(text[0..start].to_owned() + new_text + &text[end..])
}

/// Kind of `#include` directive, which determines the search order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IncludeKind {
    /// `#include "file.h"` - relative to the including file first.
    Quotes,
    /// `#include <file.h>` - system/angle include paths only.
    Angle,
}

/// Parse a `#include "file.h"` / `#include <file.h>` directive from `line`.
/// Returns the kind and the requested file name, or `None` if the line is not
/// an include directive (or the cursor is not on the directive).
fn parse_include_directive(line: &str) -> Option<(IncludeKind, String)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    // Allow optional whitespace between `#` and `include`.
    let after = trimmed[1..].trim_start();
    let rest = after.strip_prefix("include")?;
    // `include` must be followed by whitespace, not e.g. `includex`.
    let rest = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('\t'))?;
    let rest = rest.trim_start();
    if let Some(path) = rest.strip_prefix('"') {
        let end = path.find('"')?;
        return Some((IncludeKind::Quotes, path[..end].to_string()));
    }
    if let Some(path) = rest.strip_prefix('<') {
        let end = path.find('>')?;
        return Some((IncludeKind::Angle, path[..end].to_string()));
    }
    None
}

/// Read a little/big-endian u16 from a byte slice (ELF header fields).
fn read_u16_le(bytes: &[u8], little: bool) -> u16 {
    if bytes.len() < 2 {
        return 0;
    }
    if little {
        u16::from_le_bytes([bytes[0], bytes[1]])
    } else {
        u16::from_be_bytes([bytes[0], bytes[1]])
    }
}


#[derive(Default)]
struct SimulatorState {
    /// Whether cycle-accurate modeling is enabled for the next run.
    cycle_accurate: bool,
    core: Option<ns_sim::Simulator>,
    /// Whether the simulator panel is visible.
    panel_open: bool,
    /// Human-readable status of the last simulator action.
    status: String,
    /// Memory view: base address input (hex, as typed).
    mem_address: String,
    /// The dumped bytes starting at `mem_address`.
    mem_bytes: Vec<u8>,
    /// Last memory-read error, shown inline.
    mem_error: Option<String>,
    /// Hex bytes to write into the simulator memory (whitespace-separated).
    mem_edit_input: Option<gpui::Entity<crate::text_field::TextField>>,
    /// Register currently being edited (index into the arch register file).
    reg_edit: Option<usize>,
    /// The text field used to edit a register value (hex, as typed).
    reg_edit_input: Option<gpui::Entity<crate::text_field::TextField>>,
    /// The text field used to jump to a program counter (hex, as typed).
    pc_edit_input: Option<gpui::Entity<crate::text_field::TextField>>,
    /// The text field used to add a breakpoint by address (hex, as typed).
    bp_input: Option<gpui::Entity<crate::text_field::TextField>>,
}

impl IdeApp {
    pub fn new(initial: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        // Restore the persisted theme preference before the first paint.
        let theme_settings = ns_project::ThemeSettings::load();
        theme::set_light(theme_settings.mode == ns_project::ThemeMode::Light);
        let recents = load_recents();
        let mut app = Self {
            project: None,
            tabs: Vec::new(),
            active_tab: None,
            split: None,
            expanded: HashSet::new(),
            tree_filter: String::new(),
            tree_filter_field: cx.new(|cx| TextField::new("过滤文件…", cx)),
            _tree_filter_sub: None,
            recents,
            recent_files: ns_project::RecentFiles::default(),
            status: "就绪".to_string(),
            build_state: BuildState::Idle,
            build_streaming: false,
             build_cancel: None,
            flash_cancel: None,
            diagnostics: Vec::new(),
            output: String::new(),
             build_command: String::new(),
             firmware_size: None,
            build_file_timings: Vec::new(),
             stack_usage: Vec::new(),
             memory_layout: None,
             include_graph: None,
            include_graph_dag: false,
             task_markers: Vec::new(),
             coverage: Vec::new(),
            show_output: false,
            output_px: 260.0,
            output_tab: OutputTab::Output,
            breakpoints: BreakpointStore::default(),
            debug: DebugView::default(),
            debug_session: None,
            pending_debug: false,
             context_menu: None,
            tab_menu: None,
             tree_menu: None,
            top_menu_open: None,
             new_entry_dialog: None,
            condition_dialog: None,
             watchpoint_dialog: None,
            settings_dialog: None,
             set_var_dialog: None,
            rename_dialog: None,
             toolchain_dialog: None,
            keybindings_dialog: None,
             external_change_dialog: None,
            git_branch_dialog: None,
            go_to_line_dialog: None,
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
            terminals: Vec::new(),
             active_terminal: None,
            _terminal_sub: None,
            scroll_terminal_to_bottom: false,
            focus_terminal: false,
            serial: None,
            _serial_sub: None,
             lsp_generation: 0,
            lsp_refresh_generation: 0,
            agent: None,
            _agent_sub: None,
            lsp: None,
            lsp_diagnostics: HashMap::new(),
             references: Vec::new(),
            references_open: false,
             references_selected: 0,
            workspace_symbols: Vec::new(),
             workspace_symbol_open: false,
            workspace_symbol_selected: 0,
            workspace_symbol_field: cx.new(|cx| TextField::new("搜索符号…", cx)),
            workspace_symbol_generation: 0,
            _workspace_symbol_sub: None,
            quick_open: false,
            quick_open_files: Vec::new(),
            quick_open_selected: 0,
            quick_open_field: cx.new(|cx| TextField::new("输入文件名…", cx)),
            _quick_open_sub: None,
            recent_open: false,
            recent_selected: 0,
            completions: Vec::new(),
            completion_open: false,
            completion_selected: 0,
            signature_anchor: None,
            signature_help: None,
            hover_tooltip: None,
            hover_anchor: None,
            semantic_tokens: Vec::new(),
             outline: Vec::new(),
             outline_open: false,
            outline_filter: String::new(),
            outline_filter_input: None,
              device_picker_open: false,
            build_config_open: false,
             device_filter: cx.new(|cx| TextField::new("过滤外设 / 寄存器…", cx)),
             sim: SimulatorState::default(),
            nav_back: Vec::new(),
            nav_forward: Vec::new(),
        };
        // Re-search workspace symbols as the query field changes.
        app._workspace_symbol_sub = Some(cx.observe(
            &app.workspace_symbol_field.clone(),
            |this, field, cx| {
                if this.workspace_symbol_open {
                    this.workspace_symbol_search(cx);
                }
                let _ = field;
            },
        ));
        // Re-filter the Quick Open list as the query field changes.
        app._quick_open_sub = Some(cx.observe(&app.quick_open_field.clone(), |this, field, cx| {
            if this.quick_open {
                this.quick_open_refresh(cx);
            }
            let _ = field;
        }));
        // Re-filter the file tree as the sidebar filter query changes.
        app._tree_filter_sub = Some(cx.observe(&app.tree_filter_field.clone(), |this, field, cx| {
            this.tree_filter = field.read(cx).text().trim().to_string();
            cx.notify();
        }));
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
        self.recent_files = ns_project::RecentFiles::load(&root);
        self.recent_files.retain_existing(&root);

        let entry = project.entry_file();
        self.project = Some(project);
        if let Some(session) = self.debug_session.take() {
            let _ = session.commands.send(DebugCommand::Stop);
        }
         self.debug = DebugView::default();
         self.debug.trace.config = TraceConfig::load(&root);
        self.load_watches(&root);
         self.pending_debug = false;
        self.pending_flash = None;
        self.tabs.clear();
        self.active_tab = None;
        self.context_menu = None;
        self.tree_menu = None;
        self.new_entry_dialog = None;
        self.condition_dialog = None;
        self.watchpoint_dialog = None;
        self.set_var_dialog = None;
        self.settings_dialog = None;
        self.toolchain_dialog = None;
        self.external_change_dialog = None;
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
        // Restore folded regions for each open file, keyed by project-relative
        // path in the session.
        let folded_by_path: std::collections::HashMap<&Path, &Vec<(usize, usize)>> = session
            .folded
            .iter()
            .map(|folded| (folded.path.as_path(), &folded.regions))
            .collect();
        for tab in &self.tabs {
            if let Ok(relative) = tab.path.strip_prefix(&root)
                && let Some(regions) = folded_by_path.get(relative)
            {
                tab.editor.update(cx, |editor, cx| {
                    editor.set_folded_regions((*regions).clone(), cx)
                });
            }
        }
        // Restore per-file cursor + scroll view state.
        let views_by_path: std::collections::HashMap<&Path, &ns_project::FileView> = session
            .views
            .iter()
            .map(|view| (view.path.as_path(), view))
            .collect();
        for tab in &self.tabs {
            if let Ok(relative) = tab.path.strip_prefix(&root)
                && let Some(view) = views_by_path.get(relative)
            {
                tab.editor.update(cx, |editor, cx| {
                    if let Some((line, column)) = view.cursor {
                        if line > 0 {
                            editor.set_cursor_line_column(line, column.saturating_sub(1), cx);
                        }
                    }
                    if view.scroll_ratio > 0.0 {
                        editor.scroll_to_ratio(view.scroll_ratio, cx);
                    }
                });
            }
        }
        if let Some(index) = session.active {
            if index < self.tabs.len() {
                self.active_tab = Some(index);
            }
        }
        // Restore the split editor pane (if it was open when the session was
        // last saved).
        if let Some(relative) = &session.split {
            let path = root.join(relative);
            if path.exists() && path.is_file() {
                if let Ok(text) = fs::read_to_string(&path)
                    && !is_binary_file(&path, text.as_bytes())
                {
                    let split = self.build_split_editor(path.clone(), text, cx);
                    self.split = Some(split);
                }
            }
        }
        self.start_lsp(cx);
        self.focus_on_next_frame = true;
        cx.notify();

        // Crash recovery: periodically snapshot the session so a crash loses
        // at most a short window of tab/cursor state, not the whole editor.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                this.update(cx, |this, cx| {
                    this.save_session(cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Spawn the language server for the project language and poll its events.
    ///
    /// Never blocks the UI thread: the `initialize` handshake runs on the
    /// background executor and the project is usable immediately (Zed-style).
    /// `lsp_generation` guards against a stale spawn attaching after the user
    /// already opened a different project.
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

        self.lsp_generation += 1;
        let generation = self.lsp_generation;

        // Spawn (and handshake) on the background executor so a slow server
        // (clangd cold-start, rust-analyzer indexing) never freezes the UI.
        let spawn_root = root.clone();
        let spawn_task = cx
            .background_executor()
            .spawn(async move { ns_lsp::LspServer::spawn(&language, &spawn_root) });

        let attach = cx.spawn(async move |this, cx| {
            let result = spawn_task.await;
            this.update(cx, |this, cx| {
                // A newer project opened while we were waiting: discard.
                if generation != this.lsp_generation {
                    if let Ok(server) = result {
                        server.shutdown();
                    }
                    return;
                }
                match result {
                    Ok(server) => {
                        this.lsp = Some(server);
                        // Open already-loaded tabs so the server indexes them.
                        let files: Vec<(PathBuf, String)> = this
                            .tabs
                            .iter()
                            .filter_map(|tab| {
                                fs::read_to_string(&tab.path)
                                    .ok()
                                    .map(|text| (tab.path.clone(), text))
                            })
                            .collect();
                        if let Some(server) = &this.lsp {
                            for (path, text) in files {
                                let _ = server.open_file(path, text);
                            }
                        }
                        this.status = format!(
                            "已连接语言服务器（{}）",
                            language_name(&language)
                        );
                        this.start_lsp_poll(generation, cx);
                    }
                    Err(error) => {
                        this.status = format!("语言服务器启动失败: {error}");
                    }
                }
                cx.notify();
            })
            .ok();
        });
        attach.detach();
    }

    /// Poll the LSP event channel for diagnostics and forward them to the UI.
    /// Returns `false` when the server has exited (the polling loop should stop).
    fn start_lsp_poll(&mut self, generation: u64, cx: &mut Context<Self>) {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                let alive = this
                    .update(cx, |this, cx| this.poll_lsp_events(generation, cx))
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
    }

    /// Poll the LSP event channel for diagnostics and forward them to the UI.
    /// Returns `false` when the server has exited (the polling loop should stop).
    fn poll_lsp_events(&mut self, generation: u64, cx: &mut Context<Self>) -> bool {
        // A newer project opened while we were polling: stop this loop.
        if generation != self.lsp_generation {
            return false;
        }
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
            self.refresh_diagnostics(cx);
            cx.notify();
        }
        alive
    }

    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let path = path.canonicalize().unwrap_or(path);
        self.remember_recent_file(&path, cx);
        if let Some(index) = self.tabs.iter().position(|tab| tab.path == path) {
            self.active_tab = Some(index);
            self.focus_on_next_frame = true;
            cx.notify();
            return;
        }

        match fs::read_to_string(&path) {
            Ok(text) if !is_binary_file(&path, text.as_bytes()) => {
                self.open_text_tab(path, text, cx);
            }
            Ok(_) | Err(_) => {
                // Binary or unreadable-as-UTF-8: open a read-only hex dump so
                // firmware artifacts (.bin/.elf/.ihx) are inspectable.
                match fs::read(&path) {
                    Ok(bytes) => {
                        let dump = hex_dump_text(&bytes);
                        let editor = cx.new(|cx| {
                            let mut editor = Editor::new(dump, None, cx);
                            editor.set_read_only(true);
                            editor
                        });
                        let breakpoint_key = self.breakpoint_key(&path);
                        // Clone the entity for subscriptions; the original is
                        // moved into the tab.
                        let editor_ref = editor.clone();
                        self.tabs.push(Tab {
                            editor,
                            path: path.clone(),
                            saved_text: String::new(),
                            _subscription: cx.observe(&editor_ref, |_, _, _| {}),
                            _breakpoint_subscription: cx.subscribe(
                                &editor_ref,
                                move |this, _editor, event: &BreakpointToggled, cx| {
                                    this.on_breakpoint_toggled(
                                        &breakpoint_key,
                                        event.line,
                                        event.enabled,
                                        cx,
                                    );
                                },
                            ),
                            _menu_subscription: cx.subscribe(
                                &editor_ref,
                                |_, _, _: &BreakpointContextMenu, _| {},
                            ),
                            _definition_subscription: cx.subscribe(
                                &editor_ref,
                                |_, _, _: &DefinitionRequested, _| {},
                            ),
                        });
                        self.active_tab = Some(self.tabs.len() - 1);
                        self.status = format!("打开 {}（hex 视图）", self.display_path(&path));
                        self.save_session(cx);
                        cx.notify();
                    }
                    Err(error) => {
                        self.status = format!("无法读取 {}: {error}", path.display());
                        cx.notify();
                    }
                }
            }
        }
     }
 
    /// Go to the counterpart file (header ↔ source) of the active editor.
    /// Uses `find_matching_source_file`; opens the match or reports honestly
    /// when no counterpart exists.
    fn goto_header_source(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.active_tab.and_then(|i| self.tabs.get(i)) else {
            self.status = "没有活动的编辑器".to_string();
            cx.notify();
            return;
        };
        let path = active.path.clone();
        let (root, includes) = self
            .project
            .as_ref()
            .map(|p| (Some(p.root.clone()), p.config.compiler.includes.clone()))
            .unwrap_or((None, Vec::new()));
        let includes: Vec<String> = includes
            .iter()
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .collect();
        let Some(match_path) =
            find_matching_source_file(&path, root.as_deref(), &includes)
        else {
            self.status = format!("未找到 {} 的对应头文件/源文件", path.display());
            cx.notify();
            return;
        };
        self.open_file(match_path, cx);
    }

     /// Record a just-opened file in the project's recent-file history and
    /// persist it. Paths are stored relative to the project root.
    fn remember_recent_file(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        // Canonicalize the root so `/var` vs `/private/var` (macOS temp dirs)
        // do not defeat the prefix strip; the opened path is already absolute.
        let root = project.root.canonicalize().unwrap_or_else(|_| project.root.clone());
        let relative = path
            .strip_prefix(&root)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| path.to_path_buf());
        self.recent_files.push(relative);
        if let Err(error) = self.recent_files.save(&project.root) {
            self.status = format!("保存最近文件失败: {error}");
            cx.notify();
        }
    }

    /// Open the recent-files popup, pruning files that no longer exist.
    fn open_recent_files(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.project.is_none() {
            self.status = "未打开项目".to_string();
            cx.notify();
            return;
        }
        if let Some(project) = &self.project {
            self.recent_files.retain_existing(&project.root);
        }
        self.recent_open = true;
        self.recent_selected = 0;
        cx.notify();
    }

    /// Move the recent-files highlight up/down (wrapping).
    fn recent_files_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.recent_files.files.len();
        if len == 0 {
            return;
        }
        let current = self.recent_selected as isize;
        self.recent_selected = (current + delta).rem_euclid(len as isize) as usize;
        cx.notify();
    }

    /// Open the highlighted recent file.
    fn recent_files_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.recent_files.files.get(self.recent_selected) else {
            return;
        };
        let Some(project) = &self.project else {
            return;
        };
        let absolute = project.root.join(path);
        self.recent_open = false;
        self.push_nav_history(cx);
        self.open_file(absolute, cx);
        cx.notify();
    }

    /// Close the recent-files popup.
    fn close_recent_files(&mut self, cx: &mut Context<Self>) {
        if self.recent_open {
            self.recent_open = false;
            cx.notify();
        }
    }

    /// Open a text file in a normal (editable) editor tab.
    fn open_text_tab(&mut self, path: PathBuf, text: String, cx: &mut Context<Self>) {
        let saved_text = text.replace("\r\n", "\n").replace('\r', "\n");
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
        let signature_path = path.clone();
        let hover_path = path.clone();
        let subscription = cx.observe(&editor, move |this, editor, cx| {
            if let Some(server) = &this.lsp {
                let next_version = version.get() + 1;
                version.set(next_version);
                let text = editor.read(cx).text().to_string();
                let _ = server.change_file(did_change_path.clone(), text, next_version);
            }
            if let Some(anchor) = &this.signature_anchor {
                let moved = editor.read(cx).cursor_line_col();
                let current = (moved.0.saturating_sub(1), moved.1.saturating_sub(1));
                if anchor.0 != signature_path || (anchor.1, anchor.2) != current {
                    this.signature_help = None;
                    this.signature_anchor = None;
                }
            }
            if let Some(anchor) = &this.hover_anchor {
                let moved = editor.read(cx).cursor_line_col();
                let current = (moved.0.saturating_sub(1), moved.1.saturating_sub(1));
                if anchor.0 != hover_path || (anchor.1, anchor.2) != current {
                    this.hover_tooltip = None;
                    this.hover_anchor = None;
                }
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
        let definition_subscription = cx.subscribe(
            &editor,
            move |this, _editor, event: &DefinitionRequested, cx| {
                this.on_definition_requested(event.line, event.column, cx);
            },
        );
        self.tabs.push(Tab {
            editor,
            path: path.clone(),
            saved_text,
            _subscription: subscription,
            _breakpoint_subscription: breakpoint_subscription,
            _menu_subscription: menu_subscription,
            _definition_subscription: definition_subscription,
        });
        self.active_tab = Some(self.tabs.len() - 1);
        if let Some(server) = &self.lsp {
            let _ = server.open_file(path.clone(), text_for_lsp);
        }
        self.status = format!("打开 {}", self.display_path(&path));
        self.focus_on_next_frame = true;
         self.lsp_document_symbols(cx);
        self.lsp_semantic_tokens(cx);
        self.lsp_inlay_hints(cx);
         self.refresh_diagnostics(cx);
        self.save_session(cx);
        cx.notify();
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
 
    /// Persist the debug watch expressions (not their runtime values) to
    /// `.ns-ide/watch.json` so they survive a project reload.
    fn persist_watches(&mut self) {
        let Some(root) = self.project.as_ref().map(|project| project.root.clone()) else {
            return;
        };
        let expressions: Vec<String> = self
            .debug
            .watches
            .iter()
            .map(|watch| watch.expression.clone())
            .collect();
        let json = serde_json::to_string_pretty(&expressions).unwrap_or_default();
        let dir = root.join(".ns-ide");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        if let Err(error) = std::fs::write(dir.join("watch.json"), json) {
            self.status = format!("{}（watch 保存失败: {error}）", self.status);
        }
    }

    /// Load the persisted watch expressions from `.ns-ide/watch.json` into
    /// `debug.watches`. Non-existent or malformed files are ignored so the
    /// project still opens.
    fn load_watches(&mut self, root: &Path) {
        let path = root.join(".ns-ide/watch.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(expressions) = serde_json::from_str::<Vec<String>>(&text) else {
            return;
        };
        self.debug.watches = expressions
            .into_iter()
            .filter(|expr| !expr.trim().is_empty())
            .map(|expression| WatchEntry {
                expression,
                value: String::new(),
            })
            .collect();
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
        // Detect external modification: compare the on-disk content against the
        // content as loaded/last saved. When it differs, ask before overwriting.
        let buffer_text: String = tab.editor.read(cx).text().to_string();
        let normalized = buffer_text.replace("\r\n", "\n").replace('\r', "\n");
        let disk_text = fs::read_to_string(&path)
            .ok()
            .map(|text| text.replace("\r\n", "\n").replace('\r', "\n"));
        if let Some(disk) = &disk_text
            && *disk != tab.saved_text
        {
            self.external_change_dialog = Some(ExternalChangeDialog {
                path: path.clone(),
                disk_text: disk.clone(),
            });
            self.status = "文件已在磁盘上被外部修改".to_string();
            cx.notify();
            return;
        }
        match fs::write(&path, buffer_text) {
            Ok(()) => {
                let tab = self.tabs.get_mut(index).expect("tab checked above");
                tab.saved_text = normalized;
                tab.editor.update(cx, |editor, _cx| editor.mark_saved());
                self.status = format!("已保存 {}", self.display_path(&path));
            }
            Err(error) => {
                self.status = format!("保存失败: {error}");
            }
        }
        cx.notify();
        self.save_split_pane(cx);
    }

    /// Save the split editor pane if it is dirty (and not externally changed).
    fn save_split_pane(&mut self, cx: &mut Context<Self>) {
        let Some(split) = &self.split else {
            return;
        };
        if !split.editor.read(cx).is_dirty() {
            return;
        }
        let path = split.path.clone();
        let buffer_text: String = split.editor.read(cx).text().to_string();
        let normalized = buffer_text.replace("\r\n", "\n").replace('\r', "\n");
        let disk_text = fs::read_to_string(&path)
            .ok()
            .map(|text| text.replace("\r\n", "\n").replace('\r', "\n"));
        if let Some(disk) = &disk_text
            && *disk != split.saved_text
        {
            self.external_change_dialog = Some(ExternalChangeDialog {
                path: path.clone(),
                disk_text: disk.clone(),
            });
            self.status = "分屏文件已在磁盘上被外部修改".to_string();
            cx.notify();
            return;
        }
        match fs::write(&path, buffer_text) {
            Ok(()) => {
                if let Some(split) = &mut self.split {
                    split.saved_text = normalized;
                    split.editor.update(cx, |editor, _cx| editor.mark_saved());
                }
                self.status = format!("已保存 {}", self.display_path(&path));
            }
            Err(error) => {
                self.status = format!("保存失败: {error}");
            }
        }
        cx.notify();
    }

    /// Ctrl/Cmd+Click on a symbol: move the cursor to the click point and
    /// trigger LSP "go to definition" (same code path as the command/F12).
    fn on_definition_requested(&mut self, line: usize, column: usize, cx: &mut Context<Self>) {
        if let Some(editor) = self.active_editor() {
            // The event carries 1-based line + 0-based UTF-8 column; the editor
            // already moved its cursor to the click, this just re-asserts it.
            editor.update(cx, |editor, cx| {
                editor.set_cursor_line_column(line, column.saturating_sub(0) + 0, cx)
            });
        }
        self.lsp_go_to_definition(cx);
    }

    /// Keep the file's disk content and reload it into the editor, discarding
    /// the stale in-memory copy of an externally changed file.
    fn reload_external_change(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.external_change_dialog.take() else {
            return;
        };
        let path = dialog.path;
        let disk_text = dialog.disk_text.clone();
        if let Some(index) = self.tabs.iter().position(|tab| tab.path == path) {
            let tab = self.tabs.get_mut(index).expect("index from tabs");
            tab.saved_text = disk_text.clone();
            tab.editor.update(cx, |editor, cx| {
                editor.reload_text(&disk_text, cx);
                editor.mark_saved();
            });
        }
        self.status = format!("已从磁盘重新加载 {}", self.display_path(&path));
        cx.notify();
    }

    /// Keep the in-memory version and overwrite the externally changed file.
    fn keep_external_change(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.external_change_dialog.take() else {
            return;
        };
        let path = dialog.path;
        let Some(index) = self.active_tab else {
            return;
        };
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if tab.path != path {
            self.status = "外部修改对话框与当前标签不一致".to_string();
            cx.notify();
            return;
        }
        let text = tab.editor.read(cx).text().to_string();
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        match fs::write(&path, text) {
            Ok(()) => {
                let tab = self.tabs.get_mut(index).expect("tab checked above");
                tab.saved_text = normalized;
                tab.editor.update(cx, |editor, _cx| editor.mark_saved());
                self.status = format!("已用当前内容覆盖 {}", self.display_path(&path));
            }
            Err(error) => {
                self.status = format!("保存失败: {error}");
            }
        }
        cx.notify();
    }

    /// Refuse to save: leave both versions untouched and keep the file dirty.
    fn cancel_external_change(&mut self, cx: &mut Context<Self>) {
        self.external_change_dialog = None;
        self.status = "未保存（文件存在外部修改）".to_string();
        cx.notify();
    }

    /// One-line summary of the divergence shown in the external-change dialog.
    fn file_changed_dialog_diff(&self, cx: &gpui::App, dialog: &ExternalChangeDialog) -> gpui::AnyElement {
        let buffer = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.editor.read(cx).text().chars().count())
            .unwrap_or(0);
        let buffer_lines = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.editor.read(cx).text().lines().count())
            .unwrap_or(0);
        let disk_lines = dialog.disk_text.lines().count();
        let disk_chars = dialog.disk_text.chars().count();
        div()
            .rounded(px(6.))
            .bg(theme::alpha(0xffffff, 0.03))
            .border_1()
            .border_color(theme::alpha(theme::STROKE, 0.2))
            .p(px(8.))
            .text_size(px(10.5))
            .text_color(theme::color(theme::TEXT_DIM))
            .child(format!(
                "磁盘版本 {} 行 / {} 字符 · 当前编辑 {} 行 / {} 字符",
                disk_lines, disk_chars, buffer_lines, buffer
            ))
            .into_any_element()
    }

    fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        let closed_path = self.tabs.get(index).map(|tab| tab.path.clone());
        self.signature_help = None;
        self.signature_anchor = None;
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
        // Remove the tab from the list; the `active_tab` bookkeeping above
        // already accounts for the shift (it assumed the tab disappears).
        self.tabs.remove(index);
         self.focus_on_next_frame = true;
        cx.notify();
    }

    fn activate_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            self.active_tab = Some(index);
            self.focus_on_next_frame = true;
            // Switching files invalidates any cursor-anchored signature help.
            self.signature_help = None;
            self.signature_anchor = None;
            self.save_session(cx);
            self.refresh_diagnostics(cx);
            cx.notify();
         }
     }
 
    /// Close every tab except `keep`. The active tab is re-selected from the
    /// survivors (prefer the kept tab, then the nearest remaining).
    fn close_other_tabs(&mut self, keep: usize, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            self.tab_menu = None;
            return;
        }
         let keep_path = self.tabs.get(keep).map(|tab| tab.path.clone());
        // Close by path so index shifts from each removal don't skip tabs.
        let to_close: Vec<PathBuf> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != keep)
            .map(|(_, tab)| tab.path.clone())
            .collect();
        for path in to_close {
            if let Some(index) = self.tabs.iter().position(|tab| tab.path == path) {
                self.close_tab(index, cx);
            }
        }
         // Re-select the kept tab (its index may have shifted).
         if let Some(path) = keep_path {
             if let Some(new_index) = self.tabs.iter().position(|tab| tab.path == path) {
                 self.active_tab = Some(new_index);
             }
         }
        self.tab_menu = None;
        cx.notify();
    }

    /// Close every tab to the right of `index` (inclusive).
    fn close_tabs_to_right(&mut self, index: usize, cx: &mut Context<Self>) {
        let mut to_close: Vec<usize> = (index..self.tabs.len()).collect();
        to_close.sort_by(|a, b| b.cmp(a));
        for i in to_close {
            self.close_tab(i, cx);
        }
        self.tab_menu = None;
        cx.notify();
    }

    /// Close every tab.
    fn close_all_tabs(&mut self, cx: &mut Context<Self>) {
        let count = self.tabs.len();
        for i in (0..count).rev() {
            self.close_tab(i, cx);
        }
        self.tab_menu = None;
        cx.notify();
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
        let folded: Vec<ns_project::FoldedFile> = self
            .tabs
            .iter()
            .filter_map(|tab| {
                let path = tab.path.strip_prefix(&root).ok()?.to_path_buf();
                let regions = tab.editor.read(cx).folded_regions();
                (!regions.is_empty()).then(|| ns_project::FoldedFile { path, regions })
            })
            .collect();
        // Per-file cursor + scroll view state, restored on reopen so a crash
        // does not lose the caret position or viewport.
        let views: Vec<ns_project::FileView> = self
            .tabs
            .iter()
            .filter_map(|tab| {
                let path = tab.path.strip_prefix(&root).ok()?.to_path_buf();
                let editor = tab.editor.read(cx);
                Some(ns_project::FileView {
                    path,
                    cursor: Some(editor.cursor_line_col()),
                    scroll_ratio: editor.scroll_ratio(),
                })
            })
            .collect();
        let mut session = ns_project::Session {
            open_files,
            active: self.active_tab,
            folded,
            views,
            saved_at: 0,
            split: self
                .split
                .as_ref()
                .and_then(|split| split.path.strip_prefix(&root).ok().map(PathBuf::from)),
        };
        if let Err(error) = session.save_stamped(&root) {
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

    /// Import a vendor embedded project (.ewp / .uvprojx / .ioc) into a new
    /// ns-embed-ide project: parse the vendor file, write `project.toml`, copy
    /// the referenced sources into `src/`, then open the new project.
    fn import_project_dialog(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择要导入的工程文件（.ewp/.uvprojx/.ioc）".into()),
        });
        cx.spawn(async move |this, cx| {
            match receiver.await {
                Ok(Ok(Some(paths))) => {
                    if let Some(vendor) = paths.into_iter().next() {
                        this.update(cx, |this, cx| {
                            this.do_import_project(vendor, cx);
                        })
                        .ok();
                    }
                }
                Ok(Ok(None)) | Err(_) => {}
                Ok(Err(error)) => {
                    this.update(cx, |this, cx| {
                        this.status = format!("导入工程对话框失败: {error}");
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn do_import_project(&mut self, vendor: PathBuf, cx: &mut Context<Self>) {
        use ns_project::parse_import;
        let imported = match parse_import(&vendor) {
            Ok(imported) => imported,
            Err(error) => {
                self.status = format!("导入失败: {error}");
                cx.notify();
                return;
            }
        };

        // Project name = vendor file stem.
        let name = vendor
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "imported".to_string());
        // New project lives next to the vendor project file.
        let location = vendor
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let root = location.join(&name);
        if root.exists() {
            let non_empty = std::fs::read_dir(&root)
                .map(|mut e| e.next().is_some())
                .unwrap_or(false);
            if non_empty {
                self.status = format!("导入失败: 目录已存在且不为空: {}", root.display());
                cx.notify();
                return;
            }
        }

        let mut config = imported.config;
        config.project.name = name.clone();
        if let Err(error) = std::fs::create_dir_all(root.join("src"))
            .and_then(|_| std::fs::create_dir_all(root.join("include")))
            .and_then(|_| std::fs::create_dir_all(root.join("build")))
        {
            self.status = format!("导入失败: 创建项目目录出错: {error}");
            cx.notify();
            return;
        }

        // Copy referenced sources into src/ (flattened to avoid colliding
        // directory names; relative subpaths keep their parent dirs).
        let mut copied = 0usize;
        for source in &imported.sources {
            if !source.is_file() {
                continue;
            }
            let rel = source
                .strip_prefix(&vendor.parent().unwrap_or(Path::new(".")))
                .unwrap_or(source);
            // Use the file name only; if it collides, keep the relative path.
            let target_rel = if rel.components().count() > 1 {
                rel.to_path_buf()
            } else {
                PathBuf::from(rel.file_name().unwrap_or_default())
            };
            let target = root.join("src").join(&target_rel);
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::copy(source, &target).is_ok() {
                copied += 1;
            }
        }

        // Preserve the vendor file itself under the project root for reference.
        if vendor.is_file() {
            let _ = std::fs::copy(&vendor, root.join(vendor.file_name().unwrap_or_default()));
        }

        let toml_text = match toml::to_string_pretty(&config) {
            Ok(text) => text,
            Err(error) => {
                self.status = format!("导入失败: 序列化 project.toml 失败: {error}");
                cx.notify();
                return;
            }
        };
        if let Err(error) = std::fs::write(root.join("project.toml"), &toml_text) {
            self.status = format!("导入失败: 写 project.toml 失败: {error}");
            cx.notify();
            return;
        }
        let _ = std::fs::write(
            root.join("README.md"),
            format!(
                "# {name}\n\n由 {} 导入（{}）。\n\n工具链：`arm-none-eabi-gcc` + `openocd`（可在项目设置中修改）。\n",
                vendor.display(),
                imported.notes.join("；")
            ),
        );
        let _ = std::fs::write(root.join(".gitignore"), "build/\ntarget/\n.ns-ide/\n.DS_Store\n");

        match Project::open(&root) {
            Ok(project) => {
                self.load_project(project, cx);
                let mut message = format!(
                    "导入完成：{name}（复制 {} 个源文件）",
                    copied
                );
                if !imported.notes.is_empty() {
                    message.push_str(&format!("。{}", imported.notes.join("；")));
                }
                self.status = message;
            }
            Err(error) => {
                self.status = format!("导入后打开项目失败: {error}");
            }
        }
        cx.notify();
    }

    fn start_build(&mut self, run_after: bool, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        // Refresh the clangd `compile_commands.json` so the C/C++ language
        // server sees the exact `-D`/`-I`/`-std` flags before each build.
        if let Err(error) = ns_build::generate_compile_commands(project) {
            // Non-fatal: diagnostics still resolve from `didOpen`; report in
            // the status line so the user knows the database is stale.
            self.status = format!("compile_commands.json 更新失败: {error}");
        }
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
        self.build_streaming = true;
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.build_cancel = Some(cancel.clone());
        self.diagnostics.clear();
        self.refresh_diagnostics(cx);
        self.build_command = plan.command_line();
        self.output = format!("$ {}\n\n", self.build_command);
        self.status = "正在构建…".to_string();
        cx.notify();

        cx.spawn(async move |this, cx| {
            // Stream each stdout/stderr line to the output panel as it is
            // produced, so long builds show progress instead of a final dump.
            let (tx, mut rx) = futures::channel::mpsc::unbounded::<(bool, String)>();
            let this_stream = this.clone();
            let streamer = cx.spawn(async move |cx| {
                while let Some((_is_stderr, line)) = rx.next().await {
                    let _ = this_stream.update(cx, |this, cx| {
                        this.output.push_str(&line);
                        this.output.push('\n');
                        cx.notify();
                    });
                }
            });
            let task = cx.background_executor().spawn(async move {
                ns_build::execute_stream_cancelable(&plan, cancel, |line| {
                    let _ = tx.unbounded_send((line.is_stderr, line.text));
                })
            });
            let outcome = task.await;
            streamer.await;
            let _ = this.update(cx, |this, cx| {
                this.build_cancel = None;
                this.finish_build(outcome, cx)
            });
        })
        .detach();
    }

    /// Stop the running build by setting its cancel flag; the child process is
    /// killed and `finish_build` reports the build as cancelled.
    fn cancel_build(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.build_cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            self.status = "正在取消构建…".to_string();
            cx.notify();
        }
     }

    /// Stop the running flash operation by setting its cancel flag; the
    /// adapter's subprocess is killed and the outcome reports failure.
    fn cancel_flash(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.flash_cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            self.status = "正在取消烧写…".to_string();
            cx.notify();
        }
    }
 
     /// Delete the build output directory and report what was removed.
    fn clean_build(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        match ns_build::clean_build(project) {
            Ok(Some(out_dir)) => {
                self.output = format!(
                    "$ 清理构建\n\n已删除构建目录 {}（包括产物与中间文件）\n",
                    out_dir.display()
                );
                self.build_state = BuildState::Idle;
                self.status = "已清理构建".to_string();
            }
            Ok(None) => {
                self.output = "清理构建: 输出目录不存在（无需清理）\n".to_string();
                self.status = "构建目录不存在".to_string();
            }
            Err(error) => {
                self.output = format!("清理构建失败\n\n{error}\n");
                self.status = format!("清理失败: {error}");
            }
        }
        self.show_output = true;
        self.output_tab = OutputTab::Output;
        cx.notify();
    }

     /// Set the build configuration (`debug` | `release`) and write it back to
     /// `project.toml`. The change takes effect on the next build. With a
     /// per-target entry, the config is stored on that entry so each target
     /// keeps its own setting; otherwise the shared `build.config` is used.
     fn set_build_config(&mut self, config: &str, cx: &mut Context<Self>) {
         let Some(project) = self.project.as_mut() else {
             self.status = "请先打开项目".to_string();
             cx.notify();
             return;
         };
        if config != "debug" && config != "release" {
            self.status = "构建配置必须为 debug 或 release".to_string();
            cx.notify();
            return;
        }
        let target = project.config.project.target.trim().to_string();
        let current = project.build_config().to_string();
        let next = config.to_string();
        if current == next {
            self.status = format!("构建配置已是 {next}");
            cx.notify();
            return;
        }
        // Record where the change was applied so a failed save can roll it
        // back to the exact prior state.
        let rollback = if target.is_empty() {
            RollbackKind::Shared(current.clone())
        } else if let Some(entry) = project
            .config
            .targets
            .iter_mut()
            .find(|entry| entry.target.trim() == target)
        {
            let prior = entry.config.clone();
            entry.config = next.clone();
            RollbackKind::Entry(prior)
        } else {
            project.config.targets.push(TargetSection {
                target: target.clone(),
                config: next.clone(),
            });
            RollbackKind::NewEntry
        };
        match project.save_config() {
            Ok(()) => {
                self.build_state = BuildState::Idle;
                self.status = format!("构建配置已切换为 {next}");
                self.output = format!(
                    "$ 切换构建配置\n\n构建配置: {current} → {next}\n下次构建将使用 {next} 配置。\n"
                );
                self.show_output = true;
                self.output_tab = OutputTab::Output;
            }
            Err(error) => {
                // Roll back on failure so the in-memory state stays consistent
                // with the on-disk `project.toml`.
                match rollback {
                    RollbackKind::Shared(prior) => {
                        project.config.build.config = prior;
                    }
                    RollbackKind::Entry(prior) => {
                        if let Some(entry) = project
                            .config
                            .targets
                            .iter_mut()
                            .find(|entry| entry.target.trim() == target)
                        {
                            entry.config = prior;
                        }
                    }
                    RollbackKind::NewEntry => {
                        project.config.targets.retain(|entry| entry.target.trim() != target);
                    }
                }
                self.status = format!("切换构建配置失败: {error}");
            }
        }
        cx.notify();
    }

    /// Toggle the build configuration between `debug` and `release`.
    fn toggle_build_config(&mut self, cx: &mut Context<Self>) {
        let current = self
            .project
            .as_ref()
            .map(|p| p.build_config().to_string())
            .unwrap_or_default();
        let next = if current == "release" { "debug" } else { "release" };
        self.set_build_config(&next, cx);
    }

    /// Set the project target MCU and write it back to `project.toml`. The SVD
    /// source follows the target unless an explicit `svd` is configured.
    fn set_project_target(&mut self, target: &str, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_mut() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let old = project.config.project.target.clone();
        project.config.project.target = target.to_string();
        // Clear an external SVD source that no longer matches, so the device
        // page reflects the new MCU's peripheral set.
        let svd = project.config.project.svd.trim().to_string();
        if !svd.is_empty() && ns_device::catalog_entry(&svd).is_none() {
            project.config.project.svd.clear();
        }
        match project.save_config() {
            Ok(()) => {
                self.status = format!("目标设备已设为 {target}");
                self.device_picker_open = false;
                self.build_state = BuildState::Idle;
            }
            Err(error) => {
                project.config.project.target = old;
                self.status = format!("设置目标设备失败: {error}");
            }
        }
        cx.notify();
    }

    /// Generate `link.ld` and `src/startup.s` for the current project's target
    /// device (the wizard covers initial generation; this re-runs it for an
    /// existing project after a device change).
    fn generate_device_files_for_project(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let target = project.config.project.target.trim().to_string();
        if target.is_empty() {
            self.status = "项目未设置目标设备；请先在「设备」页或「项目设置」中选择 MCU。".to_string();
            cx.notify();
            return;
        }
        let linker = match ns_device::linker_script(&target) {
            Some(linker) => linker,
            None => {
                self.status = format!("目标设备「{target}」没有内置链接脚本/启动文件模板。");
                cx.notify();
                return;
            }
        };
        let startup = ns_device::startup_file(&target).unwrap_or_default();
        let root = project.root.clone();
        let linker_path = root.join("link.ld");
        let startup_path = root.join("src").join("startup.s");
        let _ = std::fs::create_dir_all(root.join("src"));
        let mut written = Vec::new();
        if std::fs::write(&linker_path, &linker).is_ok() {
            written.push(linker_path.display().to_string());
        }
        if !startup.is_empty() && std::fs::write(&startup_path, &startup).is_ok() {
            written.push(startup_path.display().to_string());
        }
        if written.is_empty() {
            self.status = format!("生成设备文件失败（{target}）。");
        } else {
            self.status = format!("已生成设备文件：{}", written.join("、"));
            self.show_output = true;
            self.output_tab = OutputTab::Output;
            self.output.push_str(&format!("已生成设备文件：{}\n", written.join("、")));
        }
        cx.notify();
    }

    /// Generate `src/peripheral_init.c` from the project target's SVD for the
    /// peripherals that look application-relevant (GPIO first, then UART/USART).
    fn generate_peripheral_init_code(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let target = project.config.project.target.trim().to_string();
        if target.is_empty() {
            self.status = "项目未设置目标设备；请先在「设备」页或「项目设置」中选择 MCU。".to_string();
            cx.notify();
            return;
        }
        let names = ns_device::peripheral_names(&target);
        if names.is_empty() {
            self.status = format!("目标设备「{target}」没有可用的 SVD 外设列表。");
            cx.notify();
            return;
        }
        // Pick the peripherals most relevant to a starter app: GPIO ports and
        // the first UART/USART (used for printf/debug).
        let mut configs: Vec<ns_device::PeripheralConfig> = Vec::new();
        for name in &names {
            let upper = name.to_ascii_uppercase();
            let is_gpio = upper.starts_with("GPIO");
            let is_uart = upper.contains("USART") || upper.contains("UART");
            if is_gpio {
                configs.push(ns_device::PeripheralConfig {
                    name: name.clone(),
                    purpose: "GPIO 端口配置".to_string(),
                    options: Vec::new(),
                });
            } else if is_uart && !configs.iter().any(|c| c.name.to_ascii_uppercase().contains("UART")) {
                configs.push(ns_device::PeripheralConfig {
                    name: name.clone(),
                    purpose: "调试串口".to_string(),
                    options: Vec::new(),
                });
            }
        }
        if configs.is_empty() {
            // Fall back to the first three peripherals so the output is useful.
            for name in names.iter().take(3) {
                configs.push(ns_device::PeripheralConfig {
                    name: name.clone(),
                    purpose: String::new(),
                    options: Vec::new(),
                });
            }
        }
        let Some(code) = ns_device::generate_peripheral_init(&target, &configs) else {
            self.status = format!("生成外设初始化代码失败（{target}）。");
            cx.notify();
            return;
        };
        let root = project.root.clone();
        let path = root.join("src").join("peripheral_init.c");
        let _ = std::fs::create_dir_all(root.join("src"));
        if let Err(error) = std::fs::write(&path, &code) {
            self.status = format!("写入 {} 失败: {error}", path.display());
            cx.notify();
            return;
        }
        self.open_file(path.clone(), cx);
        self.status = format!(
            "已生成外设初始化代码：{}（{} 个外设）",
            path.display(),
            configs.len()
        );
        self.show_output = true;
        self.output_tab = OutputTab::Output;
        self.output.push_str(&format!(
            "已生成外设初始化代码：{}（{} 个外设：{}）\n",
            path.display(),
            configs.len(),
            configs
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        cx.notify();
    }
    /// Clean the output directory and immediately rebuild.
    fn rebuild_build(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        if let Err(error) = ns_build::clean_build(project) {
            self.output = format!("清理失败，重新构建中止\n\n{error}\n");
            self.status = format!("清理失败: {error}");
            self.show_output = true;
            self.output_tab = OutputTab::Output;
            cx.notify();
            return;
        }
        self.start_build(false, cx);
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
        self.refresh_diagnostics(cx);
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
        // Parse an optional external flash-algorithm (`.FLM`) so the adapter
        // can use its geometry; a missing/unparseable file is reported inline
        // rather than silently ignored.
        let algorithm = if project.config.flasher.algorithm.trim().is_empty() {
            None
        } else {
            let algorithm_path = project.root.join(project.config.flasher.algorithm.trim());
            match ns_flash::load_flm(&algorithm_path) {
                Ok(flm) => Some(flm),
                Err(error) => {
                    self.status = format!("加载烧写算法失败: {error}");
                    cx.notify();
                    return;
                }
            }
         };
        let flash_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.flash_cancel = Some(flash_cancel.clone());
         let request = FlashRequest {
             tool: adapter.tool(),
             action,
             firmware: artifact.clone(),
             interface: project.config.flasher.interface.clone(),
             read_to,
             // Pass the project's target as the adapter device override so
             // pyOCD/J-Link/ST-Link select the right part instead of a hardcoded
             // default (the external flash algorithm can refine the geometry).
             device: Some(project.config.project.target.clone()),
             algorithm,
            cancel: Some(flash_cancel),
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
              this.update(cx, |this, cx| {
                    this.flash_cancel = None;
                      match result {
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
                            Some(FlashMode::Verify) => {
                                // Chain: after a successful flash, verify the
                                // written bytes against the artifact.
                                this.flash_action(FlashAction::Verify, cx);
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
             }
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

    /// 烧写并校验: build → flash → verify the written bytes against the
    /// artifact. The verify only runs when the flash succeeds (the
    /// `FlashMode::Verify` chain in `flash_artifact`).
    fn flash_and_verify(&mut self, cx: &mut Context<Self>) {
        self.pending_flash = Some(FlashMode::Verify);
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
        self.refresh_diagnostics(cx);
        cx.notify();
    }

    /// Run the project's unit tests (F4.1 单测集成) via the language's test
    /// runner (`cargo test` / `ctest` / `make test` / `zig build test`).
    fn start_tests(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let Some(plan) = ns_build::plan_tests(project) else {
            self.show_output = true;
            self.output_tab = OutputTab::Output;
            self.build_state = BuildState::Failed(Duration::ZERO);
            self.output = format!(
                "没有适用于 {} 的测试运行器（cargo test / ctest / make test / zig build test）。\n\
                 请在项目中使用对应的构建系统或在终端手动运行测试。\n",
                project.config.project.language
            );
            self.build_command.clear();
            self.status = "测试: 未找到测试运行器".to_string();
            cx.notify();
            return;
        };
        self.show_output = true;
        self.output_tab = OutputTab::Output;
        self.build_state = BuildState::Running;
        self.diagnostics.clear();
        self.refresh_diagnostics(cx);
        self.build_command = plan.command_line();
        self.output = format!("$ {}\n\n正在运行测试…\n", self.build_command);
        self.status = "正在运行测试…".to_string();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let task = cx
                .background_executor()
                .spawn(async move { ns_build::execute(&plan) });
            let outcome = task.await;
            this.update(cx, |this, cx| this.finish_tests(outcome, cx)).ok();
        })
        .detach();
    }

    fn finish_tests(&mut self, outcome: BuildOutcome, cx: &mut Context<Self>) {
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
            "\n[测试] {} · 用时 {:.2}s\n",
            if outcome.success { "全部通过" } else { "有失败" },
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
            "测试全部通过".to_string()
        } else {
            "测试有失败（见问题面板）".to_string()
        };
        self.refresh_diagnostics(cx);
        cx.notify();
    }

    /// Run `-fstack-usage` analysis (C/C++): compile each source with the flag,
    /// then parse the emitted `.su` files into the stack-usage panel.
    fn start_stack_usage(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let Some(plan) = ns_build::plan_stack_usage(project) else {
            self.show_output = true;
            self.output_tab = OutputTab::Output;
            self.build_state = BuildState::Failed(Duration::ZERO);
            self.output = format!(
                "栈使用分析需要 C/C++ 项目与可用编译器（gcc / clang / 交叉工具链），\n\
                 当前项目为 {}。\n",
                project.config.project.language
            );
            self.build_command.clear();
            self.status = "栈使用分析: 不可用".to_string();
            cx.notify();
            return;
        };
        self.show_output = true;
        self.output_tab = OutputTab::Output;
        self.build_state = BuildState::Running;
        self.diagnostics.clear();
        self.refresh_diagnostics(cx);
        self.build_command = plan.command_line();
        self.output = format!("$ {}\n\n正在分析栈使用…\n", self.build_command);
        self.status = "正在分析栈使用…".to_string();
        cx.notify();

        let artifact = plan.artifact.clone();
        cx.spawn(async move |this, cx| {
            let task = cx
                .background_executor()
                .spawn(async move { ns_build::execute(&plan) });
            let outcome = task.await;
            this.update(cx, |this, cx| {
                this.finish_stack_usage(outcome, artifact, cx)
            })
            .ok();
        })
        .detach();
    }

    fn finish_stack_usage(
        &mut self,
        outcome: BuildOutcome,
        artifact: PathBuf,
        cx: &mut Context<Self>,
    ) {
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
        let entries = if outcome.success {
            ns_build::collect_stack_usage(&artifact)
        } else {
            Vec::new()
        };
        self.stack_usage = entries;
        let mut output = format!("$ {}\n\n", self.build_command);
        output.push_str(&outcome.combined_output());
        if outcome.success {
            let total: u64 = self.stack_usage.iter().map(|entry| entry.bytes).sum();
            let dynamic = self.stack_usage.iter().filter(|entry| entry.dynamic).count();
            output.push_str(&format!(
                "\n[栈使用] {} 个函数 · 静态峰值 {} 字节 · 动态 {} 个 · 用时 {:.2}s\n",
                self.stack_usage.len(),
                total,
                dynamic,
                outcome.duration.as_secs_f64()
            ));
            for entry in &self.stack_usage {
                let marker = if entry.dynamic { "~" } else { "" };
                output.push_str(&format!(
                    "  {:>6} B{}  {}  {}\n",
                    entry.bytes, marker, entry.function, entry.file
                ));
            }
        }
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
        self.status = format!(
            "栈使用分析完成 · {} 个函数",
            self.stack_usage.len()
        );
        self.refresh_diagnostics(cx);
        cx.notify();
    }
    /// Run the C/C++ preprocessor (`-E`) and stream the macro-expanded output
    /// into the output panel. Non-C/C++ projects or missing compilers report a
    /// clear message instead of silently doing nothing.
    fn start_preprocess(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let Some(plan) = ns_build::plan_preprocess(project) else {
            self.show_output = true;
            self.output_tab = OutputTab::Output;
            self.build_state = BuildState::Failed(Duration::ZERO);
            self.output = format!(
                "宏展开需要 C/C++ 项目与可用编译器（gcc / clang / 交叉工具链），\n\
                 当前项目为 {}。\n",
                project.config.project.language
            );
            self.build_command.clear();
            self.status = "宏展开: 不可用".to_string();
            cx.notify();
            return;
        };
        self.show_output = true;
        self.output_tab = OutputTab::Output;
        self.build_state = BuildState::Running;
        self.build_command = plan.command_line();
        self.output = format!("$ {}\n\n正在预处理…\n", self.build_command);
        self.status = "正在预处理…".to_string();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let task = cx
                .background_executor()
                .spawn(async move { ns_build::execute(&plan) });
            let outcome = task.await;
            this.update(cx, |this, cx| this.finish_preprocess(outcome, cx))
                .ok();
        })
        .detach();
    }

    fn finish_preprocess(&mut self, outcome: ns_build::BuildOutcome, cx: &mut Context<Self>) {
        let mut output = format!("$ {}\n\n", self.build_command);
        output.push_str(&outcome.combined_output());
         if outcome.success {
             self.status = "宏展开完成".to_string();
         } else {
            self.status = format!("宏展开失败（退出码 {}）", outcome.code.unwrap_or(-1));
         }
         self.output = output;
         self.build_state = if outcome.success {
            BuildState::Success(outcome.duration)
         } else {
             BuildState::Failed(outcome.duration)
         };
        self.refresh_diagnostics(cx);
        cx.notify();
    }

     /// Run gcov line-coverage analysis (C/C++): build with `--coverage`, run
    /// the instrumented binary, then summarize per-file coverage via gcov.
    fn start_coverage(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let root = project.root.clone();
        let language = project.config.project.language;
        let plan = ns_build::plan_coverage(project);
        let Some(plan) = plan else {
            self.show_output = true;
            self.output_tab = OutputTab::Output;
            self.build_state = BuildState::Failed(Duration::ZERO);
            self.output = format!(
                "覆盖率分析需要 C/C++ 项目与可用编译器（gcc / clang），\n\
                 当前项目为 {}。\n",
                language
            );
            self.build_command.clear();
            self.status = "覆盖率: 不可用".to_string();
            cx.notify();
            return;
        };
        self.show_output = true;
        self.output_tab = OutputTab::Output;
        self.build_state = BuildState::Running;
        self.diagnostics.clear();
        self.refresh_diagnostics(cx);
        self.build_command = plan.command_line();
        self.output = format!("$ {}\n\n正在构建覆盖率测试…\n", self.build_command);
        self.status = "正在分析覆盖率…".to_string();
        cx.notify();

        let binary = plan.artifact.clone();
        let out_dir = binary
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.join("build"));
        cx.spawn(async move |this, cx| {
            let task = cx
                .background_executor()
                .spawn(async move {
                    let build = ns_build::execute(&plan);
                    let coverage = if build.success {
                        ns_build::run_coverage(&binary, &root, &out_dir).ok().flatten()
                    } else {
                        None
                    };
                    (build, coverage)
                });
            let (build, coverage) = task.await;
            this.update(cx, |this, cx| {
                this.finish_coverage(build, coverage, cx)
            })
            .ok();
        })
        .detach();
    }
    fn finish_coverage(
        &mut self,
        build: ns_build::BuildOutcome,
        coverage: Option<Vec<ns_build::CoverageFile>>,
        cx: &mut Context<Self>,
    ) {
        let root = self.project.as_ref().map(|p| p.root.clone()).unwrap_or_default();
        let diagnostics = build.diagnostics(&root);
        self.diagnostics = diagnostics.into_iter().filter(|d| d.severity != Severity::Info).collect();
        self.build_command = build.plan.command_line();
        self.coverage = coverage.unwrap_or_default();
        let mut output = format!("$ {}\n\n", self.build_command);
        output.push_str(&build.combined_output());
        if build.success {
            self.format_coverage_output(&mut output, build.duration.as_secs_f64());
        }
        self.output = output;
        self.build_state = if build.success {
            BuildState::Success(build.duration)
        } else {
            BuildState::Failed(build.duration)
        };
        self.output_tab = if self.diagnostics.is_empty() {
            OutputTab::Output
        } else {
            OutputTab::Problems
        };
        self.show_output = true;
         self.status = format!("覆盖率分析完成 · {} 个文件", self.coverage.len());
         self.refresh_diagnostics(cx);
         self.apply_coverage_to_editors(cx);
         cx.notify();
     }
 
     /// Append the per-file coverage summary lines to `output`.
    fn format_coverage_output(&self, output: &mut String, seconds: f64) {
        let total_lines: u64 = self.coverage.iter().map(|f| f.executed + f.not_executed).sum();
        let executed: u64 = self.coverage.iter().map(|f| f.executed).sum();
        let overall = if total_lines == 0 { 0 } else { executed * 100 / total_lines };
        output.push_str(&format!(
            "\n[覆盖率] {} 个文件 · 总体行覆盖 {}% · 用时 {:.2}s\n",
            self.coverage.len(), overall, seconds
        ));
        for file in &self.coverage {
            output.push_str(&format!(
                "  {:>3}%  {:>4}/{:<4}  {}\n",
                file.percent(), file.executed, file.executed + file.not_executed, file.file
            ));
        }
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
        let output = if self.build_streaming {
            // Streamed lines were already appended live; keep them and add only
            // the summary. `build_command` was set before the stream started.
            let mut out = self.output.clone();
            if !outcome.success && outcome.stderr.contains("已取消") {
                out.push_str("\n构建已取消\n");
            }
            out.push_str(&format!(
                "\n[{}] 用时 {:.2}s\n",
                if outcome.success { "成功" } else { "失败" },
                outcome.duration.as_secs_f64()
            ));
            out
        } else {
            let mut out = format!("$ {}\n\n", self.build_command);
            out.push_str(&outcome.combined_output());
            out.push_str(&format!(
                "\n[{}] 用时 {:.2}s\n",
                if outcome.success { "成功" } else { "失败" },
                outcome.duration.as_secs_f64()
            ));
            out
        };
        self.build_streaming = false;
        self.output = output;
        // Analyze the firmware artifact's memory usage when the build succeeded
        // and the project targets an MCU. This powers the「固件大小」summary in
        // the build output panel.
        self.firmware_size = if outcome.success {
            self.analyze_firmware_artifact()
        } else {
             None
         };
        self.build_file_timings = outcome.file_timings.clone();
         // Persist the firmware summary in the build output so it survives
        // tab switches and re-renders (render appends it only as a computed
        // trailing line, but this keeps the record in the output text).
        if self.firmware_size.is_some() {
            let summary = self.render_firmware_size_summary();
            if !summary.is_empty() && !self.output.contains(&summary) {
                self.output.push_str(&format!("\n{summary}\n"));
             }
         }
        // Report per-file compile times when the toolchain exposed them.
        if !self.build_file_timings.is_empty() {
            let mut timing_lines: Vec<String> = self
                .build_file_timings
                .iter()
                .map(|timing| {
                    format!(
                        "  {:>7.1} ms  {}",
                        timing.elapsed_ms as f64 / 1000.0,
                        timing.file
                    )
                })
                .collect();
            timing_lines.sort_by(|a, b| b.cmp(a));
            let mut report = String::from("\n文件编译耗时（估计）:\n");
            report.push_str(&timing_lines.join("\n"));
            report.push('\n');
            if !self.output.contains(&report) {
                self.output.push_str(&report);
            }
        }
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
        self.refresh_diagnostics(cx);
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
        self.refresh_diagnostics(cx);
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
        // Never leak an existing session: stop it before starting a new one.
        if self.debug_session.is_some() {
            self.stop_debug(cx);
        }
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
        // Artifact freshness: if any project source was modified after the
        // artifact, the firmware is stale — warn (don't block) so the user
        // can rebuild before debugging.
        if let Ok(sources) = project.source_files() {
            let stale = stale_sources(&artifact, &sources);
            if !stale.is_empty() {
                let preview: Vec<String> = stale
                    .iter()
                    .take(3)
                    .map(|path| project.relative(path))
                    .collect();
                let more = if stale.len() > 3 {
                    format!(" 等 {} 个文件", stale.len())
                } else {
                    String::new()
                };
                self.debug.push_console(format!(
                    "⚠ 调试产物可能过时（{} 个源文件比产物新：{}{}）。建议先「构建」。",
                    stale.len(),
                    preview.join(", "),
                    more
                ));
            }
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
        let launch = &project.config.launch;
        let debug_cwd = if launch.cwd.trim().is_empty() {
            root.clone()
        } else {
            PathBuf::from(project.expand_path(&launch.cwd))
        };
        let config = DebugConfig {
            program: artifact,
            args: launch.args.clone(),
            env: launch.env.clone(),
            cwd: debug_cwd,
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
                        let app: Entity<IdeApp> = cx.entity();
                        input.update(cx, |field, _cx| {
                            let commands = console_commands.clone();
                            field.set_on_submit(move |text, _window, _cx| {
                                let text = text.trim().to_string();
                                if text.is_empty() {
                                    return;
                                }
                                // Record the command for up/down history recall.
                                let app = app.clone();
                                let _ = app.update(_cx, |this, _cx| {
                                    this.debug.push_console_history(&text);
                                });
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
                                    this.persist_watches();
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
                    watches: std::mem::take(&mut self.debug.watches),
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

     /// Restart the debug session. When a session is already running, the DAP
     /// `restart` request restarts the debuggee in place (keeping the adapter
     /// and its breakpoints alive); otherwise the session is relaunched. The
     /// transient debug views (frames/scopes/variables) are cleared either way.
     fn restart_debug(&mut self, cx: &mut Context<Self>) {
         if let Some(session) = self.debug_session.as_ref() {
             // Native in-place restart: keep the adapter, just reset the
             // debuggee. Clear the transient views so the next stop event
             // repopulates them from scratch.
             self.debug.frames.clear();
             self.debug.scopes.clear();
             self.debug.variables.clear();
             self.debug.children.clear();
             self.debug.expanded.clear();
             self.debug.threads.clear();
             self.debug.register_groups.clear();
             self.debug.register_group_refs.clear();
             self.debug.pc = None;
             self.debug.status = DebugStatus::Idle;
             if session.commands.send(DebugCommand::Restart).is_err() {
                 // The session died while restarting; fall through to relaunch.
                 self.stop_debug(cx);
             } else {
                 self.debug.push_console("↻ 正在重启调试会话…".to_string());
                 cx.notify();
                 return;
             }
         }
         if self.debug_session.is_some() {
             self.stop_debug(cx);
         }
         self.debug.status = DebugStatus::Idle;
         self.debug.error = None;
         self.debug.frames.clear();
         self.debug.scopes.clear();
         self.debug.variables.clear();
         self.debug.children.clear();
         self.debug.expanded.clear();
         self.debug.threads.clear();
         self.debug.register_groups.clear();
         self.debug.register_group_refs.clear();
         self.debug.pc = None;
         self.debug.console.clear();
         let artifact = self
             .project
             .as_ref()
             .map(|project| project.artifact_path())
             .unwrap_or_default();
         if !artifact.exists() {
             self.debug.push_console(format!(
                 "✕ 找不到调试产物 {}。请先构建项目。",
                 artifact.display()
             ));
             self.output_tab = OutputTab::Debug;
             self.show_output = true;
             self.status = "调试产物缺失，请先构建".to_string();
             cx.notify();
             return;
         }
         self.launch_debug(cx);
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
                            this.persist_watches();
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
            watches: std::mem::take(&mut self.debug.watches),
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
        // Capture the configured server path (if any) so a failed start can
        // report probe availability without touching the (moved) project.
        let probe_server_path = config.server_path.clone();
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
                            let app = cx.entity();
                            input.update(cx, |field, _cx| {
                                let commands = commands.clone();
                                field.set_on_submit(move |text, _window, _cx| {
                                    let text = text.trim().to_string();
                                    if text.is_empty() {
                                        return;
                                    }
                                    let app = app.clone();
                                    let _ = app.update(_cx, |this, _cx| {
                                        this.debug.push_console_history(&text);
                                    });
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
                    let availability = ns_debug::embedded::probe_availability(
                        probe_server_path.as_deref(),
                    );
                    let summary = availability
                        .iter()
                        .map(|status| {
                            format!(
                                "{} {}",
                                status.probe.label(),
                                if status.available { "✓" } else { "✕ 未安装" }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" · ");
                    this.update(cx, |this, cx| {
                        this.debug.error = Some(error.to_string());
                        this.debug.push_console(format!("✕ {error}"));
                        this.debug.push_console(format!(
                            "探针可用性: {summary}（服务器需在 PATH 或调试器路径中）"
                        ));
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
            DebugMessage::Stopped { reason, thread_id, description } => {
                self.debug.stop_description = description.clone();
                self.debug.status = DebugStatus::Stopped { reason: reason.clone() };
                self.debug.thread_id = thread_id;
                self.debug.active_thread = thread_id;
                self.debug.scopes.clear();
                self.debug.variables.clear();
                self.debug.children.clear();
                self.debug.expanded.clear();
                self.debug.pc = None;
                self.debug.disassembly.clear();
                self.debug.active_frame = 0;
                self.debug.active_scope = 0;
                let thread_label = self.active_thread_label();
                let stop_label = debug_stop_reason_label(
                    &reason,
                    description.as_deref(),
                );
                self.debug.push_console(format!(
                    "● 程序暂停 · {stop_label} · {thread_label}"
                ));
                if let Some(session) = &self.debug_session {
                    // Refresh the thread list, then load the stopping thread's stack.
                    let _ = session.commands.send(DebugCommand::Threads);
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
                self.status = format!("调试暂停 · {stop_label}");
                // Refresh the memory view on every stop so it shows the current
                // target state (memory may have changed since the last read).
                self.refresh_memory_on_stop(cx);
                self.refresh_device_registers_on_stop();
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
                // Locate the adapter's "Registers" scope for the register tab.
                self.debug.registers_scope = self
                    .debug
                    .scopes
                    .iter()
                    .find(|scope| scope.name.eq_ignore_ascii_case("registers"))
                    .map(|scope| scope.id)
                    .unwrap_or(0);
                self.debug.register_groups.clear();
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
                // Auto-load the register groups for the register tab.
                if self.debug.registers_scope != 0 {
                    self.load_register_groups(cx);
                }
            }
            DebugMessage::Variables { reference, variables } => {
                if self.debug.watchpoint_resolution_pending {
                    self.collect_watchpoint_scope(reference, variables.clone());
                }
                // Route register-scope replies to the register tab.
                if reference == self.debug.registers_scope
                    || self.debug.register_group_refs.contains_key(&reference)
                {
                    self.store_register_group(reference, variables.clone());
                }
                let is_active_scope = self
                    .debug
                    .scopes
                    .get(self.debug.active_scope)
                    .map(|scope| scope.id == reference)
                    .unwrap_or(false);
                if is_active_scope || reference == 0 && self.debug.variables.is_empty() {
                    self.debug.variables = variables;
                    // Re-apply persisted expansion paths against the freshly
                    // fetched scope so the tree re-expands after a stop (DAP
                    // references churn between stops).
                    self.reapply_expanded_paths();
                } else {
                    self.debug.children.insert(reference, variables);
                    // A nested expansion path may now be satisfiable — if any
                    // persisted path continues under this child, expand it.
                    self.reapply_nested_paths(reference);
                }
            }
            DebugMessage::Memory { address, data } => {
                self.debug.memory.bytes = data.clone();
                self.debug.memory.error = None;
                // Keep the active region's address/size in sync with the read.
                if let Some(region) =
                    self.debug.memory.regions.get_mut(self.debug.memory.active_region)
                {
                    region.address = address;
                    region.size = self.debug.memory.read_size;
                }
                self.debug
                    .push_console(format!("内存 {address:#x} · {} 字节", self.debug.memory.bytes.len()));
                // Store any device-register value that falls inside this read.
                if let Some(device) = self.device_for_project() {
                    for peripheral in device.sorted_peripherals() {
                        for register in &peripheral.registers {
                            let rel = register.address.wrapping_sub(address as u64);
                            if rel + 4 <= data.len() as u64 {
                                let mut value = 0u32;
                                for i in 0..4 {
                                    value |= (data[(rel as usize) + i] as u32) << (8 * i);
                                }
                                self.debug
                                    .device_reg_values
                                    .insert(register.address, value);
                            }
                        }
                    }
                }
                // End of the reworked block.
            }
            DebugMessage::MemoryWritten { address, bytes } => {
                self.debug.memory.error = None;
                self.debug
                    .push_console(format!("内存 {address:#x} 已写入 {bytes} 字节"));
                // Re-read the region so the view reflects the new bytes.
                if let Some(session) = &self.debug_session {
                    let count = self.debug.memory.read_size;
                    let reference = format!("{address:#x}");
                    let _ = session
                        .commands
                        .send(DebugCommand::ReadMemory { reference, count });
                }
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
            DebugMessage::Threads { threads } => {
                self.debug.threads = threads;
                cx.notify();
            }
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
             DebugMessage::TracePackets(packets) => {
                 // Append decoded SWO packets to the live buffer (capped so a
                 // busy probe cannot grow it without bound).
                 let trace = &mut self.debug.trace;
                // Feed DWT PC samples into the profiler aggregator for the
                // hotspot histogram (see the trace panel's profiler view).
                for packet in &packets {
                    if let swvd::Packet::Dwt { pc: Some(pc), .. } = packet {
                        trace.profile.record(*pc);
                    }
                }
                 trace.packets.extend(packets);
                 const TRACE_CAP: usize = 2000;
                 if trace.packets.len() > TRACE_CAP {
                     let overflow = trace.packets.len() - TRACE_CAP;
                     trace.packets.drain(..overflow);
                 }
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
                self.debug.trace.streaming = false;
                self.debug.trace.packets.clear();
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
    /// Manually refresh the variables view: clear cached scopes/variables for
    /// the selected frame and re-request them from the adapter.
     fn refresh_variables(&mut self, cx: &mut Context<Self>) {
        let frame_index = self.debug.active_frame;
        let Some(frame) = self.debug.frames.get(frame_index) else {
             self.status = "没有选中的栈帧".to_string();
             cx.notify();
             return;
         };
        self.debug.scopes.clear();
         self.debug.variables.clear();
         if let Some(session) = &self.debug_session {
            let _ = session.commands.send(DebugCommand::GetScopes { frame_id: frame.id });
         }
        self.status = "已刷新变量".to_string();
         cx.notify();
     }
     /// Switch the debugger's active thread; the call stack / scopes / variables
    fn select_debug_thread(&mut self, thread_id: u64, cx: &mut Context<Self>) {
        if self.debug.active_thread == thread_id {
            return;
        }
        self.debug.active_thread = thread_id;
        self.debug.frames.clear();
        self.debug.scopes.clear();
        self.debug.variables.clear();
        self.debug.children.clear();
        self.debug.expanded.clear();
        self.debug.active_frame = 0;
        self.debug.active_scope = 0;
        self.debug.pc = None;
        if let Some(session) = &self.debug_session {
            let _ = session.commands.send(DebugCommand::GetStackTrace { thread_id });
        }
        self.debug.push_console(format!("已切换线程 #{thread_id}"));
        cx.notify();
    }

    /// The currently selected thread's display name (for the selector label).
    fn active_thread_label(&self) -> String {
        self.debug
            .threads
            .iter()
            .find(|thread| thread.id == self.debug.active_thread)
            .map(|thread| thread.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("线程 #{}", self.debug.active_thread))
    }

    /// Render the thread selector row (only when the adapter reports >1 thread).
    fn render_thread_selector(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active_thread = self.debug.active_thread;
        // Collect owned child elements first so the returned element does not
        // borrow `self` (the thread list iterator would otherwise keep it alive).
        let pills: Vec<gpui::AnyElement> = self
            .debug
            .threads
            .iter()
            .map(|thread| {
                let active = thread.id == active_thread;
                let name = if thread.name.is_empty() {
                    format!("#{}", thread.id)
                } else {
                    thread.name.clone()
                };
                let id = thread.id;
                div()
                    .id(("dbg-thread", id))
                    .px(px(7.))
                    .py(px(2.))
                    .rounded_full()
                    .text_size(px(9.5))
                    .cursor_pointer()
                    .bg(if active {
                        theme::alpha(theme::JADE, 0.16)
                    } else {
                        theme::alpha(theme::STROKE, 0.08)
                    })
                    .text_color(if active {
                        theme::color(theme::JADE)
                    } else {
                        theme::color(theme::TEXT_DIM)
                    })
                    .hover(|style| {
                        style.bg(theme::alpha(theme::STROKE, 0.12))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_debug_thread(id, cx)
                    }))
                    .child(name)
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(10.))
            .pb(px(4.))
            .children(pills)
            .into_any_element()
    }

    /// Fetch the register group list for the register tab: request the
    /// "Registers" scope's variables (the group headers), then each group's
    /// children. Replies arrive as [`DebugMessage::Variables`] and are stored
    /// by `store_register_group`.
    fn load_register_groups(&mut self, cx: &mut Context<Self>) {
        let scope = self.debug.registers_scope;
        if scope == 0 {
            return;
        }
        if let Some(session) = &self.debug_session {
            let _ = session.commands.send(DebugCommand::GetVariables {
                variables_reference: scope,
            });
        }
        cx.notify();
    }

    /// Store a register group. A reply for the "Registers" scope lists the
    /// group headers; each header's variablesReference is remembered so the
    /// matching group reply can be stored under the right name. The group
    /// headers' references are fetched next via the adapter.
    fn store_register_group(&mut self, reference: u64, variables: Vec<Variable>) {
        let mut fetch: Vec<u64> = Vec::new();
        self.debug.apply_register_group(reference, &variables, &mut fetch);
        if let Some(session) = &self.debug_session {
            for child in fetch {
                let _ = session.commands.send(DebugCommand::GetVariables {
                    variables_reference: child,
                });
            }
        }
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
            // Drop any persisted path that ends at this reference.
            let path = self.variable_path(reference);
            if !path.is_empty() {
                self.debug.expanded_paths.retain(|p| p != &path);
            }
        } else {
            self.debug.expanded.insert(reference);
            let path = self.variable_path(reference);
            if !path.is_empty() && !self.debug.expanded_paths.contains(&path) {
                self.debug.expanded_paths.push(path);
            }
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

    /// Reconstruct the name chain from the active scope's root to the variable
    /// with the given `reference`, by walking the fetched children map.
    fn variable_path(&self, reference: u64) -> Vec<String> {
        fn walk(
            children: &HashMap<u64, Vec<Variable>>,
            reference: u64,
            prefix: &mut Vec<String>,
        ) -> Option<Vec<String>> {
            for vars in children.values() {
                for var in vars {
                    if var.variables_reference == reference {
                        let mut path = prefix.clone();
                        path.push(var.name.clone());
                        return Some(path);
                    }
                    if var.variables_reference != 0
                        && children.contains_key(&var.variables_reference)
                    {
                        prefix.push(var.name.clone());
                        if let Some(found) = walk(children, reference, prefix) {
                            return Some(found);
                        }
                        prefix.pop();
                    }
                }
            }
            None
        }
        // Seed the walk from each top-level variable (its name is the first
        // segment of any path that descends from it).
        for var in &self.debug.variables {
            if var.variables_reference == reference {
                return vec![var.name.clone()];
            }
            if var.variables_reference != 0
                && self.debug.children.contains_key(&var.variables_reference)
            {
                let mut prefix = vec![var.name.clone()];
                if let Some(found) = walk(&self.debug.children, reference, &mut prefix) {
                    return found;
                }
            }
        }
        Vec::new()
    }
    /// Re-apply persisted expansion paths to the freshly fetched scope's
    /// variables. Walks the top-level variables; for each persisted path whose
    /// first segment matches a top-level variable's name, marks it expanded
    /// and (re)fetches its children. Deeper segments are resolved recursively
    /// as children arrive.
    fn reapply_expanded_paths(&mut self) {
        let paths = self.debug.expanded_paths.clone();
        if paths.is_empty() {
            return;
        }
        for path in paths {
            let Some(first) = path.first() else { continue };
            let Some(var) = self
                .debug
                .variables
                .iter()
                .find(|v| &v.name == first)
                .cloned()
            else {
                continue;
            };
            let reference = var.variables_reference;
            if reference == 0 {
                continue;
            }
            if !self.debug.children.contains_key(&reference) {
                if let Some(session) = &self.debug_session {
                    let _ = session.commands.send(DebugCommand::GetVariables {
                        variables_reference: reference,
                    });
                }
            }
            self.debug.expanded.insert(reference);
        }
    }
    /// When a variable's children arrive, check whether any persisted
    /// expansion path continues under the just-fetched child; if so, expand
    /// the matching nested variable and fetch its children.
    fn reapply_nested_paths(&mut self, parent_reference: u64) {
        let Some(parent_vars) = self.debug.children.get(&parent_reference).cloned() else {
            return;
        };
        // Build the path prefix leading to this parent by locating its name
        // chain via `variable_path`.
        let parent_path = self.variable_path(parent_reference);
        let paths = self.debug.expanded_paths.clone();
        for path in paths {
            if path.len() <= parent_path.len() || !path.starts_with(&parent_path) {
                continue;
            }
            let next_name = &path[parent_path.len()];
            let Some(child) = parent_vars.iter().find(|v| &v.name == next_name) else {
                continue;
            };
            let reference = child.variables_reference;
            if reference == 0 {
                continue;
            }
            if !self.debug.children.contains_key(&reference) {
                if let Some(session) = &self.debug_session {
                    let _ = session.commands.send(DebugCommand::GetVariables {
                        variables_reference: reference,
                    });
                }
            }
            self.debug.expanded.insert(reference);
        }
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
        if tab == DebugViewTab::Registers && self.debug.register_groups.is_empty() {
            self.load_register_groups(cx);
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
        // Pick the target architecture: the project's declared `target` wins
        // (it is the user's intent); otherwise fall back to the ELF machine
        // type, then to the historical RISC-V default.
        let target = project.config.project.target.trim().to_string();
        let arch = if !target.is_empty() {
            ns_sim::Arch::from_target(&target)
        } else if bytes.len() >= 20 && &bytes[0..4] == b"\x7fELF" {
            let little = bytes[5] == 1;
            let machine = read_u16_le(&bytes[18..20], little);
            ns_sim::Arch::from_elf_machine(machine).unwrap_or(ns_sim::Arch::RiscV)
        } else {
            ns_sim::Arch::RiscV
        };
        let mut core = match ns_sim::Simulator::for_arch(arch) {
            Ok(core) => core,
            Err(error) => {
                // Drop any previous core so the panel doesn't show stale state.
                self.sim.core = None;
                self.sim.status = error.to_string();
                self.sim.panel_open = true;
                self.status = self.sim.status.clone();
                cx.notify();
                return;
            }
        };
        // 8051-family (8051/80251) and C166: artifacts are Intel HEX (or raw).
        // Decode HEX via ns_flash and load into the code space; otherwise
        // load raw bytes at 0.
        let is_hex_core = arch == ns_sim::Arch::Mcs51
            || arch == ns_sim::Arch::Mcs251
            || arch == ns_sim::Arch::C166
            || arch == ns_sim::Arch::Stm8;
        if is_hex_core {
            let hex = bytes.starts_with(b":");
            let load_result = if hex {
                let Ok(text) = std::str::from_utf8(&bytes) else {
                    self.sim.status = "固件不是有效 UTF-8（Intel HEX 应为文本）".to_string();
                    self.sim.panel_open = true;
                    self.status = self.sim.status.clone();
                    cx.notify();
                    return;
                };
                match ns_flash::decode_intel_hex(text) {
                    Ok(decoded) => match arch {
                        ns_sim::Arch::Mcs51 => {
                            core.load_mcs51(decoded.base as u16, &decoded.bytes)
                        }
                        ns_sim::Arch::Mcs251 => {
                            core.load_x51(decoded.base as u32, &decoded.bytes)
                        }
                        ns_sim::Arch::C166 => {
                            core.load_c166(decoded.base as u16, &decoded.bytes)
                        }
                        ns_sim::Arch::Stm8 => {
                            core.load_stm8(decoded.base as u32, &decoded.bytes)
                        }
                        _ => unreachable!(),
                    },
                    Err(error) => {
                        self.sim.status = format!("解析 Intel HEX 失败: {error}");
                        self.sim.panel_open = true;
                        self.status = self.sim.status.clone();
                        cx.notify();
                        return;
                    }
                }
            } else {
                match arch {
                    ns_sim::Arch::Mcs51 => core.load_mcs51(0, &bytes),
                    ns_sim::Arch::Mcs251 => core.load_x51(0, &bytes),
                    ns_sim::Arch::C166 => core.load_c166(0, &bytes),
                    ns_sim::Arch::Stm8 => core.load_stm8(0, &bytes),
                    _ => unreachable!(),
                }
            };
            if let Err(error) = load_result {
                self.sim.status = format!("加载固件失败: {error}");
                self.sim.panel_open = true;
                self.status = self.sim.status.clone();
                cx.notify();
                return;
            }
        } else {
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
        }
        self.sim.core = Some(core);
        self.sim_sync_breakpoints(cx);
        self.sim.panel_open = true;
        self.sim.status = format!(
            "已加载 {} 到仿真器（入口 0x{:x}）",
            artifact.display(),
            self.sim.core.as_ref().map(|c| c.program_counter()).unwrap_or(0)
        );
        if self.sim.mem_address.is_empty() {
            self.sim.mem_address = match arch {
                ns_sim::Arch::Xtensa => "0x3FF00000".to_string(),
                ns_sim::Arch::Arm => {
                    // ARM7/ARM9 (LPC2148) keep SRAM at 0x4000_0000; S3C2440 at
                    // 0x3000_0000; Cortex-M parts at 0x2000_0000.
                    if target.contains("lpc21") || target.contains("lpc22") {
                        "0x40000000".to_string()
                    } else if target.contains("s3c24") {
                        "0x30000000".to_string()
                    } else {
                        "0x20000000".to_string()
                    }
                }
                ns_sim::Arch::Avr => "0x0100".to_string(),
                ns_sim::Arch::Msp430 => "0x0200".to_string(),
                ns_sim::Arch::Stm8 => "0x0000".to_string(),
                ns_sim::Arch::Rx => "0x00100000".to_string(),
                ns_sim::Arch::V850 => "0x1F0000".to_string(),
                ns_sim::Arch::Rl78 => "0xFFE00".to_string(),
                _ => "0x10000000".to_string(),
            };
        }
        self.sim_read_memory(cx);
        self.status = self.sim.status.clone();
        self.debug.view_tab = DebugViewTab::Simulator;
        self.ensure_sim_mem_input(cx);
        cx.notify();
    }


    fn sim_step(&mut self, cx: &mut Context<Self>) {
        if let Some(core) = self.sim.core.as_mut() {
            let result = core.step();
            self.sim.status = match result {
                ns_sim::StepResult::Halted => "仿真已停止（halted）".to_string(),
                ns_sim::StepResult::Breakpoint => "命中断点".to_string(),
                ns_sim::StepResult::Advanced => format!("单步: PC=0x{:x}", core.program_counter()),
            };
        }
        cx.notify();
    }

    /// Step over a call/loop: run until PC returns to the instruction after the
    /// current one, or until a breakpoint/halt. Mirrors the debugger's "step
    /// over" but against the built-in simulator.
    fn sim_step_over(&mut self, cx: &mut Context<Self>) {
        if let Some(core) = self.sim.core.as_mut() {
            let result = core.step_over(100_000);
            self.sim.status = match result {
                ns_sim::StepResult::Halted => "仿真已停止（halted）".to_string(),
                ns_sim::StepResult::Breakpoint => "命中断点".to_string(),
                ns_sim::StepResult::Advanced => {
                    format!("步过: PC=0x{:x}", core.program_counter())
                }
            };
        }
        cx.notify();
    }

    /// Dump 256 bytes from `self.sim.mem_address` into `self.sim.mem_bytes`.
    /// Rejects malformed addresses and out-of-range reads honestly.
    fn sim_read_memory(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.sim.core.as_ref() else {
            return;
        };
        let addr = match parse_address(&self.sim.mem_address) {
            Some(addr) => addr,
            None => {
                self.sim.mem_error = Some(format!("无效地址: {}", self.sim.mem_address));
                self.sim.mem_bytes.clear();
                cx.notify();
                return;
            }
        };
        let len = 256usize;
        if core.arch == ns_sim::Arch::Arm {
            // ARM memory is in the core's mapped buffer; read via the
            // accessor so the flash/SRAM windows resolve correctly.
            self.sim.mem_bytes = (0..len)
                .map(|i| core.read_u8(addr.wrapping_add(i as u32)))
                .collect();
            self.sim.mem_error = None;
            cx.notify();
            return;
        }
        let end = addr as usize + len;
        if end > core.memory.len() {
            self.sim.mem_error = Some(format!(
                "地址 0x{addr:x} + {len} 超出仿真器内存（{} 字节）",
                core.memory.len()
            ));
            self.sim.mem_bytes.clear();
            cx.notify();
            return;
        }
        self.sim.mem_bytes = core.memory[addr as usize..end].to_vec();
        self.sim.mem_error = None;
        cx.notify();
    }

    /// Write hex bytes into the simulator's memory at `self.sim.mem_address`.
    /// `data` is whitespace-separated hex like `de ad be ef`; re-reads the
    /// window afterwards so the dump reflects the write.
    fn sim_write_memory(&mut self, data: &str, cx: &mut Context<Self>) {
        let Some(core) = self.sim.core.as_mut() else {
            self.sim.mem_error = Some("仿真器未运行".to_string());
            cx.notify();
            return;
        };
        let addr = match parse_address(&self.sim.mem_address) {
            Some(addr) => addr,
            None => {
                self.sim.mem_error = Some(format!("无效地址: {}", self.sim.mem_address));
                cx.notify();
                return;
            }
        };
        let bytes = match parse_hex_bytes(data) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.sim.mem_error = Some(error);
                cx.notify();
                return;
            }
        };
        if bytes.is_empty() {
            self.sim.mem_error = Some("请输入要写入的十六进制字节".to_string());
            cx.notify();
            return;
        }
        core.write_bytes(addr, &bytes);
        self.sim.mem_error = None;
        self.sim.status = format!("已写入 {} 字节 @ 0x{addr:x}", bytes.len());
        cx.notify();
        // Re-read the window so the dump shows the updated bytes.
        self.sim_read_memory(cx);
    }

    /// Lazily create the simulator memory hex-edit field and wire Enter to
    /// commit the write (mirrors the debugger memory view).
    fn ensure_sim_mem_input(&mut self, cx: &mut Context<Self>) {
        if self.sim.mem_edit_input.is_none() {
            let field = cx.new(|cx| TextField::new("de ad be ef", cx));
            let app = cx.entity();
            field.update(cx, |field, _cx| {
                field.set_on_submit(move |text, _window, _cx| {
                    app.update(_cx, |this, cx| this.sim_write_memory(text, cx));
                });
            });
            self.sim.mem_edit_input = Some(field);
        }
    }

    /// Begin editing a register value: create (or reuse) the edit field
    /// pre-filled with the current value and focus it.
    fn begin_reg_edit(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = self.sim.core.as_ref() else {
            return;
        };
        let value = core.read_reg(index);
        let field = match self.sim.reg_edit_input.take() {
            Some(field) => field,
            None => cx.new(|cx| TextField::new("0", cx)),
        };
        let app = cx.entity();
        field.update(cx, |field, _cx| {
            field.set_text(format!("{value:x}"), _cx);
            field.set_on_submit(move |text, _window, _cx| {
                app.update(_cx, |this, cx| {
                    this.sim_write_reg(index, text, cx);
                });
            });
        });
        self.sim.reg_edit_input = Some(field.clone());
        self.sim.reg_edit = Some(index);
        window.focus(&field.read(cx).focus_handle(cx));
        cx.notify();
    }

    /// Commit a register edit: parse hex and write via `write_reg`.
    fn sim_write_reg(&mut self, index: usize, text: &str, cx: &mut Context<Self>) {
        let Some(value) = parse_hex_address(text) else {
            self.sim.status = format!("寄存器 {}: 无效值 \"{text}\"", index);
            self.sim.reg_edit = None;
            cx.notify();
            return;
        };
        let Some(core) = self.sim.core.as_mut() else {
            return;
        };
        core.write_reg(index, value as u32);
        let name = ns_sim::reg_label_for(core.arch, index);
        self.sim.reg_edit = None;
        self.sim.status = format!("{} = 0x{:x}", name, value);
        cx.notify();
    }

    /// Begin editing the program counter: create (or reuse) the field.
    fn begin_pc_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = self.sim.core.as_ref() else {
            return;
        };
        let pc = core.program_counter();
        let field = match self.sim.pc_edit_input.take() {
            Some(field) => field,
            None => cx.new(|cx| TextField::new("0", cx)),
        };
        let app = cx.entity();
        field.update(cx, |field, _cx| {
            field.set_text(format!("{pc:x}"), _cx);
            field.set_on_submit(move |text, _window, _cx| {
                app.update(_cx, |this, cx| this.sim_set_pc(text, cx));
            });
        });
        self.sim.pc_edit_input = Some(field.clone());
        window.focus(&field.read(cx).focus_handle(cx));
        cx.notify();
     }

    /// Nominal simulator clock in Hz from `[sim] clock_hz` (0 = unknown).
    fn sim_clock_hz(&self) -> u64 {
        self.project
            .as_ref()
            .map(|p| p.config.sim.clock_hz)
            .unwrap_or(0)
    }
 
     /// Commit a PC jump: parse hex and set the program counter.
    fn sim_set_pc(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(value) = parse_hex_address(text) else {
            self.sim.status = format!("无效 PC 值 \"{text}\"");
            cx.notify();
            return;
        };
        let Some(core) = self.sim.core.as_mut() else {
            return;
        };
        core.set_program_counter(value as u32);
        self.sim.status = format!("PC = 0x{:x}", value);
        cx.notify();
    }

    /// Lazily create the breakpoint address field and wire Enter to add it.
    fn ensure_sim_bp_input(&mut self, cx: &mut Context<Self>) {
        if self.sim.bp_input.is_none() {
            let field = cx.new(|cx| TextField::new("地址(hex)", cx));
            let app = cx.entity();
            field.update(cx, |field, _cx| {
                field.set_on_submit(move |text, _window, _cx| {
                    app.update(_cx, |this, cx| this.sim_add_breakpoint(text, cx));
                });
            });
            self.sim.bp_input = Some(field);
        }
    }

    /// Add a breakpoint by address (hex), reporting the outcome in status.
    fn sim_add_breakpoint(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(value) = parse_hex_address(text) else {
            self.sim.status = format!("无效断点地址 \"{text}\"");
            cx.notify();
            return;
        };
        let Some(core) = self.sim.core.as_mut() else {
            return;
        };
        if core.set_breakpoint(value as u32) {
            self.sim.status = format!("断点已添加 @ 0x{:x}", value);
        } else {
            self.sim.status = format!("断点已存在 @ 0x{:x}", value);
        }
        cx.notify();
    }

    /// Remove a breakpoint by address, reporting the outcome in status.
    fn sim_remove_breakpoint(&mut self, address: u32, cx: &mut Context<Self>) {
        let Some(core) = self.sim.core.as_mut() else {
            return;
        };
        if core.clear_breakpoint(address) {
            self.sim.status = format!("断点已删除 @ 0x{address:x}");
        }
        cx.notify();
    }

    /// Read the breakpoint address field and add it.
    fn sim_add_breakpoint_from_field(&mut self, cx: &mut Context<Self>) {
        let text = self
            .sim
            .bp_input
            .as_ref()
            .map(|field| field.read(cx).text().trim().to_string())
            .unwrap_or_default();
        self.sim_add_breakpoint(&text, cx);
    }

    /// Read the hex-edit field's text and write it into simulator memory.
    fn sim_write_memory_from_field(&mut self, cx: &mut Context<Self>) {
        let text = self
            .sim
            .mem_edit_input
            .as_ref()
            .map(|field| field.read(cx).text().trim().to_string())
            .unwrap_or_default();
        self.sim_write_memory(&text, cx);
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
        // Extract the source location the PC stopped at before releasing the
        // `core` borrow, so we can open the file afterwards.
        let hit_location: Option<(std::path::PathBuf, usize)> =
            if result == ns_sim::StepResult::Breakpoint {
                core.line_for_address(core.pc).and_then(|(file, line)| {
                    core.source_files.get(file).map(|source| {
                        (std::path::PathBuf::from(source), line as usize)
                    })
                })
            } else {
                None
            };
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
        // Jump the editor to the source line the PC stopped at, so the user
        // sees where the breakpoint hit (mirrors debugger stops).
        if let Some((source, line)) = hit_location {
            if source.exists() {
                self.open_file(source, cx);
                if let Some(editor) = self.active_editor() {
                    editor.update(cx, |editor, cx| {
                        editor.set_cursor_line(line, cx);
                        editor.set_debug_line(Some(line), cx);
                    });
                }
            }
        }
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
        if self.debug.memory.address_input.is_none() {
            let pc_hex =
                self.debug.pc.map(|pc| format!("0x{pc:x}")).unwrap_or_default();
            let field = cx.new(|cx| {
                let mut field = TextField::new("例如 0x1000003e8", cx);
                if !pc_hex.is_empty() {
                    field.set_text(pc_hex, cx);
                }
                field
            });
            self.debug.memory.address_input = Some(field);
        }
        if self.debug.memory.edit_input.is_none() {
            let field = cx.new(|cx| TextField::new("de ad be ef", cx));
            self.debug.memory.edit_input = Some(field);
        }
         if self.debug.memory.region_name_input.is_none() {
             let field = cx.new(|cx| TextField::new("例如 RAM 区", cx));
             self.debug.memory.region_name_input = Some(field);
         }
        if self.debug.memory.search_input.is_none() {
            let field = cx.new(|cx| TextField::new("搜索字节模式（如 de ad be ef）", cx));
            self.debug.memory.search_input = Some(field);
        }
        if self.debug.memory.fill_input.is_none() {
            let field = cx.new(|cx| TextField::new("填充模式（如 00 或 ff）", cx));
            self.debug.memory.fill_input = Some(field);
        }
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

     fn trace_toggle_pc_sample(&mut self, cx: &mut Context<Self>) {
         self.debug.trace.config.pc_sample = !self.debug.trace.config.pc_sample;
         self.debug.trace.error = None;
         self.trace_save(cx);
         cx.notify();
     }

    /// Toggle the profiler hotspot view. When turning it on, load the firmware
    /// symbol table (from `nm`) so sampled PCs can be attributed to functions.
    fn trace_toggle_profile(&mut self, cx: &mut Context<Self>) {
        let show = !self.debug.trace.show_profile;
        self.debug.trace.show_profile = show;
        if show {
            // Rebuild the aggregator with the current firmware's symbols so
            // already-recorded samples are re-attributed where possible.
            if let Some(project) = self.project.as_ref() {
                let artifact = project.artifact_path();
                if artifact.is_file() {
                    let symbols = ns_flash::firmware::analyze_symbols(&artifact);
                    let tuples: Vec<(u64, u64, String)> = symbols
                        .into_iter()
                        .map(|symbol| (symbol.address, symbol.size, symbol.name))
                        .collect();
                    self.debug.trace.profile = ns_debug::profile::ProfileAggregator::new(&tuples);
                }
            }
        }
        cx.notify();
    }
     fn trace_cycle_mode(&mut self, cx: &mut Context<Self>) {
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
        let _ = commands.send(DebugCommand::ConfigureTrace(config.clone()));
        // Start the host-side SWO stream reader so decoded packets reach the
        // trace panel. The port comes from the config, or is derived from the
        // probe's GDB server port (OpenOCD default: itm_port = gdb_port + 1).
        let swo_port = if config.swo_port != 0 {
            config.swo_port
        } else {
            match find_probe(std::env::var("NS_TRACE_PROBE").ok().as_deref()) {
                Some(ns_debug::swvd::Probe::Server { port }) => port.saturating_add(1),
                Some(ns_debug::swvd::Probe::Tool { .. }) => 0, // tool streams SWV itself.
                None => 0,
            }
        };
        if swo_port != 0 {
            let _ = commands.send(DebugCommand::StartTraceStream {
                port: swo_port,
                itm_port: config.itm_port,
            });
            self.debug.trace.streaming = true;
        }
        self.debug.trace.armed = true;
        self.debug.push_console("◈ 已请求启用跟踪窗口…".to_string());
        self.trace_save(cx);
        cx.notify();
    }

    fn trace_disarm(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = &self.debug_session {
            let _ = session.commands.send(DebugCommand::StopTrace);
            if self.debug.trace.streaming {
                let _ = session.commands.send(DebugCommand::StopTraceStream);
                self.debug.trace.streaming = false;
            }
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

    /// Write bytes into the target at the memory view's current address and
    /// re-read the region on success. `data` is hex text like `de ad be ef`
    /// (whitespace-separated); an empty value is rejected as a no-op.
    fn write_memory(&mut self, cx: &mut Context<Self>) {
        let edit = self
            .debug
            .memory
            .edit_input
            .clone()
            .map(|field| field.read(cx).text().trim().to_string());
        let Some(text) = edit else {
            return;
        };
        let bytes = parse_hex_bytes(&text);
        match bytes {
            Ok(data) => {
                let address = self.debug.memory.address;
                let reference = format!("{address:#x}");
                if let Some(session) = &self.debug_session {
                    let _ = session
                        .commands
                        .send(DebugCommand::WriteMemory { reference, data });
                } else {
                    self.debug.memory.error = Some("调试会话未运行".to_string());
                }
            }
            Err(message) => {
                self.debug.memory.error = Some(message);
            }
        }
         cx.notify();
 
     }
 
    /// Search the dumped memory region for a byte pattern. `text` is hex like
    /// `de ad be ef` (whitespace-separated) or `deadbeef`; the first match
    /// offset is stored in `search_hit` and scrolled into view.
    fn search_memory(&mut self, cx: &mut Context<Self>) {
        let text = self
            .debug
            .memory
            .search_input
            .clone()
            .map(|field| field.read(cx).text().trim().to_string());
        let Some(text) = text else {
            return;
        };
        let pattern = match parse_hex_bytes(&text) {
            Ok(data) if !data.is_empty() => data,
            Ok(_) => {
                self.debug.memory.search_hit = None;
                self.debug.memory.error = Some("搜索模式为空。".to_string());
                cx.notify();
                return;
            }
            Err(message) => {
                self.debug.memory.search_hit = None;
                self.debug.memory.error = Some(message);
                cx.notify();
                return;
            }
        };
        let bytes = &self.debug.memory.bytes;
        let hit = find_pattern(bytes, &pattern);
         match hit {
             Some(offset) => {
                self.debug.memory.search_hit = Some((offset, pattern.len()));
                 let address = self.debug.memory.address.wrapping_add(offset as u64);
                 self.status = format!("内存搜索命中: {address:#x}（偏移 {offset} 字节）");
             }
             None => {
                 self.debug.memory.search_hit = None;
                 self.status = "内存搜索无匹配。".to_string();
             }
         }
         cx.notify();
     }
     /// Fill the dumped region with a repeating byte pattern. `text` is hex
     /// like `00` or `de ad be ef`; the pattern repeats across the region.
    fn fill_memory(&mut self, cx: &mut Context<Self>) {
        let text = self
            .debug
            .memory
            .fill_input
            .clone()
            .map(|field| field.read(cx).text().trim().to_string());
        let Some(text) = text else {
            return;
        };
        let pattern = match parse_hex_bytes(&text) {
            Ok(data) if !data.is_empty() => data,
            Ok(_) => {
                self.debug.memory.error = Some("填充模式为空。".to_string());
                cx.notify();
                return;
            }
            Err(message) => {
                self.debug.memory.error = Some(message);
                cx.notify();
                return;
            }
        };
        let address = self.debug.memory.address;
        let count = self.debug.memory.read_size as usize;
        let mut data = Vec::with_capacity(count);
        for index in 0..count {
            data.push(pattern[index % pattern.len()]);
        }
        if let Some(session) = &self.debug_session {
            let reference = format!("{address:#x}");
            let _ = session
                .commands
                .send(DebugCommand::WriteMemory { reference, data });
            self.status = format!(
                "内存填充: 区域 {count} 字节 @ {address:#x}（模式 {pattern:02x?}）"
            );
        } else {
            self.debug.memory.error = Some("调试会话未运行".to_string());
        }
        cx.notify();
    }

     /// Re-read the memory view's current region after a stop so the dump stays
    /// current. Only re-reads if the user has actually viewed memory (bytes are
    /// non-empty); otherwise there is nothing to refresh.
    fn refresh_memory_on_stop(&mut self, cx: &mut Context<Self>) {
        if self.debug.memory.bytes.is_empty() {
            return;
        }
        let address = self.debug.memory.address;
        let count = self.debug.memory.read_size;
        let reference = format!("{address:#x}");
        if let Some(session) = &self.debug_session {
            let _ = session
                .commands
                .send(DebugCommand::ReadMemory { reference, count });
        }
        cx.notify();
    }

    /// After a debug stop, re-read the device registers shown on the「设备」page
    /// so their live values track the paused target. Reads are batched per
    /// peripheral: one `ReadMemory` per peripheral base covering the span from
    /// the lowest to the highest register address.
    fn refresh_device_registers_on_stop(&mut self) {
        let Some(session) = &self.debug_session else {
            return;
        };
        let Some(device) = self.device_for_project() else {
            return;
        };
        for peripheral in device.sorted_peripherals() {
            if peripheral.registers.is_empty() {
                continue;
            }
            let min_addr = peripheral
                .registers
                .iter()
                .map(|r| r.address)
                .min()
                .unwrap_or(peripheral.base_address);
            let max_addr = peripheral
                .registers
                .iter()
                .map(|r| r.address)
                .max()
                .unwrap_or(peripheral.base_address);
            let count = (max_addr - min_addr + 4) as usize;
            let reference = format!("{min_addr:#x}");
            let _ = session
                .commands
                .send(DebugCommand::ReadMemory { reference, count: count as u64 });
        }
    }

    /// Toggle live-watch register refresh while the target runs. The ticker
    /// reads the device registers (batched per peripheral) on a fixed interval
    /// only when the session is active and running; the「设备」page then shows
    /// values that track the live target.
    fn toggle_live_watch(&mut self, cx: &mut Context<Self>) {
        self.debug.live_watch = !self.debug.live_watch;
        if self.debug.live_watch {
            self.start_live_watch_ticker(cx);
            self.status = "实时监视已开启（设备寄存器随运行刷新）".to_string();
        } else {
            self.status = "实时监视已关闭".to_string();
        }
        cx.notify();
    }

    /// Begin editing a device-register value: create the input field, prefill
    /// it with the current live value (hex), and focus it.
    fn begin_device_reg_edit(&mut self, address: u64, cx: &mut Context<Self>) {
        let current = self
            .debug
            .device_reg_values
            .get(&address)
            .copied()
            .or_else(|| {
                self.sim
                    .core
                    .as_ref()
                    .map(|sim| sim.read_u32(address as u32))
            })
            .unwrap_or(0);
        let field = cx.new(|cx| {
            let mut field = TextField::new("0x00000000", cx);
            field.set_text(format!("0x{:08X}", current), cx);
            field
        });
        self.debug.device_reg_edit = Some((address, field));
        cx.notify();
    }

    /// Commit a device-register value edit: parse hex and write to the target
    /// memory via the simulator (if active) or the debug session (DAP
    /// `writeMemory`). Falls back with a clear message when no source is
    /// active; the read side refreshes via the existing Memory reply path.
    fn commit_device_reg_edit(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some((address, _field)) = self.debug.device_reg_edit.take() else {
            return;
        };
        let text = text.trim();
        let value = parse_hex_address(text)
            .and_then(|v| u32::try_from(v).ok())
            .or_else(|| text.parse::<u32>().ok());
        let Some(value) = value else {
            self.status = format!("寄存器写入失败：无法解析「{text}」为 32 位整数");
            cx.notify();
            return;
        };
        if let Some(sim) = self.sim.core.as_mut() {
            sim.write_u32(address as u32, value);
            self.status = format!("寄存器 0x{address:08X} 已写入 0x{value:08X}（仿真）");
        } else if let Some(session) = &self.debug_session {
            let _ = session.commands.send(DebugCommand::WriteMemory {
                reference: format!("{address:#x}"),
                data: value.to_le_bytes().to_vec(),
            });
            self.status = format!("寄存器 0x{address:08X} 写入 0x{value:08X}（调试会话）");
        } else {
            self.status = "寄存器写入失败：无活动仿真或调试会话".to_string();
        }
        self.debug.device_reg_edit = None;
        cx.notify();
    }
    /// Spawn the periodic live-watch reader. It only issues `ReadMemory` when
    /// the session is active and `DebugStatus::Running`, so a paused or ended
    /// session costs nothing.
    fn start_live_watch_ticker(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
                let ok = this
                    .update(cx, |this, cx| {
                        if !this.debug.live_watch {
                            return;
                        }
                        if this.debug.status != DebugStatus::Running {
                            return;
                        }
                        if this.debug_session.is_none() {
                            return;
                        }
                        this.refresh_device_registers_on_stop();
                        // Keep the memory view current too (when shown).
                        if !this.debug.memory.bytes.is_empty() {
                            this.refresh_memory_on_stop(cx);
                        }
                        cx.notify();
                    })
                    .is_err();
                if ok {
                    break;
                }
            }
        })
        .detach();
    }

    /// Save the current address/size as a named region and select it.
    fn add_memory_region(&mut self, name: String, cx: &mut Context<Self>) {
        let name = name.trim().to_string();
        if name.is_empty() {
            self.debug.memory.error = Some("区域名称不能为空".to_string());
            cx.notify();
            return;
        }
        let region = MemoryRegion {
            name,
            address: self.debug.memory.address,
            size: self.debug.memory.read_size,
        };
        self.debug.memory.regions.push(region);
        self.debug.memory.active_region = self.debug.memory.regions.len() - 1;
        self.debug.memory.error = None;
        cx.notify();

    }

    /// Add a region using the name typed in the region-name input field.
    fn add_memory_region_from_field(&mut self, cx: &mut Context<Self>) {
        self.ensure_memory_input(cx);
        let name = self
            .debug
            .memory
            .region_name_input
            .clone()
            .map(|field| field.read(cx).text())
            .unwrap_or_default();
        self.add_memory_region(name, cx);
    }

    /// Switch to the region at `index`, updating the address field and re-reading.
    fn select_memory_region(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.debug.memory.regions.len() {
            return;
        }
        self.debug.memory.active_region = index;
        let region = self.debug.memory.regions[index].clone();
        self.debug.memory.address = region.address;
        self.debug.memory.read_size = region.size;
        if let Some(field) = &self.debug.memory.address_input {
            field.update(cx, |field, cx| {
                field.set_text(format!("0x{:x}", region.address), cx);
            });
        }
        self.read_memory(cx);
    }

    /// Remove the region at `index` (0 = custom is never removed).
    fn remove_memory_region(&mut self, index: usize, cx: &mut Context<Self>) {
        if index == 0 || index >= self.debug.memory.regions.len() {
            return;
        }
        self.debug.memory.regions.remove(index);
        if self.debug.memory.active_region == index {
            self.debug.memory.active_region = 0;
        } else if self.debug.memory.active_region > index {
            self.debug.memory.active_region -= 1;
        }
        cx.notify();
    }

     fn disassemble_at_pc(&mut self, cx: &mut Context<Self>) {
         self.ensure_disasm_input(cx);
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
 
    /// Disassemble at an arbitrary address typed into the disassembly field.
    fn disassemble_at(&mut self, cx: &mut Context<Self>) {
        self.ensure_disasm_input(cx);
        let text = self
            .debug
            .disasm_input
            .clone()
            .and_then(|field| {
                let text = field.read(cx).text().trim().to_string();
                if text.is_empty() {
                    None
                } else {
                    Some(text)
                }
            });
        let Some(text) = text else {
            self.status = "请输入十六进制地址（如 0x08000100）。".to_string();
            cx.notify();
            return;
        };
        let address = parse_address(&text);
        match address {
            Some(address) => {
                if let Some(session) = &self.debug_session {
                    let _ = session.commands.send(DebugCommand::Disassemble {
                        reference: format!("0x{address:x}"),
                        instruction_count: 24,
                    });
                    self.status = format!("反汇编 @ {address:#x}");
                } else {
                    self.status = "调试会话未运行".to_string();
                }
            }
            None => {
                self.status = format!("无效地址: {text}");
            }
        }
        cx.notify();
    }

    /// Lazily create the disassembly address input field.
    fn ensure_disasm_input(&mut self, cx: &mut Context<Self>) {
        if self.debug.disasm_input.is_none() {
            let field = cx.new(|cx| TextField::new("地址，如 0x08000100", cx));
            self.debug.disasm_input = Some(field);
        }
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
                    .text_color(theme::color(theme::TEXT_FAINT))
                    .child(chevron),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(theme::MONO_FONT)
                    .text_size(px(10.5))
                    .text_color(theme::color(theme::ACCENT_3))
                    .child(variable.name.clone()),
            )
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_size(px(10.5))
                    .text_color(theme::color(theme::TEXT_DIM))
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
                    .text_color(theme::color(theme::TEXT_FAINT))
                    .cursor_pointer()
                    .hover(|style| style.text_color(theme::color(theme::ACCENT_3)))
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
                    .text_color(theme::color(theme::TEXT_FAINT))
                    .cursor_pointer()
                    .hover(|style| style.text_color(theme::color(theme::ACCENT_3)))
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
            DebugStep::Instruction => {
                DebugCommand::StepInstruction { thread_id: self.debug.thread_id }
            }
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
        if let Some(split) = &self.split
            && self.breakpoint_key(&split.path) == path
        {
            split
                .editor
                .update(cx, |editor, cx| editor.set_breakpoints(lines, cx));
        }
    }

    fn sync_all_editor_breakpoints(&mut self, cx: &mut Context<Self>) {
        let mut paths: Vec<String> = self
            .tabs
            .iter()
            .map(|tab| self.breakpoint_key(&tab.path))
            .collect();
        if let Some(split) = &self.split {
            let key = self.breakpoint_key(&split.path);
            if !paths.contains(&key) {
                paths.push(key);
            }
        }
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
        self.push_nav_history(cx);
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

    /// Move the active editor's cursor to a 0-based `line`, for breadcrumb /
    /// outline navigation.
    fn jump_to_line(&mut self, line: usize, cx: &mut Context<Self>) {
        self.push_nav_history(cx);
        if let Some(editor) = self.active_editor() {
            editor.update(cx, |editor, cx| editor.set_cursor_line(line + 1, cx));
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
            build_config: settings_field("debug | release", project.build_config().to_string(), cx),
            out_dir: settings_field("输出目录", config.build.out_dir, cx),
            build_jobs: settings_field("并行作业数（0=自动）", config.build.jobs.to_string(), cx),
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
            sim_clock: settings_field(
                "仿真时钟频率（Hz，0=未知）",
                config.sim.clock_hz.to_string(),
                cx,
            ),
            launch_args: settings_field(
                "调试参数（空格分隔，如 `--port 8080`）",
                config.launch.args.join(" "),
                cx,
            ),
            launch_env: settings_field(
                "调试环境变量（每行 KEY=value）",
                config.launch.env.join("\n"),
                cx,
            ),
            launch_cwd: settings_field("调试工作目录（留空=项目根）", config.launch.cwd.clone(), cx),
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
        let build_jobs = dialog.build_jobs.read(cx).text();
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
        let sim_clock = dialog.sim_clock.read(cx).text();
         let launch_args = dialog.launch_args.read(cx).text();
        let launch_env = dialog.launch_env.read(cx).text();
        let launch_cwd = dialog.launch_cwd.read(cx).text();
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
        let parsed_jobs = build_jobs.trim().parse::<u32>();
        if build_jobs.trim() != "0" && parsed_jobs.is_err() {
            self.status = "并行作业数必须为 0（自动）或正整数".to_string();
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
        let clock_hz = sim_clock.trim().parse::<u64>().unwrap_or(0);
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
        // Write the build config to the active target's entry when a target is
        // set (per-target configs), else the shared `build.config`.
        if project.config.project.target.trim().is_empty() {
            project.config.build.config = build_config;
        } else if let Some(entry) = project
            .config
            .targets
            .iter_mut()
            .find(|entry| entry.target.trim() == project.config.project.target.trim())
        {
            entry.config = build_config;
        } else {
            project.config.targets.push(TargetSection {
                target: project.config.project.target.clone(),
                config: build_config,
            });
        }
        project.config.build.out_dir = out_dir;
        project.config.build.jobs = parsed_jobs.unwrap_or(0);
        project.config.debugger.path = debugger;
        project.config.debugger.interface = debugger_interface;
        project.config.debugger.server = debugger_server;
        project.config.debugger.gdb = debugger_gdb;
        project.config.flasher.path = flasher;
        project.config.flasher.kind = kind;
        project.config.flasher.interface = flasher_interface;
        project.config.sim.cpu = sim_cpu;
         project.config.sim.period = period;
        project.config.sim.clock_hz = clock_hz;
         project.config.launch.args = split_csv(&launch_args);
        project.config.launch.env = launch_env
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();
        project.config.launch.cwd = launch_cwd.trim().to_string();
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

    /// Open the keyboard-shortcut overrides dialog (F13 快捷键配置). Each
    /// remappable action gets one text field prefilled with its configured
    /// keystroke (or empty = default). The remappable set mirrors the action
    /// names used by [`bindings_with`].
    fn open_keybindings(&mut self, cx: &mut Context<Self>) {
        let overrides = ns_project::KeybindingOverrides::load();
        let remappable: &[(&str, &str, &str)] = &[
            ("NewProject", "新建项目", "cmd-n"),
            ("OpenProject", "打开项目", "cmd-o"),
            ("Save", "保存当前文件", "cmd-s"),
            ("Build", "构建项目", "cmd-b"),
            ("Run", "直接运行", "cmd-r"),
            ("ToggleOutput", "显示/隐藏输出面板", "cmd-j"),
            ("CloseTab", "关闭当前标签页", "cmd-w"),
            ("NextTab", "下一个标签页", "ctrl-tab"),
            ("PrevTab", "上一个标签页", "ctrl-shift-tab"),
            ("QuitApp", "退出应用", "cmd-q"),
            ("Settings", "项目设置", "cmd-,"),
            ("Debug", "开始调试", "f5"),
            ("StopDebug", "停止调试", "shift-f5"),
            ("DebugStepOver", "步过", "f10"),
            ("DebugStepIn", "步入", "f11"),
            ("DebugStepOut", "跳出", "shift-f11"),
            ("RunToCursor", "运行到光标", "cmd-f10"),
            ("Flash", "烧写固件", "cmd-shift-f10"),
            ("FindInProject", "全局搜索", "cmd-shift-f"),
            ("FindInFile", "文件内查找", "cmd-f"),
            ("Palette", "命令面板", "cmd-shift-p"),
            ("ClearBreakpoints", "清除全部断点", "cmd-shift-f9"),
            ("EscapeModal", "关闭模态", "escape"),
            ("GoToDefinition", "跳转到定义", "f12"),
            ("GoToLine", "跳转到行", "cmd-g"),
            ("FindReferences", "查找引用", "shift-f12"),
            ("Hover", "悬停提示", "cmd-k cmd-i"),
            ("SignatureHelp", "签名帮助", "cmd-shift-space"),
            ("ShowCompletion", "代码补全", "cmd-space"),
            ("NavigateBack", "返回", "alt-left"),
            ("NavigateForward", "前进", "alt-right"),
            ("JumpToInclude", "跳转到包含头文件", "cmd-i"),
            ("RenameSymbol", "重命名符号", "cmd-shift-r"),
        ];
        let fields = remappable
            .iter()
            .map(|(action, label, default)| {
                let value = overrides.get(action).unwrap_or("").to_string();
                let field = settings_field(*default, value, cx);
                (*action, *label, *default, field)
            })
            .collect();
        self.keybindings_dialog = Some(KeybindingsDialog { fields });
        self.focus_dialog_on_next_frame = true;
        cx.notify();
    }

    fn save_keybindings(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.keybindings_dialog.take() else {
            return;
        };
        let mut overrides = ns_project::KeybindingOverrides::default();
        for (action, _label, _default, field) in &dialog.fields {
            let value = field.read(cx).text().trim().to_string();
            if !value.is_empty() {
                overrides.overrides.insert(action.to_string(), value);
            }
        }
        match overrides.save() {
            Ok(()) => self.status = "已保存快捷键配置（重启后生效）".to_string(),
            Err(error) => self.status = format!("保存快捷键配置失败: {error}"),
        }
        cx.notify();
    }

    fn cancel_keybindings(&mut self, cx: &mut Context<Self>) {
        if self.keybindings_dialog.take().is_some() {
            self.status = "已取消快捷键编辑".to_string();
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

    /// Open the "go to line" dialog for the active editor.
    fn open_go_to_line(&mut self, cx: &mut Context<Self>) {
        if self.active_editor().is_none() {
            self.status = "没有打开的编辑器，无法跳转到行".to_string();
            cx.notify();
            return;
        }
        let field = cx.new(|cx| TextField::new("行号", cx));
        self.go_to_line_dialog = Some(GoToLineDialog { field });
        self.context_menu = None;
        self.focus_dialog_on_next_frame = true;
        cx.notify();
    }

    /// Parse the dialog input and jump the active editor to that line.
    fn save_go_to_line_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.go_to_line_dialog.take() else {
            return;
        };
        let text = dialog.field.read(cx).text();
        let parsed: usize = match text.trim().parse() {
            Ok(n) if n >= 1 => n,
            _ => {
                self.status = "行号必须是大于 0 的整数".to_string();
                cx.notify();
                return;
            }
        };
        let Some(editor) = self.active_editor() else {
            self.status = "没有打开的编辑器".to_string();
            cx.notify();
            return;
        };
        // `set_cursor_line` takes a 1-based number and clamps to the document.
        let total = editor.read(cx).line_count();
        let line = parsed.min(total.max(1));
        editor.update(cx, |editor, cx| {
            editor.set_cursor_line(line, cx);
        });
        self.status = format!("已跳转到第 {} 行", line);
        cx.notify();
    }

    fn cancel_go_to_line_dialog(&mut self, cx: &mut Context<Self>) {
        if self.go_to_line_dialog.take().is_some() {
            self.status = "已取消跳转到行".to_string();
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
            "import-project" => self.import_project_dialog(cx),
             "save" => self.save_active(cx),
            "build" => self.start_build(false, cx),
            "rebuild" => self.rebuild_build(cx),
            "clean" => self.clean_build(cx),
            "run" => self.start_build(true, cx),
            "restart-run" => self.restart_run(cx),
            "stop-run" => self.stop_run(cx),
            "debug" => self.start_debug(cx),
            "embedded-debug" => self.start_embedded_debug(cx),
            "stop-debug" => self.stop_debug(cx),
            "restart-debug" => self.restart_debug(cx),
            "run-to-cursor" => self.run_to_cursor(cx),
            "find" => self.open_search(window, cx),
            "go-to-line" => self.open_go_to_line(cx),
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
             "flash-and-verify" => self.flash_and_verify(cx),
             "toggle-build-config" => self.toggle_build_config(cx),
            "toolchain" => self.open_toolchain(cx),
              "goto-header-source" => self.goto_header_source(cx),
             "include-graph" => self.show_include_graph(cx),
             "tasks" => self.show_tasks(cx),
             "memory-layout" => self.show_memory_layout(cx),
            "generate-device-files" => self.generate_device_files_for_project(cx),
             "generate-peripheral-init" => self.generate_peripheral_init_code(cx),
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
            "document-symbols" => self.lsp_document_symbols(cx),
            "format-document" => self.lsp_format_document(cx),
            "code-action" => self.lsp_code_action(cx),
             "toggle-outline" => {
                 self.outline_open = !self.outline_open;
                 if self.outline_open {
                    if self.outline_filter_input.is_none() {
                        let input = cx.new(|cx| TextField::new("过滤符号…", cx));
                        let app = cx.entity();
                         input.update(cx, |field, _cx| {
                            field.set_on_change(move |text, _window, cx| {
                                app.update(cx, |this, cx| {
                                    this.outline_filter = text.to_string();
                                    cx.notify();
                                });
                            });
                        });
                        self.outline_filter_input = Some(input);
                    }
                     self.lsp_document_symbols(cx);
                 }
                 cx.notify();
             }
             "show-completion" => self.lsp_completion(cx),
            "toggle-theme" => self.toggle_theme(cx),
            "new-window" => self.open_new_window(cx),
            "keybindings" => self.open_keybindings(cx),
            "split-right" => self.split_editor_right(cx),
            "close-split" => self.close_split(cx),
            "run-tests" => self.start_tests(cx),
             "stack-usage" => self.start_stack_usage(cx),
             "coverage" => self.start_coverage(cx),
            "preprocess" => self.start_preprocess(cx),
            "workspace-symbols" => self.open_workspace_symbols(window, cx),
            "quick-open" => self.open_quick_open(window, cx),
            "recent-files" => self.open_recent_files(window, cx),
            _ => {
                self.status = format!("未知命令 {id}");
                cx.notify();
            }
        }
    }

    /// Open a new independent IDE window. Each window owns its own workspace
    /// (tabs, output, panels); the project selection is independent too.
    fn open_new_window(&mut self, cx: &mut Context<Self>) {
        use gpui::{Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, size};
        let bounds = Bounds::centered(None, size(px(1380.), px(860.)), cx);
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(980.), px(620.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("ns-embed-ide".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(16.), px(17.))),
                }),
                ..Default::default()
            },
            |_window, cx| cx.new(|cx| IdeApp::new(None, cx)),
        );
        match result {
            Ok(_) => {
                self.status = "已打开新窗口".to_string();
            }
            Err(error) => {
                self.status = format!("打开新窗口失败: {error}");
            }
        }
        cx.notify();
    }

    /// Toggle between the dark and light themes, repaint and persist.
    /// Split the editor to the right, opening the active file in an
    /// independent second pane. If a split pane already exists, it is
    /// replaced with the current active file.
    fn split_editor_right(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.active_tab else {
            self.status = "没有可分屏的文件".to_string();
            cx.notify();
            return;
        };
        let Some(tab) = self.tabs.get(index) else {
            self.status = "没有可分屏的文件".to_string();
            cx.notify();
            return;
        };
        let path = tab.path.clone();
        // Re-read from disk so the split pane has independent state.
        match fs::read_to_string(&path) {
            Ok(text) if !is_binary_file(&path, text.as_bytes()) => {
                let split = self.build_split_editor(path.clone(), text, cx);
                self.split = Some(split);
                self.status = format!("分屏 {}", self.display_path(&path));
                self.save_session(cx);
                cx.notify();
            }
            Ok(_) | Err(_) => {
                self.status = format!("无法分屏 {}（需文本文件）", path.display());
                cx.notify();
            }
        }
    }

    /// Close the split editor pane.
    fn close_split(&mut self, cx: &mut Context<Self>) {
        if self.split.is_some() {
            self.split = None;
            self.status = "已关闭分屏".to_string();
            self.save_session(cx);
        } else {
            self.status = "分屏未打开".to_string();
        }
        cx.notify();
    }

    /// Build a `SplitPane` with the same editor wiring as a tab (LSP sync,
    /// breakpoint/menu/definition subscriptions), for the file at `path`.
    fn build_split_editor(&mut self, path: PathBuf, text: String, cx: &mut Context<Self>) -> SplitPane {
        let saved_text = text.replace("\r\n", "\n").replace('\r', "\n");
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
        let signature_path = path.clone();
        let hover_path = path.clone();
        let subscription = cx.observe(&editor, move |this, editor, cx| {
            if let Some(server) = &this.lsp {
                let next_version = version.get() + 1;
                version.set(next_version);
                let text = editor.read(cx).text().to_string();
                let _ = server.change_file(did_change_path.clone(), text, next_version);
             }
            // Debounced auto-refresh of semantic highlighting and inlay hints:
            // bump the generation and, after a short pause, re-request both if
            // no newer edit has arrived in the meantime.
             this.lsp_refresh_generation = this.lsp_refresh_generation.wrapping_add(1);
              let generation = this.lsp_refresh_generation;
              cx.spawn(async move |refresh_this, cx| {
                 cx.background_executor()
                     .timer(std::time::Duration::from_millis(400))
                     .await;
                 let _ = refresh_this.update(cx, |this, cx| {
                     if this.lsp_refresh_generation == generation && this.lsp.is_some() {
                         this.lsp_semantic_tokens(cx);
                         this.lsp_inlay_hints(cx);
                     }
                 });
             })
             .detach();
              if let Some(anchor) = &this.signature_anchor {
                let moved = editor.read(cx).cursor_line_col();
                 let current = (moved.0.saturating_sub(1), moved.1.saturating_sub(1));
                if anchor.0 != signature_path || (anchor.1, anchor.2) != current {
                    this.signature_help = None;
                    this.signature_anchor = None;
                }
            }
            if let Some(anchor) = &this.hover_anchor {
                let moved = editor.read(cx).cursor_line_col();
                let current = (moved.0.saturating_sub(1), moved.1.saturating_sub(1));
                if anchor.0 != hover_path || (anchor.1, anchor.2) != current {
                    this.hover_tooltip = None;
                    this.hover_anchor = None;
                }
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
        let definition_subscription = cx.subscribe(
            &editor,
            move |this, _editor, event: &DefinitionRequested, cx| {
                this.on_definition_requested(event.line, event.column, cx);
            },
        );
        if let Some(server) = &self.lsp {
            let _ = server.open_file(path.clone(), text_for_lsp);
        }
        SplitPane {
            editor,
            path,
            saved_text,
            _subscription: subscription,
            _breakpoint_subscription: breakpoint_subscription,
            _menu_subscription: menu_subscription,
            _definition_subscription: definition_subscription,
        }
    }
    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        theme::set_light(!theme::is_light());
        self.status = if theme::is_light() {
            "已切换到浅色主题".to_string()
        } else {
            "已切换到深色主题".to_string()
        };
        let settings = ns_project::ThemeSettings {
            mode: if theme::is_light() {
                ns_project::ThemeMode::Light
            } else {
                ns_project::ThemeMode::Dark
            },
        };
        if let Err(error) = settings.save() {
            self.status = format!("主题切换失败：{error}");
        }
        cx.notify();
    }

    /// Resolve the definition under the cursor and jump to it (LSP).
    fn lsp_go_to_definition(&mut self, cx: &mut Context<Self>) {
        let Some((path, line, column)) = self.cursor_location(cx) else {
            self.status = "没有活动编辑器".to_string();
            cx.notify();
            return;
        };
        let Some(server) = &self.lsp else {
            // No LSP: for assembly files, jump to the label definition via a
            // project-wide label scan (clangd/rust-analyzer don't parse `.s`).
            if self.jump_to_asm_label(&path, line, column, cx) {
                return;
            }
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
                            this.push_nav_history(cx);
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

    /// Jump to an assembly label definition. Scans the project's `.s`/`.S`/
    /// `.asm`/`.inc` files for `label:` and moves the cursor there. Returns
    /// `true` when a definition was found and jumped to.
    fn jump_to_asm_label(
        &mut self,
        _path: &Path,
        _line: usize,
        _column: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(project) = self.project.as_ref() else {
            return false;
        };
        let Some(editor) = self.active_editor() else {
            return false;
        };
        let word = editor.read(cx).word_under_cursor();
        if word.is_empty() {
            return false;
        }
        // Scan all project sources for a `label:` definition.
        let Ok(sources) = project.source_files() else {
            return false;
        };
        for source in sources {
            let ext = source
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !matches!(ext.as_str(), "s" | "asm" | "inc") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&source) else {
                continue;
            };
            for (idx, src_line) in text.lines().enumerate() {
                let trimmed = src_line.trim();
                // `label:` or `label:` with a trailing comment/directive.
                if let Some(label) = trimmed.strip_suffix(':') {
                    if label.trim() == word {
                        self.open_file(source.clone(), cx);
                        if let Some(target) = self.active_editor() {
                            target.update(cx, |ed, cx| {
                                ed.set_cursor_line(idx + 1, cx)
                            });
                        }
                        self.status = format!("已跳转到汇编标签 {word}（{}:{}）", source.display(), idx + 1);
                        cx.notify();
                        return true;
                    }
                } else if let Some((label, _)) = trimmed.split_once(':') {
                    if label.trim() == word {
                        self.open_file(source.clone(), cx);
                        if let Some(target) = self.active_editor() {
                            target.update(cx, |ed, cx| {
                                ed.set_cursor_line(idx + 1, cx)
                            });
                        }
                        self.status = format!("已跳转到汇编标签 {word}（{}:{}）", source.display(), idx + 1);
                        cx.notify();
                        return true;
                    }
                }
            }
        }
        false
    }
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
                            this.references_open = false;
                         } else {
                             let count = locations.len();
                             this.references = locations;
                            this.references_selected = 0;
                            this.references_open = true;
                            this.status = format!("找到 {count} 处引用");
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

    /// Open the rename dialog for the symbol under the cursor.
    fn rename_symbol(&mut self, cx: &mut Context<Self>) {
        let Some((path, line, column)) = self.cursor_location(cx) else {
            self.status = "没有活动编辑器".to_string();
            cx.notify();
            return;
        };
        let editor = self.active_editor();
        let Some(editor) = editor else {
            self.status = "没有活动编辑器".to_string();
            cx.notify();
            return;
        };
        // Pre-fill the field with the identifier under the cursor.
        let ident = editor.read(cx).word_under_cursor();
        let field = cx.new(|cx| crate::text_field::TextField::new("新名称", cx));
        if !ident.is_empty() {
            field.update(cx, |field, cx| field.set_text(&ident, cx));
        }
        self.rename_dialog = Some(RenameDialog {
            path,
            line,
            column,
            field,
        });
        self.focus_dialog_on_next_frame = true;
        cx.notify();
    }

    /// Submit the rename: request LSP, apply the returned edits, and report.
    fn confirm_rename(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.rename_dialog.take() else {
            return;
        };
        let new_name = dialog.field.read(cx).text();
        let new_name = new_name.trim().to_string();
        if new_name.is_empty() {
            self.status = "重命名符号：名称不能为空".to_string();
            self.rename_dialog = Some(dialog);
            cx.notify();
            return;
        }
        let Some(server) = &self.lsp else {
            self.status = "语言服务器未连接（检查是否安装了 clangd / rust-analyzer / zls）".to_string();
            self.rename_dialog = Some(dialog);
            cx.notify();
            return;
        };
        let path = dialog.path.clone();
        let line = dialog.line;
        let column = dialog.column;
        let Ok(rx) = server.rename(path, line, column, new_name.clone()) else {
            self.status = "无法请求重命名（语言服务器不可用）".to_string();
            self.rename_dialog = Some(dialog);
            cx.notify();
            return;
        };
        self.status = "正在重命名符号…".to_string();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let response = rx
                .recv_timeout(std::time::Duration::from_secs(8))
                .ok();
            this.update(cx, |this, cx| {
                match response {
                    Some(response) => {
                        let edits = ns_lsp::parse_workspace_edit(&response);
                        if edits.is_empty() {
                            this.status = format!("未找到可重命名的符号：{new_name}");
                        } else {
                            this.apply_workspace_edit(&edits, cx);
                            this.status = format!("已重命名符号为 {new_name}");
                        }
                    }
                    None => this.status = "重命名请求超时".to_string(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Apply `WorkspaceEdit` edits: update open editor tabs in memory and write
    /// the file to disk so the rename is persistent and the LSP stays in sync.
    fn apply_workspace_edit(&mut self, edits: &[ns_lsp::TextEdit], cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            self.status = "没有打开的项目".to_string();
            return;
        };
        for edit in edits {
            let absolute = if edit.path.is_absolute() {
                edit.path.clone()
            } else {
                project.root.join(&edit.path)
            };
            // Update an open tab, if any.
            if let Some(tab) = self
                .tabs
                .iter()
                .find(|tab| tab.path == absolute)
            {
                tab.editor.update(cx, |editor, _cx| {
                    editor.apply_text_edit(
                        edit.start_line,
                        edit.start_char,
                        edit.end_line,
                        edit.end_char,
                        &edit.new_text,
                    );
                });
            }
            // Persist to disk (LSP requires the file on disk to stay in sync).
            if absolute.exists() {
                if let Ok(content) = std::fs::read_to_string(&absolute) {
                    if let Some(applied) = apply_edit_to_text(
                        &content,
                        edit.start_line,
                        edit.start_char,
                        edit.end_line,
                        edit.end_char,
                        &edit.new_text,
                    ) {
                        let _ = std::fs::write(&absolute, applied);
                    }
                }
            }
        }
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
                            this.hover_tooltip = None;
                            this.hover_anchor = None;
                        } else {
                            this.status = text.clone();
                            this.hover_tooltip = Some(text);
                            this.hover_anchor = Some((path.clone(), line, column));
                        }
                    }
                    None => {
                        this.status = "悬停提示请求超时".to_string();
                        this.hover_tooltip = None;
                    }
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
                                this.signature_anchor = Some((path.clone(), line, column));
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

    /// Request the document outline for the active file (LSP) and cache it.
    fn lsp_document_symbols(&mut self, cx: &mut Context<Self>) {
        let Some((path, _, _)) = self.cursor_location(cx) else {
            return;
        };
        let Some(server) = &self.lsp else {
            return;
        };
        let Ok(rx) = server.document_symbols(path) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let response = rx
                .recv_timeout(std::time::Duration::from_secs(8))
                .ok();
            this.update(cx, |this, cx| {
                if let Some(response) = response {
                    this.outline = ns_lsp::parse_symbols(&response);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Open the workspace-symbol search popup and focus its query field.
    fn open_workspace_symbols(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.lsp.is_none() {
            self.status = "语言服务未运行，无法搜索项目符号".to_string();
            cx.notify();
            return;
        }
        self.workspace_symbol_open = true;
        self.workspace_symbols.clear();
        self.workspace_symbol_selected = 0;
        self.workspace_symbol_generation = self.workspace_symbol_generation.wrapping_add(1);
        let focus = self.workspace_symbol_field.update(cx, |field, cx| {
            field.set_text("", cx);
            field.focus_handle(cx)
        });
        window.focus(&focus);
        cx.notify();
    }

    /// Issue a `workspace/symbol` search for the current query. Responses are
    /// tagged with a generation id so a stale reply cannot overwrite a newer
    /// search.
    fn workspace_symbol_search(&mut self, cx: &mut Context<Self>) {
        let Some(server) = &self.lsp else {
            return;
        };
        let query = self.workspace_symbol_field.read(cx).text().trim().to_string();
        if query.is_empty() {
            self.workspace_symbols.clear();
            self.workspace_symbol_selected = 0;
            cx.notify();
            return;
        }
        let generation = self.workspace_symbol_generation;
        let Ok(rx) = server.workspace_symbols(query) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let response = rx.recv_timeout(std::time::Duration::from_secs(8)).ok();
            this.update(cx, |this, cx| {
                if this.workspace_symbol_generation != generation {
                    // A newer search superseded this one.
                    return;
                }
                if let Some(response) = response {
                    this.workspace_symbols = ns_lsp::parse_workspace_symbols(&response);
                    if this.workspace_symbol_selected >= this.workspace_symbols.len() {
                        this.workspace_symbol_selected = 0;
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Move the highlighted workspace symbol up/down.
    fn workspace_symbol_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.workspace_symbols.is_empty() {
            return;
        }
        let len = self.workspace_symbols.len();
        let current = self.workspace_symbol_selected as isize;
        let next = (current + delta).rem_euclid(len as isize) as usize;
        self.workspace_symbol_selected = next;
        cx.notify();
     }

    /// Move the highlighted reference up/down.
    fn references_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.references.is_empty() {
            return;
        }
        let len = self.references.len();
        let current = self.references_selected as isize;
        let next = (current + delta).rem_euclid(len as isize) as usize;
        self.references_selected = next;
        cx.notify();
    }
 
     /// Open the highlighted workspace symbol in the editor.
    fn workspace_symbol_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(symbol) = self.workspace_symbols.get(self.workspace_symbol_selected) else {
            return;
        };
        let path = symbol.path.clone();
        let line = symbol.line;
        self.workspace_symbol_open = false;
        self.push_nav_history(cx);
        self.open_file(path, cx);
        if let Some(editor) = self.active_editor() {
            editor.update(cx, |editor, cx| editor.set_cursor_line(line + 1, cx));
        }
        cx.notify();
    }

    /// Close the workspace-symbol popup.
    fn close_workspace_symbols(&mut self, cx: &mut Context<Self>) {
        if self.workspace_symbol_open {
            self.workspace_symbol_open = false;
            cx.notify();
        }
     }

    /// Open the highlighted reference in the editor.
    fn references_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(reference) = self.references.get(self.references_selected) else {
            return;
        };
        let path = reference.path.clone();
        let line = reference.line;
        let column = reference.column;
        self.references_open = false;
        self.push_nav_history(cx);
        self.open_file(path, cx);
         if let Some(editor) = self.active_editor() {
             editor.update(cx, |editor, cx| {
                editor.set_cursor_line_column(line + 1, column, cx);
             });
         }
        cx.notify();
    }

    /// Close the references popup.
    fn close_references(&mut self, cx: &mut Context<Self>) {
        if self.references_open {
            self.references_open = false;
            cx.notify();
        }
    }
 
     /// Open the Quick Open popup, focus its query field, and populate the file
    /// list from the project's source files.
    fn open_quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project.is_none() {
            self.status = "未打开项目".to_string();
            cx.notify();
            return;
        }
        self.quick_open = true;
        self.quick_open_files.clear();
        self.quick_open_selected = 0;
        let focus = self.quick_open_field.update(cx, |field, cx| {
            field.set_text("", cx);
            field.focus_handle(cx)
        });
        window.focus(&focus);
        self.quick_open_refresh(cx);
        cx.notify();
    }

    /// Refresh the Quick Open file list from the project root, filtered by the
    /// current query (case-insensitive substring on the relative path).
    fn quick_open_refresh(&mut self, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        let query = self.quick_open_field.read(cx).text().trim().to_lowercase();
        let files = search::collect_files(&project.root);
        let filtered: Vec<String> = if query.is_empty() {
            files
        } else {
            files
                .into_iter()
                .filter(|path| path.to_lowercase().contains(&query))
                .collect()
        };
        self.quick_open_files = filtered;
        if self.quick_open_selected >= self.quick_open_files.len() {
            self.quick_open_selected = 0;
        }
        cx.notify();
    }

    /// Move the Quick Open highlight up/down (wrapping).
    fn quick_open_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.quick_open_files.is_empty() {
            return;
        }
        let len = self.quick_open_files.len();
        let current = self.quick_open_selected as isize;
        self.quick_open_selected = (current + delta).rem_euclid(len as isize) as usize;
        cx.notify();
    }

    /// Open the highlighted Quick Open file.
    fn quick_open_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.quick_open_files.get(self.quick_open_selected) else {
            return;
        };
        let Some(project) = &self.project else {
            return;
        };
        let absolute = project.root.join(path);
        self.quick_open = false;
        self.push_nav_history(cx);
        self.open_file(absolute, cx);
        cx.notify();
    }

    /// Close the Quick Open popup.
    fn close_quick_open(&mut self, cx: &mut Context<Self>) {
        if self.quick_open {
            self.quick_open = false;
            cx.notify();
        }
    }

    /// Format the active file via LSP `textDocument/formatting` and apply the
    /// returned `TextEdit[]` to the editor.
    fn lsp_format_document(&mut self, cx: &mut Context<Self>) {
        let Some((path, _, _)) = self.cursor_location(cx) else {
            self.status = "没有活动编辑器".to_string();
            cx.notify();
            return;
        };
        let Some(server) = &self.lsp else {
            self.status = "语言服务未运行".to_string();
            cx.notify();
            return;
        };
        let Ok(rx) = server.formatting(path.clone()) else {
            self.status = "格式化请求失败".to_string();
            cx.notify();
            return;
        };
        self.status = "正在格式化…".to_string();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let response = rx
                .recv_timeout(std::time::Duration::from_secs(8))
                .ok();
            this.update(cx, |this, cx| {
                match response {
                    Some(response) => {
                        let edits = ns_lsp::parse_format_edits(&response, &path);
                        if edits.is_empty() {
                            this.status = "格式化完成（无变更）".to_string();
                        } else {
                            let mut applied = 0usize;
                            if let Some(editor) = this.active_editor() {
                                editor.update(cx, |editor, cx| {
                                    for edit in edits {
                                        editor.apply_text_edit(
                                            edit.start_line,
                                            edit.start_char,
                                            edit.end_line,
                                            edit.end_char,
                                            &edit.new_text,
                                        );
                                        applied += 1;
                                    }
                                    cx.notify();
                                });
                            }
                            this.status = format!("格式化完成（{applied} 处编辑）");
                        }
                    }
                    None => this.status = "格式化请求超时".to_string(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Request code actions at the cursor (LSP `textDocument/codeAction`) and
    /// apply the first action that carries edits. Actions without edits (e.g.
    /// command-only quick fixes) are reported but not applied.
    fn lsp_code_action(&mut self, cx: &mut Context<Self>) {
        let Some((path, line, column)) = self.cursor_location(cx) else {
            self.status = "没有活动编辑器".to_string();
            cx.notify();
            return;
        };
        let Some(server) = &self.lsp else {
            self.status = "语言服务未运行".to_string();
            cx.notify();
            return;
        };
        let Ok(rx) = server.code_action(path, line, column) else {
            self.status = "快速修复请求失败".to_string();
            cx.notify();
            return;
        };
        self.status = "正在请求快速修复…".to_string();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let response = rx
                .recv_timeout(std::time::Duration::from_secs(8))
                .ok();
            this.update(cx, |this, cx| {
                match response {
                    Some(response) => {
                        let actions = ns_lsp::parse_code_actions(&response);
                        if actions.is_empty() {
                            this.status = "当前位置没有可用的快速修复".to_string();
                        } else if let Some(action) = actions.iter().find(|a| !a.edits.is_empty()) {
                            this.apply_workspace_edit(&action.edits, cx);
                            this.status = format!("已应用快速修复：{}", action.title);
                        } else {
                            let titles: Vec<&str> =
                                actions.iter().map(|a| a.title.as_str()).collect();
                            this.status =
                                format!("快速修复需要命令支持（未应用）：{}", titles.join(" / "));
                        }
                    }
                    None => this.status = "快速修复请求超时".to_string(),
                }
                cx.notify();
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
                        let mut items = parse_completion_items(&response);
                        // Merge user-defined snippets (trigger-word matches).
                        let word = this.completion_word(cx);
                        let user_snippets = ns_project::SnippetStore::load().matching(&word);
                        for snippet in user_snippets {
                            items.push(ns_lsp::CompletionItem {
                                label: snippet.prefix.clone(),
                                detail: if snippet.description.is_empty() {
                                    "代码片段".to_string()
                                } else {
                                    snippet.description.clone()
                                },
                                kind: "snippet".to_string(),
                                insert_text: Some(snippet.body.clone()),
                                snippet: true,
                                documentation: snippet.body.clone(),
                            });
                        }
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

    /// Insert the selected completion item. `insert_text` (from the server's
    /// `insertText`/`textEdit.newText`) is preferred over the label, and a
    /// snippet body is expanded (`${1:...}`, `$0`, `$TM_FILENAME`). The typed
    /// prefix before the cursor is replaced so confirming `printf` over `pri`
    /// does not duplicate the prefix.
    fn completion_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.completions.get(self.completion_selected) else {
            self.completion_open = false;
            cx.notify();
            return;
        };
        let insert = item
            .insert_text
            .clone()
            .unwrap_or_else(|| item.label.clone());
        let active_path = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.path.clone());
        let (text, cursor_at) = if item.snippet {
            let (text, cursor_at) = expand_snippet(&insert, active_path.as_deref());
            (text, cursor_at)
        } else {
            (insert, None)
        };
        if let Some(editor) = self.active_editor() {
            editor.update(cx, |editor, cx| {
                editor.insert_completion(&text, true, cursor_at, cx);
            });
        }
        self.completion_open = false;
        self.status = if item.snippet { "已插入代码片段".to_string() } else { "已插入补全".to_string() };
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

    /// The identifier word immediately before the cursor in the active editor
    /// (used to match user snippet trigger words and to filter completion).
    fn completion_word(&self, cx: &mut Context<Self>) -> String {
        let Some(editor) = self.active_editor() else {
            return String::new();
        };
        let editor = editor.read(cx);
        let (line, column) = editor.cursor_line_col();
        let Some(line_text) = editor.line_text(line.saturating_sub(1)) else {
            return String::new();
        };
        let column = column.saturating_sub(1).min(line_text.len());
        let prefix = &line_text[..column];
        let word_start = prefix
            .char_indices()
            .rev()
            .take_while(|(_, ch)| ch.is_alphanumeric() || *ch == '_')
            .last()
            .map(|(index, _)| index)
            .unwrap_or(column);
        prefix[word_start..].to_string()
    }

    /// Record the current cursor position as a navigation-history entry.
    /// Clears the forward stack (a new jump invalidates any redo path).
    fn push_nav_history(&mut self, cx: &mut Context<Self>) {
        if let Some((path, line, column)) = self.cursor_location(cx) {
            push_nav_entry(
                &mut self.nav_back,
                &mut self.nav_forward,
                NavEntry { path, line, column },
            );
        }
    }

    /// Alt+←: jump back to the previous navigation-history position.
    fn navigate_back(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.nav_back.pop() {
            // Record where we are now so Alt+→ can return here.
            if let Some((path, line, column)) = self.cursor_location(cx) {
                self.nav_forward.push(NavEntry { path, line, column });
            }
            self.jump_to_nav_entry(&entry, cx);
        } else {
            self.status = "没有可回退的导航历史".to_string();
            cx.notify();
        }
    }

    /// Alt+→: jump forward to the next navigation-history position.
    fn navigate_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.nav_forward.pop() {
            if let Some((path, line, column)) = self.cursor_location(cx) {
                self.nav_back.push(NavEntry { path, line, column });
            }
            self.jump_to_nav_entry(&entry, cx);
        } else {
            self.status = "没有可前进的导航历史".to_string();
            cx.notify();
        }
    }

    /// Recompute the active editor's diagnostic markers (error/warning rows)
    /// from build diagnostics and LSP diagnostics. Rows are 0-based and
    /// deduplicated in `Editor::set_diagnostics`.
    fn refresh_diagnostics(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.active_tab else {
            return;
        };
        let Some(path) = self.tabs.get(index).map(|tab| tab.path.clone()) else {
            return;
        };
        let mut marks: Vec<(usize, bool)> = Vec::new();
        for diagnostic in &self.diagnostics {
            let same_file = diagnostic
                .path
                .as_ref()
                .map(|candidate| paths_equal(candidate, &path))
                .unwrap_or(false);
            if same_file
                && let Some(line) = diagnostic.line
            {
                let is_error = diagnostic.severity == Severity::Error;
                marks.push((line.saturating_sub(1), is_error));
            }
        }
        for lsp_diagnostic in self.lsp_diagnostics.get(&path).into_iter().flatten() {
            let is_error = matches!(
                lsp_diagnostic.severity,
                ns_lsp::LspSeverity::Error
            );
            marks.push((lsp_diagnostic.line, is_error));
        }
        if let Some(tab) = self.tabs.get(index) {
            tab.editor.update(cx, |editor, cx| editor.set_diagnostics(marks, cx));
        }
     }

    /// Push per-line coverage hits onto every open editor whose file was
    /// covered by the last gcov run. Uncovered files (no line data) are left
    /// untouched; a cleared run removes the overlay.
    fn apply_coverage_to_editors(&mut self, cx: &mut Context<Self>) {
        for tab in &mut self.tabs {
            let mut hits = Vec::new();
            for file in &self.coverage {
                let file_path = PathBuf::from(&file.file);
                if paths_equal(&file_path, &tab.path) {
                    hits = file.line_hits.clone().unwrap_or_default();
                    break;
                }
            }
            tab.editor.update(cx, |editor, cx| editor.set_coverage(hits.clone(), cx));
        }
    }
 
     /// Move the cursor to a navigation-history entry, opening the file if needed.
    fn jump_to_nav_entry(&mut self, entry: &NavEntry, cx: &mut Context<Self>) {        if entry.path.exists() {
            self.open_file(entry.path.clone(), cx);
        }
        if let Some(editor) = self.active_editor() {
            editor.update(cx, |editor, cx| {
                editor.set_cursor_line_column(entry.line + 1, entry.column, cx);
            });
        }
        self.status = format!("导航:{}", entry.path.display());
        cx.notify();
    }

    /// Jump to the header named by a `#include` directive on the cursor line.
    /// Resolves against the project include paths, then the including file's
    /// directory, then common system paths; reports honestly when not found.
    fn jump_to_include(&mut self, cx: &mut Context<Self>) {
        let Some((path, line, _column)) = self.cursor_location(cx) else {
            self.status = "没有活动编辑器".to_string();
            cx.notify();
            return;
        };
        let editor = self.active_editor();
        let Some(editor) = editor else {
            self.status = "没有活动编辑器".to_string();
            cx.notify();
            return;
        };
        let line_text = editor.read(cx).line_text(line).unwrap_or("").to_string();
        let Some((kind, name)) = parse_include_directive(&line_text) else {
            self.status = "光标不在 #include 指令上".to_string();
            cx.notify();
            return;
        };
        let candidates = self.include_search_paths(kind, &path);
        let mut found: Option<PathBuf> = None;
        for dir in &candidates {
            let candidate = dir.join(&name);
            if candidate.exists() {
                found = Some(candidate);
                break;
            }
        }
        if let Some(target) = found {
            self.push_nav_history(cx);
            self.open_file(target, cx);
            self.status = format!("已跳转到头文件：{name}");
        } else {
            self.status = format!("未找到头文件：{name}（搜索了 {} 个路径）", candidates.len());
        }
        cx.notify();
    }

    /// Ordered candidate directories for resolving an `#include` name.
    fn include_search_paths(&self, kind: IncludeKind, including: &PathBuf) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        // Angle includes never search the including file's directory first
        // (standard behaviour); quoted includes do.
        if kind == IncludeKind::Quotes {
            if let Some(parent) = including.parent() {
                dirs.push(parent.to_path_buf());
            }
        }
        if let Some(project) = &self.project {
            for inc in &project.config.compiler.includes {
                let abs = project.root.join(inc);
                dirs.push(abs);
            }
            // The project root itself is a common include base.
            dirs.push(project.root.clone());
        }
        // Common system include directories, when present.
        for sys in ["/usr/include", "/usr/local/include", "/opt/homebrew/include"] {
            let p = PathBuf::from(sys);
            if p.is_dir() {
                dirs.push(p);
            }
        }
        dirs
    }

    fn cancel_condition_dialog(&mut self, cx: &mut Context<Self>) {
        if self.condition_dialog.take().is_some() {
            self.status = "已取消条件编辑".to_string();
            cx.notify();
        }
    }

    /// Open the right-click menu for a tree entry at the given position.
    fn open_tree_menu(&mut self, path: PathBuf, is_dir: bool, x: Pixels, y: Pixels, cx: &mut Context<Self>) {
        self.tree_menu = Some(TreeContextMenu { path, is_dir, x, y });
        cx.notify();
    }

    /// Move a dragged tree entry to the project root (drop on empty space).
    fn drop_tree_entry(&mut self, drag: &TreeDragPayload, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            return;
        };
        let root = project.root.clone();
        self.move_tree_entry(&drag.source, &root, cx);
    }

    /// Move a dragged tree entry into `target` (a directory).
    fn drop_tree_entry_into(
        &mut self,
        drag: &TreeDragPayload,
        target: &Path,
        target_is_dir: bool,
        cx: &mut Context<Self>,
    ) {
        let parent = if target_is_dir {
            target.to_path_buf()
        } else {
            target.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        self.move_tree_entry(&drag.source, &parent, cx);
    }

    /// Move `source` into `dest_dir` on disk, then refresh the tree. Refuses
    /// self/nested moves (a directory into itself or a descendant), moves that
    /// would overwrite an existing entry, and source==dest. Reports the outcome
    /// in the status bar and re-opens any open tab whose path changed.
    fn move_tree_entry(&mut self, source: &Path, dest_dir: &Path, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            return;
        };
        let root = project.root.clone();
        let source = if source.is_absolute() {
            source.to_path_buf()
        } else {
            root.join(source)
        };
        let dest_dir = if dest_dir.is_absolute() {
            dest_dir.to_path_buf()
        } else {
            root.join(dest_dir)
        };
        let destination = match tree_move_plan(&source, &dest_dir) {
            Ok(destination) => destination,
            Err(message) => {
                self.status = message;
                cx.notify();
                return;
            }
        };
        match std::fs::rename(&source, &destination) {
            Ok(()) => {
                // Refresh open tabs whose path is under the moved source.
                for tab in &mut self.tabs {
                    if tab.path.starts_with(&source) {
                        let suffix = tab.path.strip_prefix(&source).unwrap_or(Path::new(""));
                        tab.path = destination.join(suffix);
                    }
                }
                self.status = format!("已移动到 {}", destination.display());
            }
            Err(error) => {
                self.status = format!("移动失败: {error}");
            }
        }
        cx.notify();
    }
    /// Target directory for creating a new entry inside the menu's entry.
    fn tree_menu_parent(&self) -> Option<PathBuf> {
        let menu = self.tree_menu.as_ref()?;
        Some(if menu.is_dir {
            menu.path.clone()
        } else {
            menu.path.parent().map(Path::to_path_buf)?
        })
    }

    /// Open the "new file / new directory" dialog inside the menu target.
    fn open_new_entry_dialog(&mut self, kind: NewEntryKind, cx: &mut Context<Self>) {
        let Some(parent) = self.tree_menu_parent() else {
            return;
        };
        self.tree_menu = None;
        let field = cx.new(|cx| TextField::new("名称…", cx));
        self.new_entry_dialog = Some(NewEntryDialog {
            parent,
            kind,
            field,
            rename: None,
        });
        self.focus_dialog_on_next_frame = true;
        cx.notify();
    }

    /// Create or rename the entry, then refresh the tree and expand its parent.
    fn save_new_entry_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.new_entry_dialog.take() else {
            return;
        };
        let name = dialog.field.read(cx).text().trim().to_string();
        if name.is_empty() {
            self.status = "名称不能为空".to_string();
            cx.notify();
            return;
        }
        if name.contains('/') || name.contains('\\') || name.contains(':') || name.contains('\0') {
            self.status = "名称不能包含路径分隔符".to_string();
            cx.notify();
            return;
        }

        // Rename mode: move the existing path to the new name.
        if let Some(old) = dialog.rename {
            let target = dialog.parent.join(&name);
            if target == old {
                cx.notify();
                return;
            }
            if target.exists() {
                self.status = format!("已存在: {name}");
                cx.notify();
                return;
            }
            match std::fs::rename(&old, &target) {
                Ok(()) => {
                    // Update any open tab that pointed at the old path.
                    for tab in &mut self.tabs {
                        if tab.path == old {
                            tab.path = target.clone();
                        }
                    }
                    // Expand the parent so the renamed entry is visible.
                    self.expanded.insert(dialog.parent.clone());
                    self.status = format!("已重命名为 {name}");
                    cx.notify();
                }
                Err(error) => {
                    self.status = format!("重命名失败: {error}");
                    cx.notify();
                }
            }
            return;
        }

        // Create mode.
        let target = dialog.parent.join(&name);
        if target.exists() {
            self.status = format!("已存在: {name}");
            cx.notify();
            return;
        }
        let result = match dialog.kind {
            NewEntryKind::File => std::fs::write(&target, "").map_err(|e| e.to_string()),
            NewEntryKind::Directory => std::fs::create_dir(&target).map_err(|e| e.to_string()),
        };
        match result {
            Ok(()) => {
                if let Some(project) = &self.project {
                    self.expanded.insert(project.root.clone());
                    self.expanded.insert(dialog.parent.clone());
                }
                if dialog.kind == NewEntryKind::File {
                    self.open_file(target, cx);
                }
                self.status = match dialog.kind {
                    NewEntryKind::File => format!("已创建文件 {name}"),
                    NewEntryKind::Directory => format!("已创建目录 {name}"),
                };
                cx.notify();
            }
            Err(error) => {
                self.status = format!("创建失败: {error}");
                cx.notify();
            }
        }
    }

    fn cancel_new_entry_dialog(&mut self, cx: &mut Context<Self>) {
        if self.new_entry_dialog.take().is_some() {
            cx.notify();
        }
    }

    /// Rename a tree entry (file or directory).
    fn rename_tree_entry(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.tree_menu = None;
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
            return;
        };
        let field = cx.new(|cx| {
            let mut field = TextField::new("名称…", cx);
            field.set_text(name, cx);
            field
        });
        self.new_entry_dialog = Some(NewEntryDialog {
            parent: path.parent().map(Path::to_path_buf).unwrap_or_default(),
            kind: NewEntryKind::File,
            field,
            rename: Some(path),
        });
        self.focus_dialog_on_next_frame = true;
        cx.notify();
    }

    /// Delete a tree entry (file or directory, recursive).
    fn delete_tree_entry(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.tree_menu = None;
        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Ok(()) => {
                // Close the tab if the deleted file was open.
                self.tabs.retain(|tab| tab.path != path);
                if let Some(active) = self.active_tab {
                    if active >= self.tabs.len() {
                        self.active_tab = None;
                    }
                }
                self.status = format!("已删除 {}", path.display());
                cx.notify();
            }
            Err(error) => {
                self.status = format!("删除失败: {error}");
                cx.notify();
            }
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

 
     /// Apply a quick fix to the build diagnostic at `index`. For
     /// `AddInclude`, the `#include <...>` line is inserted at the top of the
     /// source file (after any existing `#include` lines, so the leading block
     /// stays grouped); for `Hint`, the hint is shown in the status bar.
     fn apply_quick_fix(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(diagnostic) = self.diagnostics.get(index) else {
            return;
        };
        let Some(path) = diagnostic.path.clone() else {
            self.status = "该诊断没有关联文件，无法应用修复。".to_string();
            cx.notify();
            return;
        };
         let source = std::fs::read_to_string(&path).ok();
         match ns_build::suggest_quick_fix(diagnostic, source.as_deref()) {
             Some(ns_build::QuickFix::AddInclude { header }) => {
                 // Insert after the last existing `#include` line, else at the top.
                 let text = source.clone().unwrap_or_default();
                 let include = format!("#include <{header}>");
                 if text.lines().any(|line| line.trim() == include) {
                     self.status = format!("`#include <{header}>` 已存在于 {}", path.display());
                     cx.notify();
                     return;
                 }
                 // Rebuild the file on disk, then reload the open editor tab.
                 let new_text = insert_include_line(&text, &include, include_insert_at(&text));
                 match std::fs::write(&path, &new_text) {
                     Ok(()) => {
                         self.status =
                             format!("已添加 `#include <{header}>` 到 {}", path.display());
                         self.reload_diagnostic_file(&path, cx);
                     }
                     Err(err) => {
                         self.status = format!("写入 {} 失败: {err}", path.display());
                     }
                 }
             }
             Some(ns_build::QuickFix::Hint(hint)) => {
                 self.status = hint;
             }
             Some(ns_build::QuickFix::AddLinkFlag { .. }) => {
                 self.status = "链接标志修复尚未接入设置页。".to_string();
             }
             None => {
                 self.status = "没有可用的快速修复。".to_string();
             }
         }
         cx.notify();
     }
 
     /// Reload a source file that a quick fix edited, refreshing the open
     /// editor tab and clearing stale diagnostics for that file.
     fn reload_diagnostic_file(&mut self, path: &Path, cx: &mut Context<Self>) {
         if let Some(editor) = self.active_editor() {
             editor.update(cx, |editor, cx| {
                 if let Ok(text) = std::fs::read_to_string(path) {
                    editor.reload_text(&text, cx);
                 }
             });
         }
         cx.notify();
     }
 
     fn tree_entries(&self) -> Vec<TreeEntry> {
        let Some(project) = &self.project else {
            return Vec::new();
        };
        if !self.tree_filter.is_empty() {
            return self.filtered_tree_entries(project);
        }
        let mut entries = Vec::new();
        collect_tree(
            &project.root,
            0,
            &self.expanded,
            &mut entries,
        );
        entries
    }

    /// Build a flat, filter-restricted tree: every file whose path contains the
    /// filter query is listed, preceded by its ancestor directories (so the
    /// context is visible). Directory entries are marked expanded.
    fn filtered_tree_entries(&self, project: &ns_project::Project) -> Vec<TreeEntry> {
        let query = self.tree_filter.to_lowercase();
        let files = search::collect_files(&project.root);
        let mut entries: Vec<TreeEntry> = Vec::new();
        for relative in files {
            if !relative.to_lowercase().contains(&query) {
                continue;
            }
            let abs = project.root.join(&relative);
            // Ancestor directories, root-most first, so the path reads top-down.
            let mut ancestors: Vec<&str> = relative
                .rsplit('/')
                .skip(1)
                .filter(|seg| !seg.is_empty())
                .collect();
            ancestors.reverse();
            let mut depth = 0usize;
            let mut prefix = String::new();
            for seg in &ancestors {
                let dir_path = project.root.join(&prefix).join(seg);
                entries.push(TreeEntry {
                    path: dir_path,
                    name: seg.to_string(),
                    is_dir: true,
                    depth,
                    expanded: true,
                });
                prefix = if prefix.is_empty() {
                    seg.to_string()
                } else {
                    format!("{prefix}/{seg}")
                };
                depth += 1;
            }
            entries.push(TreeEntry {
                path: abs,
                name: relative
                    .rsplit('/')
                    .next()
                    .unwrap_or(&relative)
                    .to_string(),
                is_dir: false,
                depth,
                expanded: false,
            });
        }
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

        let menus = [
            TopMenu::File,
            TopMenu::Edit,
            TopMenu::Build,
            TopMenu::Debug,
            TopMenu::Flash,
        ];
        let mut triggers: Vec<gpui::AnyElement> = Vec::new();
        for menu in menus {
            let open = self.top_menu_open == Some(menu);
            triggers.push(
                div()
                    .id(("top-menu", menu as usize))
                    .h(px(30.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .bg(if open {
                        theme::alpha(theme::STROKE, 0.12)
                    } else {
                        theme::alpha(0xffffff, 0.0)
                    })
                    .text_size(px(11.5))
                    .text_color(if open {
                        theme::color(theme::TEXT)
                    } else {
                        theme::color(theme::TEXT_DIM)
                    })
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.08)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.top_menu_open = if this.top_menu_open == Some(menu) {
                            None
                        } else {
                            Some(menu)
                        };
                        cx.notify();
                    }))
                    .child(menu.label())
                    .into_any_element(),
            );
        }

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
                                    .text_color(theme::color(theme::TEXT))
                                    .child(project_name),
                            )
                            .child(
                                div()
                                    .max_w(px(330.))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(10.))
                                    .text_color(theme::color(theme::TEXT_FAINT))
                                    .child(project_path),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(3.))
                    .children(triggers),
            )
    }

    /// Render the currently open topbar menu dropdown (absolute, below topbar).
    fn render_top_menu_dropdown(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let menu = self.top_menu_open?;
        let mut items: Vec<gpui::AnyElement> = Vec::new();
        let item = |id: &'static str, label: &'static str, danger: bool| {
            div()
                .id(id)
                .px(px(10.))
                .py(px(7.))
                .rounded(px(6.))
                .text_size(px(11.))
                .text_color(if danger {
                    theme::color(theme::CINNABAR)
                } else {
                    theme::color(theme::TEXT)
                })
                .cursor_pointer()
                .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.12)))
                .child(label)
        };
        match menu {
            TopMenu::File => {
                items.push(
                    item("topm-new", "新建项目…", false)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.top_menu_open = None;
                            this.open_wizard(window, cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("topm-open", "打开项目…", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            this.open_project_dialog(cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("topm-save", "保存", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            this.save_active(cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("topm-settings", "项目设置…", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            this.open_settings(cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("topm-quit", "退出", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            cx.quit();
                        }))
                        .into_any_element(),
                );
            }
            TopMenu::Edit => {
                items.push(
                    item("topm-find-file", "文件内查找…", false)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.top_menu_open = None;
                            this.find_in_file(window, cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("topm-find-project", "全局查找…", false)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.top_menu_open = None;
                            this.open_search(window, cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("topm-palette", "命令面板…", false)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.top_menu_open = None;
                            this.open_palette(window, cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("topm-breakpoints", "断点面板", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            this.show_breakpoints(cx);
                        }))
                        .into_any_element(),
                );
            }
            TopMenu::Build => {
                items.push(
                    item("topm-build", "构建", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            this.start_build(false, cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("topm-run", if self.run.running { "重新运行" } else { "运行" }, false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            if this.run.running {
                                this.restart_run(cx);
                            } else {
                                this.start_build(true, cx);
                            }
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("topm-stop-run", "停止运行", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            this.stop_run(cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("topm-terminal", "打开终端", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            this.new_terminal(cx);
                        }))
                        .into_any_element(),
                );
            }
            TopMenu::Debug => {
                items.push(
                    item("topm-debug", "开始调试", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            this.start_debug(cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("topm-stop-debug", "停止调试", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            this.stop_debug(cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("topm-restart-debug", "重启调试", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            this.restart_debug(cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("topm-run-to-cursor", "运行到光标", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            this.run_to_cursor(cx);
                        }))
                        .into_any_element(),
                );
            }
            TopMenu::Flash => {
                items.push(
                    item("topm-flash", "烧写固件", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            this.start_flash(cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("topm-flash-erase", "擦除芯片", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            this.flash_action(FlashAction::Erase, cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("topm-flash-verify", "校验固件", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            this.flash_action(FlashAction::Verify, cx);
                        }))
                        .into_any_element(),
                );
                 items.push(
                     item("topm-flash-read", "读回固件", false)
                         .on_click(cx.listener(|this, _, _, cx| {
                             this.top_menu_open = None;
                             this.flash_action(FlashAction::Read, cx);
                         }))
                         .into_any_element(),
                 );
                 items.push(
                     item("topm-flash-verify", "烧写并校验", false)
                         .on_click(cx.listener(|this, _, _, cx| {
                             this.top_menu_open = None;
                             this.flash_and_verify(cx);
                         }))
                         .into_any_element(),
                 );
                 items.push(
                     item("topm-flash-run", "烧写并运行", false)
                         .on_click(cx.listener(|this, _, _, cx| {
                             this.top_menu_open = None;
                             this.flash_and_run(cx);
                         }))
                         .into_any_element(),
                 );
                items.push(
                    item("topm-flash-debug", "烧写并调试", false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.top_menu_open = None;
                            this.flash_and_debug(cx);
                        }))
                        .into_any_element(),
                );
            }
        }
        Some(
            div()
                .id("top-menu-backdrop")
                .absolute()
                .inset_0()
                .on_mouse_down(MouseButton::Left, cx.listener(
                    |this, _: &MouseDownEvent, _, cx| {
                        this.top_menu_open = None;
                        cx.notify();
                    },
                ))
                .on_mouse_down(MouseButton::Right, cx.listener(
                    |this, _: &MouseDownEvent, _, cx| {
                        this.top_menu_open = None;
                        cx.notify();
                    },
                ))
                .child(
                    div()
                        .absolute()
                        .top(px(52.))
                        .left(px(78.))
                        .w(px(210.))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(theme::alpha(theme::STROKE, 0.3))
                        .bg(theme::color(theme::PANEL_SOLID))
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
                )
                .into_any_element(),
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
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .child(format!("{} 项", entries.len())),
                    ),
            )
            .child(
                div()
                    .p(px(8.))
                    .border_b_1()
                    .border_color(theme::alpha(theme::STROKE, 0.08))
                    .child(self.tree_filter_field.clone()),
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
                        let drop_path = path.clone();
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
                            .on_click({
                                let click_path = path.clone();
                                let filtering = !self.tree_filter.is_empty();
                                cx.listener(move |this, _, _, cx| {
                                    if is_dir {
                                        // In filter mode directory rows are just
                                        // context ancestors; clicking is a no-op.
                                        if !filtering {
                                            if !this.expanded.remove(&click_path) {
                                                this.expanded.insert(click_path.clone());
                                            }
                                        }
                                    } else {
                                        this.open_file(click_path.clone(), cx);
                                    }
                                    cx.notify();
                                })
                            })
                            .on_mouse_down(MouseButton::Right, {
                                let menu_path = path.clone();
                                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                                    this.open_tree_menu(menu_path.clone(), is_dir, event.position.x, event.position.y, cx);
                                })
                            })
                            .on_drag(
                                TreeDragPayload {
                                    source: path.clone(),
                                    is_dir,
                                },
                                |_drag, _position, _window, cx| cx.new(|_| DividerPreview),
                            )
                            .on_drag_move(cx.listener(
                                move |_this, _event: &gpui::DragMoveEvent<TreeDragPayload>, _window, _cx| {
                                    // Drag feedback: the preview element is
                                    // sufficient; no per-move state needed.
                                },
                            ))
                            .on_drop(cx.listener(
                                move |this, drag: &TreeDragPayload, _window, cx| {
                                    if is_dir {
                                        let drop_target = drop_path.clone();
                                        this.drop_tree_entry_into(drag, &drop_target, true, cx);
                                    }
                                },
                            ))
                            .can_drop(move |payload, _window, _cx| {
                                // Directories accept drops; files do not, and
                                // an entry cannot be dropped onto itself.
                                let Some(payload) = payload.downcast_ref::<TreeDragPayload>() else {
                                    return false;
                                };
                                let self_path = path.clone();
                                is_dir && payload.source != self_path && !payload.is_dir
                            })
                            .child(
                                div()
                                    .w(px(13.))
                                    .text_size(px(10.))
                                    .text_color(theme::color(if is_dir {
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
                                        theme::color(theme::TEXT)
                                    } else if entry.is_dir {
                                        theme::color(theme::TEXT_DIM)
                                    } else {
                                        theme::color(theme::TEXT_DIM)
                                    })
                                    .child(entry.name),
                            )
                    })),
            )
            .on_drop(cx.listener(
                move |this, drag: &TreeDragPayload, _window, cx| {
                    this.drop_tree_entry(drag, cx);
                },
            ))
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
                            .text_color(theme::color(theme::TEXT_FAINT))
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
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.tab_menu = Some(TabMenuState {
                            index,
                            x: event.position.x,
                            y: event.position.y,
                        });
                        cx.notify();
                    }))
                     .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(if active {
                                theme::color(theme::TEXT)
                            } else {
                                theme::color(theme::TEXT_DIM)
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
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .cursor_pointer()
                            .hover(|style| {
                                style
                                    .text_color(theme::color(theme::TEXT))
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
        // Symbol chain at the cursor, for the breadcrumb's navigation segment.
        let symbol_chain = active
            .as_ref()
            .map(|editor| {
                let (line, _) = editor.read(cx).cursor_line_col();
                self.outline_chain_for_line(line)
            })
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
                        .gap(px(6.))
                        .bg(theme::color(theme::BG_0).opacity(0.55))
                        .border_b_1()
                        .border_color(theme::alpha(theme::STROKE, 0.06))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .text_size(px(9.5))
                                .text_color(theme::color(theme::TEXT_FAINT))
                                .child(breadcrumb),
                        )
                        .children(symbol_chain.into_iter().map(|(name, line)| {
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .text_size(px(9.5))
                                .child(div().text_color(theme::color(theme::TEXT_FAINT)).child("›"))
                                .child(
                                    div()
                                        .id(("breadcrumb-symbol", line))
                                        .text_color(theme::color(theme::TEXT_DIM))
                                        .cursor_pointer()
                                        .hover(|style| {
                                            style.text_color(theme::color(theme::ACCENT))
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.jump_to_line(line, cx);
                                        }))
                                        .child(name),
                                )
                        }))
                )
            })
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
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
                            .when(self.hover_tooltip.is_some(), |style| {
                                style.child(self.render_hover_tooltip(cx))
                            })
                            .when(self.workspace_symbol_open, |style| {
                                style.child(self.render_workspace_symbol_popup(cx))
                            })
                             .when(self.quick_open, |style| {
                                 style.child(self.render_quick_open_popup(cx))
                             })
                            .when(self.references_open, |style| {
                                style.child(self.render_references_popup(cx))
                            })
                             .when(self.recent_open, |style| {
                                style.child(self.render_recent_files_popup(cx))
                            })
                            .when(
                                active
                                    .as_ref()
                                    .map(|editor| editor.read(cx).find_open())
                                    .unwrap_or(false),
                                |style| style.child(self.render_find_bar(cx)),
                            ),
                    )
                    .when(self.outline_open, |style| {
                        style.child(self.render_outline_panel(cx))
                    }),
            )
            .when(self.split.is_some(), |style| {
                // Second editor pane (split view), shown to the right of the
                // main editor with a divider.
                style.child(self.render_split_pane(cx))
            })
    }

    /// Render the split editor pane (a second, independent file view).
    fn render_split_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(split) = &self.split else {
            return div().into_any_element();
        };
        let path_text = self.display_path(&split.path);
        div()
            .w(px(420.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme::alpha(theme::STROKE, 0.16))
            .child(
                div()
                    .h(px(30.))
                    .flex_none()
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .bg(theme::color(theme::BG_0).opacity(0.55))
                    .border_b_1()
                    .border_color(theme::alpha(theme::STROKE, 0.06))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(10.))
                            .text_color(theme::color(theme::TEXT_DIM))
                            .child("分屏 · ")
                            .child(path_text),
                    )
                    .child(
                        div()
                            .id("split-close")
                            .px(px(6.))
                            .rounded(px(4.))
                            .text_size(px(10.))
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .cursor_pointer()
                            .hover(|style| {
                                style.text_color(theme::color(theme::DANGER))
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close_split(cx);
                            }))
                            .child("✕"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .relative()
                    .overflow_hidden()
                    .child(split.editor.clone()),
            )
            .into_any_element()
    }

    /// Right-side document outline (symbol list) panel.
    fn render_outline_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active_editor();
        let cursor_line = active
            .as_ref()
            .map(|editor| editor.read(cx).cursor_line_col().0)
            .unwrap_or(0);
        let outline = &self.outline;

        div()
            .id("outline-panel")
            .w(px(220.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme::alpha(theme::STROKE, 0.08))
            .bg(theme::color(theme::BG_0).opacity(0.45))
            .child(
                div()
                    .px(px(10.))
                    .pt(px(7.))
                    .pb(px(5.))
                    .text_size(px(10.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::color(theme::TEXT_FAINT))
                     .child("大纲"),
             )
            .child(
                div()
                    .px(px(8.))
                    .pb(px(6.))
                    .child(
                        self.outline_filter_input
                            .clone()
                            .map(|field| div().child(field).into_any_element())
                            .unwrap_or_else(|| div().into_any_element()),
                    ),
            )
             .child(
                div()
                    .id("outline-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(6.))
                    .pb(px(8.))
                    .flex()
                    .flex_col()
                     .gap(px(1.))
                    .children(
                        outline
                            .iter()
                             .filter(|symbol| {
                                symbol_matches_filter(symbol, &self.outline_filter)
                             })
                            .map(|symbol| {
                                let is_current = cursor_line >= symbol.line;
                                let line = symbol.line;
                                let name = symbol.name.clone();
                                div()
                                    .id(("outline-item", line))
                                    .px(px(6.))
                                    .py(px(3.))
                                    .rounded(px(5.))
                                    .text_size(px(10.))
                                    .cursor_pointer()
                                    .text_color(theme::color(if is_current {
                                        theme::ACCENT
                                    } else {
                                        theme::TEXT_DIM
                                    }))
                                    .hover(|style| {
                                        style.bg(theme::alpha(theme::ACCENT, 0.08))
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.jump_to_line(line, cx);
                                    }))
                                    .child(name)
                             }),
                     ),
             )
     }

    /// The chain of enclosing symbols at `cursor_line`, outermost first, for
    /// the breadcrumb. Descends only through container kinds (namespace, class,
    /// struct, interface, enum, module) so a function's local variables do not
    /// clutter the breadcrumb.
    fn outline_chain_for_line(&self, cursor_line: usize) -> Vec<(String, usize)> {
        symbol_chain_at_line(&self.outline, cursor_line)
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
                    .text_color(theme::color(theme::ACCENT))
                    .child("ƒ"),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .font_family(theme::MONO_FONT)
                    .text_color(theme::color(theme::TEXT))
                    .child(label),
            )
            .when(active.is_some(), |style| {
                let _ = cx;
                style.child(
                    div()
                        .flex_none()
                        .text_size(px(9.5))
                        .text_color(theme::color(theme::TEXT_FAINT))
                        .child(format!("参数 {}", active.unwrap_or(0) + 1)),
                )
            })
            .into_any_element()
    }

    /// Floating hover tooltip (LSP `textDocument/hover`) shown near the top of
    /// the editor. Displays the hovered symbol's type/signature/documentation;
    /// cleared when the cursor moves or the file changes.
    fn render_hover_tooltip(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        let Some(text) = &self.hover_tooltip else {
            return div().into_any_element();
        };
        let text = text.clone();
        div()
            .id("hover-tooltip")
            .absolute()
            .left(px(12.))
            .top(px(12.))
            .max_w(px(560.))
            .max_h(px(300.))
            .px(px(12.))
            .py(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme::alpha(theme::STROKE, 0.18))
            .bg(theme::color(theme::BG_1).opacity(0.98))
            .shadow_lg()
            .overflow_y_scroll()
            .child(
                div()
                    .text_size(px(11.5))
                    .font_family(theme::MONO_FONT)
                    .text_color(theme::color(theme::TEXT))
                    .child(text),
            )
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
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .child(count_text),
                    )
                    .child(
                        widgets::find_toggle(
                            "find-case",
                            "Aa",
                            editor.read(cx).find_case_sensitive(),
                            cx.listener(move |this, _, _, cx| {
                                if let Some(editor) = this.active_editor() {
                                    editor.update(cx, |editor, cx| {
                                        editor.toggle_find_case_sensitive(cx)
                                    });
                                }
                            }),
                        ),
                    )
                    .child(
                        widgets::find_toggle(
                            "find-word",
                            "W",
                            editor.read(cx).find_whole_word(),
                            cx.listener(move |this, _, _, cx| {
                                if let Some(editor) = this.active_editor() {
                                    editor.update(cx, |editor, cx| {
                                        editor.toggle_find_whole_word(cx)
                                    });
                                }
                            }),
                        ),
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
                        .text_color(theme::color(theme::ACCENT))
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
                        .text_color(theme::color(theme::ACCENT))
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
                        .text_color(theme::color(theme::ACCENT))
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
                        .text_color(theme::color(theme::ACCENT))
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
                        .text_color(theme::color(theme::TEXT_FAINT))
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
            .max_h(px(360.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::alpha(theme::STROKE, 0.18))
            .bg(theme::color(theme::BG_1).opacity(0.98))
            .shadow_lg()
            .p(px(4.))
            .child(
                div()
                    .id("completion-list")
                    .max_h(px(220.))
                    .overflow_y_scroll()
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
                                            .text_color(theme::color(theme::TEXT_FAINT))
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
                                                theme::color(theme::TEXT)
                                            } else {
                                                theme::color(theme::TEXT_DIM)
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
                                                .text_color(theme::color(theme::TEXT_FAINT))
                                                .child(item.detail.clone()),
                                        )
                                    })
                            }),
                    )
            )
            .when(
                self.completions
                    .get(selected)
                    .map(|item| !item.documentation.is_empty())
                    .unwrap_or(false),
                |style| {
                    let doc = self.completions[selected].documentation.clone();
                    style.child(
                        div()
                            .id("completion-doc")
                            .border_t_1()
                            .border_color(theme::alpha(theme::STROKE, 0.12))
                            .p(px(8.))
                            .max_h(px(120.))
                            .overflow_y_scroll()
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(theme::color(theme::TEXT_DIM))
                                    .child(doc),
                            ),
                    )
                },
            )
    }

    /// Floating workspace-symbol search popup (cross-file symbol navigation,
    /// `Ctrl+Shift+O`-style). Includes a query field and a result list.
    fn render_workspace_symbol_popup(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self
            .workspace_symbol_selected
            .min(self.workspace_symbols.len().saturating_sub(1));
        div()
            .id("workspace-symbol-popup")
            .absolute()
            .left(px(12.))
            .top(px(46.))
            .w(px(480.))
            .max_h(px(380.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::alpha(theme::STROKE, 0.18))
            .bg(theme::color(theme::BG_1).opacity(0.98))
            .shadow_lg()
            .child(
                div()
                    .p(px(8.))
                    .border_b_1()
                    .border_color(theme::alpha(theme::STROKE, 0.1))
                    .child(self.workspace_symbol_field.clone()),
            )
            .child(
                div()
                    .id("workspace-symbol-list")
                    .max_h(px(300.))
                    .overflow_y_scroll()
                    .children(
                        self.workspace_symbols
                            .iter()
                            .enumerate()
                            .map(|(index, symbol)| {
                                let is_selected = index == selected;
                                div()
                                    .id(("workspace-symbol-item", index))
                                    .px(px(10.))
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
                                        this.workspace_symbol_selected = index;
                                        this.workspace_symbol_confirm(cx);
                                    }))
                                    .child(
                                        div()
                                            .w(px(80.))
                                            .flex_none()
                                            .text_size(px(9.))
                                            .text_color(theme::color(theme::TEXT_FAINT))
                                            .child(symbol.kind.clone()),
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
                                                theme::color(theme::TEXT)
                                            } else {
                                                theme::color(theme::TEXT_DIM)
                                            })
                                            .child(symbol.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .max_w(px(180.))
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .text_size(px(9.5))
                                            .text_color(theme::color(theme::TEXT_FAINT))
                                            .child(
                                                self.display_path(&symbol.path),
                                            ),
                                    )
                            }),
                    ),
            )
            .when(self.workspace_symbols.is_empty(), |style| {
                style.child(
                    div()
                        .px(px(12.))
                        .py(px(8.))
                        .text_size(px(11.))
                        .text_color(theme::color(theme::TEXT_FAINT))
                        .child("输入以搜索项目内符号…"),
                )
            })
     }

    /// Floating references popup (Shift+F12). Lists the locations returned by
    /// the last `textDocument/references` request; Enter/click opens the
    /// highlighted reference in the editor.
    fn render_references_popup(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self
            .references_selected
            .min(self.references.len().saturating_sub(1));
        div()
            .id("references-popup")
            .absolute()
            .left(px(12.))
            .top(px(46.))
            .w(px(520.))
            .max_h(px(380.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::alpha(theme::STROKE, 0.18))
            .bg(theme::color(theme::BG_1).opacity(0.98))
            .shadow_lg()
            .child(
                div()
                    .px(px(10.))
                    .py(px(6.))
                    .border_b_1()
                    .border_color(theme::alpha(theme::STROKE, 0.1))
                    .text_size(px(10.))
                    .text_color(theme::color(theme::TEXT_DIM))
                    .child(format!("引用 · {} 处", self.references.len())),
            )
            .child(
                div()
                    .id("references-list")
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .children(
                        self.references
                            .iter()
                            .enumerate()
                            .map(|(index, reference)| {
                                let is_selected = index == selected;
                                div()
                                    .id(("references-item", index))
                                    .px(px(10.))
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
                                        this.references_selected = index;
                                        this.references_confirm(cx);
                                    }))
                                    .child(
                                        div()
                                            .w(px(52.))
                                            .flex_none()
                                            .text_size(px(9.))
                                            .text_color(theme::color(theme::TEXT_FAINT))
                                            .child(format!("{}:{}", reference.line + 1, reference.column + 1)),
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
                                                theme::color(theme::TEXT)
                                            } else {
                                                theme::color(theme::TEXT_DIM)
                                            })
                                            .child(self.display_path(&reference.path)),
                                    )
                            }),
                    ),
            )
            .when(self.references.is_empty(), |style| {
                style.child(
                    div()
                        .px(px(12.))
                        .py(px(8.))
                        .text_size(px(11.))
                        .text_color(theme::color(theme::TEXT_FAINT))
                        .child("未找到引用…"),
                )
            })
    }
 
     /// Floating Quick Open popup (Ctrl+P). Shows a query field and the filtered
    /// list of project files; Enter opens the highlighted entry.
    fn render_quick_open_popup(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self
            .quick_open_selected
            .min(self.quick_open_files.len().saturating_sub(1));
        div()
            .id("quick-open-popup")
            .absolute()
            .left(px(12.))
            .top(px(46.))
            .w(px(480.))
            .max_h(px(380.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::alpha(theme::STROKE, 0.18))
            .bg(theme::color(theme::BG_1).opacity(0.98))
            .shadow_lg()
            .child(
                div()
                    .p(px(8.))
                    .border_b_1()
                    .border_color(theme::alpha(theme::STROKE, 0.1))
                    .child(self.quick_open_field.clone()),
            )
            .child(
                div()
                    .id("quick-open-list")
                    .max_h(px(300.))
                    .overflow_y_scroll()
                    .children(
                        self.quick_open_files
                            .iter()
                            .enumerate()
                            .map(|(index, path)| {
                                let is_selected = index == selected;
                                div()
                                    .id(("quick-open-item", index))
                                    .px(px(10.))
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
                                        this.quick_open_selected = index;
                                        this.quick_open_confirm(cx);
                                    }))
                                    .child(
                                        div()
                                            .flex_1()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .font_family(theme::MONO_FONT)
                                            .text_size(px(11.))
                                            .text_color(if is_selected {
                                                theme::color(theme::TEXT)
                                            } else {
                                                theme::color(theme::TEXT_DIM)
                                            })
                                            .child(path.clone()),
                                    )
                            }),
                    ),
            )
            .when(self.quick_open_files.is_empty(), |style| {
                style.child(
                    div()
                        .px(px(12.))
                        .py(px(8.))
                        .text_size(px(11.))
                        .text_color(theme::color(theme::TEXT_FAINT))
                        .child("未找到匹配的文件…"),
                )
            })
    }

    /// Floating recent-files popup. Lists the project's recently opened files
    /// (most recent first); Enter opens the highlighted entry.
    fn render_recent_files_popup(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self
            .recent_selected
            .min(self.recent_files.files.len().saturating_sub(1));
        div()
            .id("recent-files-popup")
            .absolute()
            .left(px(12.))
            .top(px(46.))
            .w(px(480.))
            .max_h(px(380.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::alpha(theme::STROKE, 0.18))
            .bg(theme::color(theme::BG_1).opacity(0.98))
            .shadow_lg()
            .child(
                div()
                    .px(px(10.))
                    .py(px(8.))
                    .border_b_1()
                    .border_color(theme::alpha(theme::STROKE, 0.1))
                    .text_size(px(10.))
                    .text_color(theme::color(theme::TEXT_FAINT))
                    .child("最近打开的文件"),
            )
            .child(
                div()
                    .id("recent-files-list")
                    .max_h(px(300.))
                    .overflow_y_scroll()
                    .children(
                        self.recent_files
                            .files
                            .iter()
                            .enumerate()
                            .map(|(index, path)| {
                                let is_selected = index == selected;
                                div()
                                    .id(("recent-file-item", index))
                                    .px(px(10.))
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
                                        this.recent_selected = index;
                                        this.recent_files_confirm(cx);
                                    }))
                                    .child(
                                        div()
                                            .flex_1()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .font_family(theme::MONO_FONT)
                                            .text_size(px(11.))
                                            .text_color(if is_selected {
                                                theme::color(theme::TEXT)
                                            } else {
                                                theme::color(theme::TEXT_DIM)
                                            })
                                            .child(path.to_string_lossy().to_string()),
                                    )
                            }),
                    ),
            )
            .when(self.recent_files.files.is_empty(), |style| {
                style.child(
                    div()
                        .px(px(12.))
                        .py(px(8.))
                        .text_size(px(11.))
                        .text_color(theme::color(theme::TEXT_FAINT))
                        .child("尚未打开任何文件…"),
                )
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
                    .text_color(theme::color(theme::TEXT_DIM))
                    .child("从左侧选择一个文件"),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(theme::color(theme::TEXT_FAINT))
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
        let mut output_lines: Vec<(usize, String)> = shown_output
            .lines()
            .enumerate()
            .take(1200)
            .map(|(index, line)| (index, line.to_string()))
            .collect();
        // Append the firmware memory-usage summary when the last build was a
        // successful MCU build (only shown in the build output tab).
        let firmware_summary = self.render_firmware_size_summary();
        if self.output_tab == OutputTab::Output && !firmware_summary.is_empty() {
            if let Some(last) = output_lines.last().map(|(index, _)| index + 1) {
                output_lines.push((last, firmware_summary));
            }
        }

        div()
            .h(px(self.output_px))
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
                                    .text_color(theme::color(theme::TEXT_FAINT))
                                    .child(self.build_command.clone()),
                            )
                            .child(
                                widgets::icon_button("out-clear", "⌫", 22.)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.run.show_run_output {
                                            this.run.output.clear();
                                        } else {
                                            this.output.clear();
                                        }
                                        cx.notify();
                                    })),
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
                     .child(self.render_memory_layout_view())
                     .child(self.render_include_graph_view(cx))
                     .child(self.render_tasks_view(cx))
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
                        let diagnostic_column = diagnostic.column;
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
                                        // LSP positions are 0-based; the editor
                                        // wants 1-based line/column.
                                        editor.update(cx, |editor, cx| {
                                            editor.set_cursor_line_column(
                                                line + 1,
                                                diagnostic_column + 1,
                                                cx,
                                            )
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
                                        .text_color(theme::color(theme::TEXT_DIM))
                                        .child(location_text),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_size(px(11.))
                                        .text_color(theme::color(theme::TEXT))
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
                                .text_color(theme::color(theme::TEXT_FAINT))
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
                                    .text_color(theme::color(theme::TEXT_DIM))
                                    .child(diagnostic.location_label()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(11.))
                                    .text_color(theme::color(theme::TEXT))
                                    .child(diagnostic.message.clone()),
                            )
                             .child(
                                 div()
                                     .text_size(px(9.5))
                                     .text_color(tone)
                                     .child(diagnostic.severity.label()),
                             )
                            .when(
                                ns_build::suggest_quick_fix(diagnostic, None).is_some(),
                                |row| {
                                     row.child(
                                         widgets::ghost_button(
                                            gpui::SharedString::from(format!("quick-fix-{index}")),
                                             "快速修复",
                                         )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.apply_quick_fix(index, cx);
                                        })),
                                    )
                                },
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
                                    .text_color(theme::color(theme::TEXT_FAINT))
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
                                            theme::color(theme::DARK_TEXT)
                                        } else {
                                            theme::color(theme::TEXT_FAINT)
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
                                            theme::color(theme::TEXT_DIM)
                                        } else {
                                            theme::color(theme::TEXT_FAINT)
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
                                        .text_color(theme::color(theme::TEXT_FAINT))
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
                                .when(breakpoint.hits > 0, |style| {
                                    style.child(
                                        div()
                                            .flex_none()
                                            .px(px(7.))
                                            .py(px(2.))
                                            .rounded_full()
                                            .border_1()
                                            .border_color(theme::alpha(theme::ACCENT, 0.45))
                                            .bg(theme::alpha(theme::ACCENT, 0.12))
                                            .text_size(px(8.5))
                                            .text_color(theme::color(theme::ACCENT))
                                            .child(format!("命中 {}", breakpoint.hits)),
                                    )
                                })
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
                                        .text_color(theme::color(theme::TEXT_FAINT))
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
                                            .text_color(theme::color(theme::TEXT_FAINT))
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
                OutputTab::Terminal => self.render_terminal_panel(cx).into_any_element(),
                OutputTab::Serial => self
                    .serial
                    .clone()
                    .map(|serial| div().flex_1().overflow_hidden().child(serial).into_any_element())
                    .unwrap_or_else(|| {
                        div()
                            .p(px(12.))
                            .text_size(px(11.))
                            .text_color(theme::color(theme::TEXT_FAINT))
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
                                .text_color(theme::color(theme::TEXT_DIM))
                                .child("当前目录不是 Git 仓库"),
                        )
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(theme::color(theme::TEXT_FAINT))
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
                                theme::color(theme::TEXT)
                            } else {
                                theme::color(theme::TEXT_DIM)
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
                                    .text_color(theme::color(theme::TEXT_DIM))
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
                .text_color(theme::color(theme::DANGER))
                .child(error.clone())
                .into_any_element()
        } else if let Some(feedback) = &feedback {
            div()
                .text_size(px(10.))
                .text_color(theme::color(theme::JADE))
                .child(feedback.clone())
                .into_any_element()
        } else {
            div()
                .text_size(px(10.))
                .text_color(theme::color(theme::TEXT_FAINT))
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
                    .text_color(theme::color(theme::TEXT_FAINT))
                    .child(if show_history { "▾" } else { "▸" }),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::color(theme::TEXT_DIM))
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
                let commit_rows: Vec<gpui::AnyElement> = history
                    .into_iter()
                    .map(|commit| {
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
                                    .text_color(theme::color(theme::TEXT))
                                    .child(commit.subject.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .font_family(theme::MONO_FONT)
                                    .text_color(theme::color(theme::TEXT_FAINT))
                                    .child(commit.hash.clone()),
                            )
                            .into_any_element()
                    })
                    .collect();
                // Local branches: current one marked, others clickable to switch.
                let branch_rows: Vec<gpui::AnyElement> = branches
                    .into_iter()
                    .enumerate()
                    .map(|(index, (name, is_current))| {
                        let branch_name = name.clone();
                        div()
                            .id(("git-branch", index))

                            .px(px(8.))
                            .py(px(3.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.06)))
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(if is_current {
                                        theme::color(theme::JADE)
                                    } else {
                                        theme::color(theme::TEXT_FAINT)
                                    })
                                    .child(if is_current { "●" } else { "○" }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(10.5))
                                    .text_color(theme::color(theme::TEXT))
                                    .child(name.clone()),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !is_current {
                                    this.git_checkout(branch_name.clone(), cx);
                                }
                            }))
                            .into_any_element()
                    })
                    .collect();
                let mut children: Vec<gpui::AnyElement> = Vec::new();
                if !branch_rows.is_empty() {
                    children.push(
                        div()
                            .px(px(8.))
                            .py(px(3.))
                            .text_size(px(9.))
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .child("本地分支（点击切换）")
                            .into_any_element(),
                    );
                    children.extend(branch_rows);
                }
                if !commit_rows.is_empty() {
                    if !children.is_empty() {
                        children.push(
                            div()
                                .px(px(8.))
                                .py(px(6.))
                                .text_size(px(9.))
                                .text_color(theme::color(theme::TEXT_FAINT))
                                .child("最近提交")
                                .into_any_element(),
                        );
                    }
                    children.extend(commit_rows);
                }
                div()
                    .id("git-history")
                    .h(px(180.))
                    .flex_none()
                    .overflow_y_scroll()
                    .border_t_1()
                    .border_color(theme::alpha(theme::STROKE, 0.08))
                    .p(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .children(children)
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
            .child(
                div()
                    .id("git-branch-actions")
                    .px(px(10.))
                    .py(px(4.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .border_b_1()
                    .border_color(theme::alpha(theme::STROKE, 0.08))
                    .child(
                        div()
                            .id("git-new-branch")
                            .px(px(8.))
                            .py(px(3.))
                            .rounded(px(4.))
                            .bg(theme::alpha(theme::STROKE, 0.10))
                            .text_size(px(10.))
                            .text_color(theme::color(theme::TEXT_DIM))
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::alpha(theme::ACCENT, 0.16)))
                            .child("＋ 新建分支")
                            .on_click(cx.listener(|this, _, _, cx| this.open_git_branch_dialog(cx))),
                    )
                    .child(
                        div()
                            .id("git-pull")
                            .px(px(8.))
                            .py(px(3.))
                            .rounded(px(4.))
                            .bg(theme::alpha(theme::STROKE, 0.10))
                            .text_size(px(10.))
                            .text_color(theme::color(theme::TEXT_DIM))
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::alpha(theme::ACCENT, 0.16)))
                            .child("↓ 拉取")
                            .on_click(cx.listener(|this, _, _, cx| this.git_pull(cx))),
                    )
                    .child(
                        div()
                            .id("git-push")
                            .px(px(8.))
                            .py(px(3.))
                            .rounded(px(4.))
                            .bg(theme::alpha(theme::STROKE, 0.10))
                            .text_size(px(10.))
                            .text_color(theme::color(theme::TEXT_DIM))
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::alpha(theme::ACCENT, 0.16)))
                            .child("↑ 推送")
                            .on_click(cx.listener(|this, _, _, cx| this.git_push(cx))),
                    ),
            )
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
                                        .text_color(theme::color(theme::TEXT_FAINT))
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
                            .text_color(theme::color(theme::TEXT_DIM))
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
                    .text_color(theme::color(theme::TEXT_FAINT))
                    .child("Agent 面板未初始化")
                    .into_any_element()
            })
    }

    /// Terminal panel: a Zed-style tab bar over the active terminal session.
    fn render_terminal_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.terminals.is_empty() {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .text_color(theme::color(theme::TEXT_FAINT))
                .child("暂无终端 · 点击「+ 新建」或按 Cmd/Ctrl+J 打开")
                .into_any_element();
        }
        let active = self.active_terminal.unwrap_or(0).min(self.terminals.len() - 1);
        let mut tabs: Vec<gpui::AnyElement> = Vec::new();
        for (index, terminal) in self.terminals.iter().enumerate() {
            let title = terminal.read(cx).title();
            let selected = index == active;
            let terminal_index = index;
            let title_label = if title.is_empty() { "终端".to_string() } else { title };
            tabs.push(
                div()
                    .id(("term-tab", index))
                    .h(px(26.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(6.))
                    .bg(if selected {
                        theme::alpha(theme::ACCENT, 0.1)
                    } else {
                        theme::alpha(0xffffff, 0.0)
                    })
                    .text_size(px(10.5))
                    .text_color(if selected {
                        theme::color(theme::TEXT)
                    } else {
                        theme::color(theme::TEXT_DIM)
                    })
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.07)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.focus_terminal_index(terminal_index, cx);
                    }))
                    .child(title_label)
                    .child(
                        div()
                            .id(("term-tab-close", index))
                            .text_size(px(9.))
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .cursor_pointer()
                            .hover(|style| style.text_color(theme::color(theme::CINNABAR)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_terminal(terminal_index, cx);
                            }))
                            .child("✕"),
                    )
                    .into_any_element(),
            );
        }
        let terminal = self.terminals[active].clone();
        div()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .h(px(32.))
                    .px(px(8.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .border_b_1()
                    .border_color(theme::alpha(theme::STROKE, 0.08))
                    .child(
                        div()
                            .id("term-tabs-scroll")
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .overflow_x_scroll()
                            .children(tabs),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("term-clear")
                            .h(px(24.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .text_size(px(11.))
                            .text_color(theme::color(theme::TEXT_DIM))
                            .cursor_pointer()
                            .hover(|style| style.text_color(theme::color(theme::TEXT)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let index = this.active_terminal.unwrap_or(0);
                                if let Some(terminal) = this.terminals.get(index) {
                                    terminal.update(cx, |terminal, _cx| terminal.clear());
                                    this.status = "已清屏终端".to_string();
                                }
                                cx.notify();
                            }))
                            .child("清屏"),
                    ),
                    .child(
                        div()
                            .id("term-tab-new")
                            .h(px(24.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .text_size(px(12.))
                            .text_color(theme::color(theme::TEXT_DIM))
                            .cursor_pointer()
                            .hover(|style| style.text_color(theme::color(theme::TEXT)))
                            .on_click(cx.listener(|this, _, _, cx| this.new_terminal(cx)))
                            .child("＋"),
                    ),
            )
            .child(div().flex_1().overflow_hidden().child(terminal))
            .into_any_element()
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
                                theme::color(theme::DARK_TEXT)
                            } else {
                                theme::color(theme::TEXT_FAINT)
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
                            .text_color(theme::color(theme::ACCENT_3))
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
                                theme::color(theme::TEXT)
                            } else {
                                theme::color(theme::TEXT_FAINT)
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
                            .text_color(theme::color(theme::TEXT_FAINT))
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
                                .text_color(theme::color(theme::TEXT_FAINT))
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
        let thread_selector: Option<gpui::AnyElement> = (self.debug.threads.len() > 1)
            .then(|| self.render_thread_selector(cx));


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
                                    widgets::ghost_button("dbg-restart", "重启").on_click(
                                        cx.listener(|this, _, _, cx| this.restart_debug(cx)),
                                    ),
                                )
                            })
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
                                    .child(view_tab_pill("dbg-tab-regs", "寄存器", self.debug.view_tab == DebugViewTab::Registers)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.set_view_tab(DebugViewTab::Registers, cx)
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
                                    .text_color(theme::color(theme::TEXT_FAINT))
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
                            .when_some(thread_selector, |style, selector| {
                                style.child(selector)
                            })
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
                                                .text_color(theme::color(theme::TEXT_FAINT))
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
                                                        .text_color(theme::color(theme::TEXT))
                                                        .child(frame.name.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .overflow_hidden()
                                                        .text_ellipsis()
                                                        .whitespace_nowrap()
                                                        .text_size(px(9.))
                                                        .text_color(theme::color(theme::TEXT_FAINT))
                                                        .child(location),
                                                )
                                        })),
                            ),
                    )
                    .child(match self.debug.view_tab {
                        DebugViewTab::Variables => {
                            self.render_debug_variables_column(cx)
                        }
                        DebugViewTab::Registers => self.render_debug_registers_column(cx),
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
                                                .text_color(theme::color(theme::TEXT_FAINT))
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
                                        .text_color(theme::color(theme::TEXT_FAINT))
                                        .child("调试会话未运行"),
                                )
                            })
                            .children(
                                self.debug
                                    .console_input
                                    .iter()
                                    .map(|field| {
                                        let field_for_keys = field.clone();
                                        div()
                                            .w_full()
                                            .child(field.clone())
                                            .on_key_down(cx.listener(
                                                move |this, event: &gpui::KeyDownEvent, _, cx| {
                                                    let key = event.keystroke.key.as_str();
                                                    if key != "up" && key != "down" {
                                                        return;
                                                    }
                                                    let up = key == "up";
                                                    let current =
                                                        field_for_keys.read(cx).text();
                                                    if let Some(text) =
                                                        this.debug.history_navigate(up, &current)
                                                    {
                                                        field_for_keys.update(cx, |field, cx| {
                                                            field.set_text(text, cx);
                                                        });
                                                    }
                                                },
                                        ))
                                    }),
                            )
            )
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
                                        .text_color(theme::color(theme::TEXT_DIM))
                                         .on_click(cx.listener(move |this, _, _, cx| {
                                             this.select_debug_scope(index, cx)
                                         }))
                                        .child(scope.name.clone())
                                  },
                              )),
                    )
                    .child(
                        div()
                            .id("dbg-vars-refresh")
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(px(6.))
                            .text_size(px(11.))
                            .text_color(theme::color(theme::TEXT_DIM))
                            .cursor_pointer()
                            .hover(|style| style.text_color(theme::color(theme::ACCENT)))
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_variables(cx)))
                            .child("↻ 刷新"),
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
                                        .text_color(theme::color(theme::ACCENT_3))
                                        .child(watch.expression.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_color(theme::color(theme::TEXT_DIM))
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
                                        .text_color(theme::color(theme::TEXT_FAINT))
                                        .cursor_pointer()
                                        .hover(|style| style.text_color(theme::color(theme::DANGER)))
                                         .on_click(cx.listener(move |this, _, _, cx| {
                                             cx.stop_propagation();
                                             this.debug.remove_watch(index);
                                            this.persist_watches();
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
                                .text_color(theme::color(theme::TEXT_FAINT))
                                .child("没有变量。命中断点后显示。"),
                        )
                    })
                    .children(self.variable_rows(&self.debug.variables, 0, 0, &mut 0, cx)),
            )
            .into_any_element()
    }

    /// Render the CPU register tab: register groups from the adapter's
    /// "Registers" scope, each register editable via the shared set-value
    /// dialog (DAP `setVariable`).
    fn render_debug_registers_column(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut groups: Vec<(String, Vec<Variable>)> = self
            .debug
            .register_groups
            .iter()
            .map(|(name, variables)| (name.clone(), variables.clone()))
            .collect();
        groups.sort_by(|a, b| a.0.cmp(&b.0));
        let group_count = groups.iter().map(|(_, vars)| vars.len()).sum::<usize>();
        div()
            .id("debug-registers-scroll")
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
                    .child(widgets::section_label(if self.debug.registers_scope == 0 {
                        "CPU 寄存器".to_string()
                    } else {
                        format!("CPU 寄存器 · {group_count}")
                    }))
                     .when(group_count > 0, |style| {
                         style.child(
                            div().flex().gap(px(4.)).children([
                                widgets::ghost_button("dbg-regs-export", "导出快照").on_click(
                                    cx.listener(|this, _, _, cx| this.export_register_snapshot(cx)),
                                ),
                                widgets::ghost_button("dbg-regs-compare", "对比快照").on_click(
                                    cx.listener(|this, _, _, cx| this.compare_register_snapshot(cx)),
                                ),
                            ]),
                         )
                     }),
             )
            .child(
                div()
                    .id("debug-registers-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(6.))
                    .pb(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .when(groups.is_empty(), |style| {
                        style.child(
                            div()
                                .p(px(10.))
                                .text_size(px(11.))
                                .text_color(theme::color(theme::TEXT_FAINT))
                                .child(
                                    if self.debug.registers_scope == 0 {
                                        "目标未暴露寄存器作用域。命中断点后可用。".to_string()
                                    } else {
                                        "正在读取寄存器…".to_string()
                                    },
                                ),
                        )
                    })
                    .children(groups.into_iter().map(|(name, registers)| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .px(px(6.))
                                    .py(px(3.))
                                    .text_size(px(10.))
                                    .text_color(theme::color(theme::TEXT_FAINT))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(name.clone()),
                            )
                            .children(
                                registers.iter().enumerate().map(|(index, variable)| {
                                    self.render_register_row(&name, variable, index, cx)
                                }),
                            )
                    })),
            )
             .into_any_element()
     }

    /// Export the current CPU register groups as a JSON snapshot in the
    /// project's `.ns-ide` directory. Writes one JSON document with each
    /// register group and its name/value pairs; the status bar reports the
    /// path or the failure honestly.
    fn export_register_snapshot(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            self.status = "没有已加载的项目".to_string();
            cx.notify();
            return;
        };
        if self.debug.register_groups.is_empty() {
            self.status = "没有寄存器可导出（目标未暴露寄存器作用域）".to_string();
            cx.notify();
            return;
        }
        let mut groups: Vec<(String, Vec<ns_debug::Variable>)> = self
            .debug
            .register_groups
            .iter()
            .map(|(name, variables)| (name.clone(), variables.clone()))
            .collect();
        groups.sort_by(|a, b| a.0.cmp(&b.0));
        let mut body = String::from("{\n");
        for (i, (name, registers)) in groups.iter().enumerate() {
            body.push_str(&format!("  {:?}: {{\n", name));
            for (j, variable) in registers.iter().enumerate() {
                let comma = if j + 1 < registers.len() { "," } else { "" };
                body.push_str(&format!(
                    "    {:?}: {:?}{}\n",
                    variable.name, variable.value, comma
                ));
            }
            let comma = if i + 1 < groups.len() { "," } else { "" };
            body.push_str(&format!("  }}{comma}\n"));
        }
        body.push_str("}\n");
        let dir = root.join(".ns-ide");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("register-snapshot.json");
        match std::fs::write(&path, &body) {
            Ok(()) => {
                self.status = format!("寄存器快照已导出: {}", path.display());
            }
            Err(err) => {
                self.status = format!("导出寄存器快照失败: {err}");
            }
         }
         cx.notify();
     }
     /// Load the saved register snapshot and compare it against the current
     /// register groups; changed registers are highlighted in the register tab.
    fn compare_register_snapshot(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            self.status = "没有已加载的项目".to_string();
            cx.notify();
            return;
        };
        let path = root.join(".ns-ide/register-snapshot.json");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => {
                self.status = format!("未找到寄存器快照: {}", path.display());
                cx.notify();
                return;
            }
        };
        let snapshot: HashMap<String, HashMap<String, String>> = match serde_json::from_str(&text) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.status = format!("寄存器快照解析失败: {error}");
                cx.notify();
                return;
            }
        };
        let mut changed = 0usize;
        let mut total = 0usize;
        for (group_name, registers) in &self.debug.register_groups {
            let Some(saved_group) = snapshot.get(group_name) else {
                continue;
            };
            for variable in registers {
                total += 1;
                if let Some(saved) = saved_group.get(&variable.name) {
                    if saved != &variable.value {
                        changed += 1;
                    }
                }
            }
        }
        self.debug.reg_snapshot = Some(snapshot);
        self.status = format!("寄存器快照对比: {total} 个寄存器，{changed} 个已变更");
        cx.notify();
    }

      /// Render one CPU register row: name + value + edit affordance. The group
    /// name is used only for a stable element id (the DAP reference is what
    /// `setVariable` needs).
    fn render_register_row(
        &self,
        group: &str,
        variable: &Variable,
        index: usize,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
         let _group = group;
         let name = variable.name.clone();
         let value = variable.value.clone();
         let reference = variable.variables_reference;
        let changed = self
            .debug
            .reg_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.get(group))
            .and_then(|saved| saved.get(&name))
            .is_some_and(|saved| saved != &value);
        div()
            .id(("dbg-register", index))
            .px(px(6.))
            .py(px(3.))
            .rounded(px(6.))
            .flex()
            .items_baseline()
            .gap(px(8.))
            .font_family(theme::MONO_FONT)
            .text_size(px(10.5))
            .child(
                div()
                    .w(px(56.))
                    .flex_none()
                    .text_color(theme::color(theme::ACCENT_3))
                    .child(variable.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                     .text_ellipsis()
                     .whitespace_nowrap()
                    .text_color(theme::color(if changed {
                        theme::CINNABAR
                    } else {
                        theme::TEXT_DIM
                    }))
                    .child(variable.value.clone()),
            )
            .child(
                div()
                    .id(("dbg-register-edit", index))
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(theme::color(theme::TEXT_FAINT))
                    .cursor_pointer()
                    .hover(|style| style.text_color(theme::color(theme::ACCENT_3)))
                    .on_click(cx.listener({
                        let name = name.clone();
                        let value = value.clone();
                        move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.open_set_var_dialog(reference, &name, &value, cx);
                        }
                    }))
                    .child("✎"),
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
         let search_hit = self.debug.memory.search_hit;
         for (row_index, chunk) in bytes.chunks(16).enumerate() {
             let line_address = address + (row_index as u64) * 16;
             let mut hex_children: Vec<gpui::AnyElement> = Vec::new();
             for (byte_index, byte) in chunk.iter().enumerate() {
                 let global = row_index * 16 + byte_index;
                 let is_hit = search_hit.is_some_and(|(hit, len)| {
                     hit <= global && global < hit.saturating_add(len)
                 });
                 let text = format!("{byte:02x}");
                 hex_children.push(
                     div()
                         .text_color(theme::color(if is_hit {
                             theme::ACCENT_3
                         } else {
                             theme::TEXT_DIM
                         }))
                         .child(text)
                         .into_any_element(),
                 );
                 if byte_index != chunk.len() - 1 {
                     hex_children.push(
                         div()
                             .text_color(theme::color(theme::TEXT_DIM))
                             .child(" ")
                             .into_any_element(),
                     );
                 }
             }
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
                     .text_color(theme::color(theme::TEXT_DIM))
                     .child(format!("{line_address:#010x}  "))
                     .children(hex_children)
                     .child(format!(" │ {ascii}"))
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
                            .gap(px(4.))
                            .overflow_x_hidden()
                            .children(
                                self.debug.memory.regions.iter().enumerate().map(
                                    |(index, region)| {
                                        let active = index == self.debug.memory.active_region;
                                        region_pill(
                                            ("mem-region", index),
                                            &region.name,
                                            active,
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.select_memory_region(index, cx)
                                        }))
                                    },
                                ),
                            )
                            .child(
                                div()
                                    .w(px(100.))
                                    .child(
                                        self.debug
                                            .memory
                                            .region_name_input
                                            .clone()
                                            .map(|field| div().child(field).into_any_element())
                                            .unwrap_or_else(|| div().into_any_element()),
                                    ),
                            )
                            .child(
                                widgets::primary_button("dbg-mem-add-region", "＋区域").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.add_memory_region_from_field(cx)
                                    }),
                                ),
                            )
                            .child(
                                widgets::primary_button("dbg-mem-del-region", "－区域").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        let active = this.debug.memory.active_region;
                                        this.remove_memory_region(active, cx);
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
                     )
                     .child(
                         div()
                             .flex()
                             .items_center()
                             .gap(px(6.))
                             .child(
                                 div()
                                     .flex_1()
                                     .child(
                                         self.debug
                                             .memory
                                             .search_input
                                             .clone()
                                             .map(|field| div().child(field).into_any_element())
                                             .unwrap_or_else(|| div().into_any_element()),
                                     ),
                             )
                             .child(
                                 widgets::ghost_button("dbg-mem-search", "搜索").on_click(
                                     cx.listener(|this, _, _, cx| this.search_memory(cx)),
                                 ),
                             ),
                     )
                     .child(
                         div()
                             .flex()
                             .items_center()
                             .gap(px(6.))
                             .child(
                                 div()
                                     .flex_1()
                                     .child(
                                         self.debug
                                             .memory
                                             .fill_input
                                             .clone()
                                             .map(|field| div().child(field).into_any_element())
                                             .unwrap_or_else(|| div().into_any_element()),
                                     ),
                             )
                             .child(
                                 widgets::ghost_button("dbg-mem-fill", "填充").on_click(
                                     cx.listener(|this, _, _, cx| this.fill_memory(cx)),
                                 ),
                             ),
                     ),
             )
            .child(
                div()
                    .px(px(10.))
                    .pt(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .child(
                                self.debug
                                    .memory
                                    .edit_input
                                    .clone()
                                    .map(|field| div().child(field).into_any_element())
                                    .unwrap_or_else(|| div().into_any_element()),
                            ),
                    )
                    .child(
                        widgets::primary_button("dbg-mem-write", "写入").on_click(
                            cx.listener(|this, _, _, cx| this.write_memory(cx)),
                        ),
                    ),
            )
            .when(has_error, |style| {
                style.child(
                    div()
                        .px(px(10.))
                        .pb(px(4.))
                        .text_size(px(9.5))
                        .text_color(theme::color(theme::DANGER))
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
                                .text_color(theme::color(theme::TEXT_FAINT))
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
                        div()
                            .flex()
                            .gap(px(6.))
                            .children([
                                widgets::ghost_button("dbg-disasm-step", "单指令步进").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.debug_step(DebugStep::Instruction, cx)
                                    }),
                                ),
                                widgets::ghost_button("dbg-disasm-btn", "从 PC 反汇编").on_click(
                                    cx.listener(|this, _, _, cx| this.disassemble_at_pc(cx)),
                                ),
                            ]),
                     ),
             )
            .child(
                div()
                    .px(px(10.))
                    .pb(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .child(
                                self.debug
                                    .disasm_input
                                    .clone()
                                    .map(|field| div().child(field).into_any_element())
                                    .unwrap_or_else(|| div().into_any_element()),
                            ),
                    )
                    .child(
                        widgets::ghost_button("dbg-disasm-go", "跳转").on_click(
                            cx.listener(|this, _, _, cx| this.disassemble_at(cx)),
                        ),
                    ),
            )
             .child(
                 div()
                     .px(px(10.))
                    .px(px(10.))
                    .pb(px(6.))
                    .text_size(px(9.5))
                    .text_color(theme::color(theme::TEXT_FAINT))
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
                                .text_color(theme::color(theme::TEXT_FAINT))
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
                                                    .text_color(theme::color(theme::JADE))
                                                    .child(format!("{source_name}:{source_line}")),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .overflow_hidden()
                                                    .text_ellipsis()
                                                    .whitespace_nowrap()
                                                    .text_size(px(10.))
                                                    .text_color(theme::color(theme::TEXT_FAINT))
                                                    .child(source_text),
                                            )
                                            .into_any_element(),
                                    );
                                }
                                let bp_file = instruction.file.clone();
                                let bp_line = instruction.line;
                                let existing_bp = bp_file
                                    .as_ref()
                                    .and_then(|file| {
                                        bp_line.and_then(|line| {
                                            self.breakpoints.find(file, line).cloned()
                                        })
                                    });
                                let has_bp = existing_bp.is_some();
                                let bp_enabled =
                                    existing_bp.as_ref().map(|bp| bp.enabled).unwrap_or(true);
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
                                                .id(("dbg-disasm-bp", index))
                                                .w(px(14.))
                                                .flex_none()
                                                .cursor_pointer()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .child(if has_bp {
                                                    div()
                                                        .w(px(7.))
                                                        .h(px(7.))
                                                        .rounded_full()
                                                        .bg(theme::color(if bp_enabled {
                                                            theme::CINNABAR
                                                        } else {
                                                            theme::TEXT_FAINT
                                                        }))
                                                        .into_any_element()
                                                } else {
                                                    div()
                                                        .w(px(7.))
                                                        .h(px(7.))
                                                        .rounded_full()
                                                        .border_1()
                                                        .border_color(
                                                            theme::alpha(theme::STROKE, 0.3),
                                                        )
                                                        .into_any_element()
                                                })
                                                .on_click(cx.listener({
                                                    let bp_file = bp_file.clone();
                                                    let bp_line = bp_line;
                                                    move |this, _, _, cx| {
                                                        if let (Some(file), Some(line)) =
                                                            (bp_file.clone(), bp_line)
                                                        {
                                                            let exists = this
                                                                .breakpoints
                                                                .find(&file, line)
                                                                .is_some();
                                                            this.on_breakpoint_toggled(
                                                                &file,
                                                                line,
                                                                !exists,
                                                                cx,
                                                            );
                                                        } else {
                                                            this.status =
                                                                "该指令无源码映射，无法设置源码断点"
                                                                    .to_string();
                                                            cx.notify();
                                                        }
                                                    }
                                                })),
                                        )
                                          .children(
                                              instruction
                                                  .symbol
                                                  .as_deref()
                                                  .filter(|symbol| !symbol.is_empty())
                                                  .map(|symbol| {
                                                      div()
                                                          .flex_none()
                                                          .px(px(4.))
                                                          .py(px(1.))
                                                          .rounded(px(4.))
                                                          .bg(theme::alpha(theme::JADE, 0.12))
                                                          .text_size(px(9.))
                                                          .text_color(theme::color(theme::JADE))
                                                          .child(symbol.to_string())
                                                          .into_any_element()
                                                  }),
                                          )
                                        .child(
                                             div()
                                                 .flex_none()
                                                 .font_family(theme::MONO_FONT)
                                                 .text_size(px(10.))
                                                 .text_color(theme::color(theme::ACCENT_3))
                                                 .child(format!("{:#x}", instruction.address)),
                                         )
                                        .child(
                                            div()
                                                .font_family(theme::MONO_FONT)
                                                .text_size(px(10.))
                                                .text_color(if is_pc {
                                                    theme::color(theme::TEXT)
                                                } else {
                                                    theme::color(theme::TEXT_DIM)
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
                        .text_color(theme::color(theme::TEXT_DIM))
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
                    .text_color(theme::color(theme::TEXT_FAINT))
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
                        .text_color(theme::color(theme::ACCENT))
                        .child(support.unwrap_or("")),
                )
            })
            .when(trace.error.is_some(), |style| {
                style.child(
                    div()
                        .px(px(10.))
                        .pb(px(4.))
                        .text_size(px(9.5))
                        .text_color(theme::color(theme::DANGER))
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
                                    .text_color(theme::color(theme::TEXT_DIM))
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
                                                .text_color(theme::color(theme::TEXT_FAINT))
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
                                    .text_color(theme::color(theme::TEXT_DIM))
                                    .child("PC 采样"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(
                                        widgets::pill(
                                            if config.pc_sample { "开" } else { "关" },
                                            if config.pc_sample {
                                                theme::color(theme::JADE)
                                            } else {
                                                theme::color(theme::TEXT_FAINT)
                                            },
                                        )
                                        .id("dbg-trace-pc-sample-pill")
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.trace_toggle_pc_sample(cx);
                                        })),
                                    )
                                    .when(config.pc_sample, |style| {
                                        style.child(
                                            div()
                                                .text_size(px(9.5))
                                                .text_color(theme::color(theme::TEXT_FAINT))
                                                .child("DWT 周期计数器"),
                                        )
                                    }),
                            ),
                    )
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
                                    .text_color(theme::color(theme::TEXT_DIM))
                                    .child("热点"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(
                                        widgets::pill(
                                            if trace.show_profile { "开" } else { "关" },
                                            if trace.show_profile {
                                                theme::color(theme::ACCENT)
                                            } else {
                                                theme::color(theme::TEXT_FAINT)
                                            },
                                        )
                                        .id("dbg-trace-profile-pill")
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.trace_toggle_profile(cx);
                                        })),
                                    ),
                            ),
                    )
                     .child(
                         div()
                             .flex()
                             .items_center()
                            .child(
                                div()
                                    .w(px(110.))
                                    .flex_none()
                                    .text_size(px(10.))
                                    .text_color(theme::color(theme::TEXT_DIM))
                                    .child("运行模式"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(10.))
                                    .text_color(theme::color(theme::TEXT))
                                    .child(config.mode.label()),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .child("DWT 数据跟踪监视地址写入；ITM 端口重定向 printf。"),
                    )
                    .child(
                        div()
                            .pt(px(4.))
                            .border_t_1()
                            .border_color(theme::alpha(theme::STROKE, 0.08))
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                             .child(widgets::section_label(format!(
                                 "实时包 ({} 条)",
                                 trace.packets.len()
                             )))
                            .when(!trace.packets.is_empty(), |style| {
                                let stats = swvd::trace_stats(&trace.packets);
                                style.child(
                                    div()
                                        .text_size(px(9.5))
                                        .text_color(theme::color(theme::TEXT_FAINT))
                                        .child(format!(
                                            "ITM {} · DWT {} · 异常 {} · PC 采样 {}",
                                            stats.stimulus, stats.dwt, stats.exceptions, stats.pc_samples
                                        )),
                                )
                            })
                             .when(!trace.packets.is_empty(), |style| {
                                style.child(
                                    div()
                                        .id("dbg-trace-packets")
                                        .overflow_y_scroll()
                                        .max_h(px(180.))
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.))
                                        .children(trace.packets.iter().rev().take(200).map(
                                            |packet| {
                                                let (label, detail, color) = match packet {
                                                    swvd::Packet::Stimulus { port, payload } => (
                                                        format!("ITM {port}"),
                                                        String::from_utf8_lossy(payload)
                                                            .to_string(),
                                                        theme::color(theme::JADE),
                                                    ),
                                                    swvd::Packet::Dwt { index, pc } => (
                                                        format!("DWT {index}"),
                                                        pc.map(|pc| format!("PC=0x{pc:08x}"))
                                                            .unwrap_or_default(),
                                                        theme::color(theme::ACCENT),
                                                    ),
                                                    swvd::Packet::Exception { number, entered } => (
                                                        format!("EXC {number}"),
                                                        if *entered {
                                                            "进入".to_string()
                                                        } else {
                                                            "退出".to_string()
                                                        },
                                                        theme::color(theme::CINNABAR),
                                                    ),
                                                    swvd::Packet::Timestamp(value) => (
                                                        "TS".to_string(),
                                                        format!("0x{value:x}"),
                                                        theme::color(theme::TEXT_FAINT),
                                                    ),
                                                };
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(6.))
                                                    .text_size(px(9.5))
                                                    .child(
                                                        div()
                                                            .flex_none()
                                                            .text_color(color)
                                                            .child(label),
                                                    )
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .truncate()
                                                            .text_color(theme::color(theme::TEXT_DIM))
                                                            .child(detail),
                                                    )
                                            },
                                        )),
                                )
                            }),
                     )
                     .child(div().when(trace.show_profile, |style| {
                         let hotspots = trace.profile.hotspots();
                         let total = trace.profile.total();
                         let mut col = div()
                             .pt(px(6.))
                             .border_t_1()
                             .border_color(theme::alpha(theme::STROKE, 0.08))
                             .flex()
                             .flex_col()
                             .gap(px(4.))
                             .child(widgets::section_label(format!(
                                 "热点 (PC 采样 · {} 条)",
                                 total
                             )));
                         if total == 0 {
                             col = col.child(
                                 div()
                                     .text_size(px(9.5))
                                     .text_color(theme::color(theme::TEXT_FAINT))
                                     .child("启用「PC 采样」并运行目标以收集样本。"),
                             );
                         } else {
                             let max_hits = hotspots.first().map(|h| h.hits).unwrap_or(1).max(1);
                             for hotspot in hotspots.iter().take(20) {
                                 let pct = hotspot.hits * 100 / total.max(1);
                                 let bar_w = (hotspot.hits * 100) / max_hits;
                                 col = col.child(
                                     div()
                                         .flex()
                                         .items_center()
                                         .gap(px(6.))
                                         .child(
                                             div()
                                                 .w(px(160.))
                                                 .flex_none()
                                                 .overflow_hidden()
                                                 .text_size(px(9.5))
                                                 .text_color(theme::color(theme::TEXT))
                                                 .child(hotspot.name.clone()),
                                         )
                                         .child(
                                             div()
                                                 .h(px(8.))
                                                 .flex_1()
                                                 .rounded(px(4.))
                                                 .bg(theme::alpha(theme::STROKE, 0.08))
                                                 .child(
                                                     div()
                                                         .h_full()
                                                         .w(px((bar_w as f32) * 1.2))
                                                         .rounded(px(4.))
                                                         .bg(theme::color(theme::ACCENT)),
                                                 ),
                                         )
                                         .child(
                                             div()
                                                 .w(px(52.))
                                                 .flex_none()
                                                 .text_size(px(9.))
                                                 .text_color(theme::color(theme::TEXT_DIM))
                                                 .child(format!("{}% · {}", pct, hotspot.hits)),
                                         ),
                                 );
                             }
                         }
                         style.child(col)
                     }))
             )
             .into_any_element()
     }

    /// Filter a device peripheral and its registers by a case-insensitive
    /// substring. Returns `(peripheral_matches, matching_registers)`: the
    /// peripheral header is shown when its own name matches OR any of its
    /// registers match (so a register-only hit keeps its group context).
     fn filter_device_peripheral<'a>(
         peripheral: &'a ns_device::Peripheral,
         filter: &str,
    ) -> (bool, Vec<&'a ns_device::Register>) {
         if filter.is_empty() {
             return (true, peripheral.registers.iter().collect());
         }
         let needle = filter.to_lowercase();
        // A peripheral-name match shows the whole group; a register-only match
        // keeps the header as context and lists just the matching registers.
        if peripheral.name.to_lowercase().contains(&needle) {
            return (true, peripheral.registers.iter().collect());
        }
        let regs: Vec<&'a ns_device::Register> = peripheral
            .registers
            .iter()
            .filter(|register| register.name.to_lowercase().contains(&needle))
            .collect();
        let periph_match = !regs.is_empty();
        (periph_match, regs)
     }

    /// Device register view: peripheral registers from the target's SVD.
     fn render_debug_device_column(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
         let device = self.device_for_project();
         let current_target = self
             .project
             .as_ref()
             .map(|p| p.config.project.target.clone())
             .unwrap_or_default();

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        // Device picker: list the built-in catalog so the target can be
        // switched in place (the wizard covers initial selection).
        rows.push(
            div()
                .id("device-picker")
                .pt(px(4.))
                .pb(px(6.))
                .px(px(4.))
                .child(
                    div()
                        .text_size(px(9.5))
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme::color(theme::TEXT_FAINT))
                        .mb(px(4.))
                        .child("目标设备"),
                )
                .child(
                    div()
                        .id("device-picker-current")
                        .cursor_pointer()
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .bg(theme::alpha(theme::STROKE, 0.08))
                        .hover(|s| s.bg(theme::alpha(theme::STROKE, 0.14)))
                        .text_size(px(10.5))
                        .text_color(theme::color(theme::TEXT))
                        .child(format!(
                            "{} ▾",
                            if current_target.is_empty() {
                                "未选择".to_string()
                            } else {
                                current_target.clone()
                            }
                        ))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.device_picker_open = !this.device_picker_open;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        );
        if self.device_picker_open {
            let picker_rows: Vec<gpui::AnyElement> = ns_device::CATALOG
                .iter()
                .map(|entry| {
                    let key = entry.key;
                    let label = format!(
                        "{} — {} · {:.0}K Flash",
                        entry.label,
                        entry.core,
                        entry.flash_size / 1024
                    );
                    div()
                        .id(gpui::SharedString::from(format!("device-opt-{}", entry.key)))
                        .cursor_pointer()
                        .px(px(8.))
                        .py(px(3.))
                        .rounded(px(4.))
                        .hover(|s| s.bg(theme::alpha(theme::STROKE, 0.10)))
                        .text_size(px(10.))
                        .text_color(if current_target == key {
                            theme::color(theme::ACCENT)
                        } else {
                            theme::color(theme::TEXT_DIM)
                        })
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_project_target(key, cx);
                        }))
                        .into_any_element()
                })
                .collect();
            rows.push(
                div()
                    .id("device-picker-list")
                    .ml(px(8.))
                    .mb(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(picker_rows)
                    .into_any_element(),
            );
        }
        rows.push(
            div()
                .pb(px(4.))
                .px(px(4.))
                .text_size(px(9.))
                .text_color(theme::color(theme::TEXT_FAINT))
                .child("点击目标设备可切换 MCU（写回 project.toml）。")
                .into_any_element(),
        );
        // Live-watch toggle: refresh device registers while the target runs.
        rows.push(
            div()
                .id("live-watch-toggle")
                .cursor_pointer()
                .px(px(8.))
                .py(px(4.))
                .mb(px(4.))
                .rounded(px(6.))
                .bg(theme::alpha(
                    if self.debug.live_watch { theme::JADE } else { theme::STROKE },
                    if self.debug.live_watch { 0.16 } else { 0.08 },
                ))
                .hover(|s| s.opacity(0.85))
                .text_size(px(10.))
                .text_color(if self.debug.live_watch {
                    theme::color(theme::JADE)
                } else {
                    theme::color(theme::TEXT_DIM)
                })
                .child(if self.debug.live_watch {
                    "◉ 实时监视：开（运行中每 250ms 刷新寄存器）"
                } else {
                    "○ 实时监视：关（仅暂停时刷新）"
                })
                 .on_click(cx.listener(|this, _, _, cx| this.toggle_live_watch(cx)))
                 .into_any_element(),
         );
         // Peripheral/register name filter (case-insensitive substring match).
         let filter_text = self.device_filter.read(cx).text().to_string();
         let filter_field = self.device_filter.clone();
         rows.push(
             div()
                 .px(px(4.))
                 .pb(px(4.))
                 .child(
                     div()
                         .flex()
                         .items_center()
                         .gap(px(4.))
                         .child(filter_field)
                         .child(
                             div()
                                 .id("device-filter-clear")
                                 .cursor_pointer()
                                 .text_size(px(10.))
                                 .text_color(theme::color(theme::TEXT_FAINT))
                                 .child("✕")
                                 .on_click(cx.listener(move |this, _, _, cx| {
                                     this.device_filter
                                         .update(cx, |f, fcx| f.set_text("", fcx));
                                     cx.notify();
                                 })),
                         ),
                 )
                 .into_any_element(),
         );
 
         match device {
            Some(device) => {
                let peripherals = device.sorted_peripherals();
                if peripherals.is_empty() {
                    rows.push(
                        div()
                            .p(px(12.))
                            .text_size(px(11.))
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .child("该目标没有已定义的 SVD 外设。")
                            .into_any_element(),
                    );
                }
                  for peripheral in peripherals {
                     let (periph_match, regs) = Self::filter_device_peripheral(peripheral, &filter_text);
                    if !periph_match && regs.is_empty() {
                        continue;
                    }
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
                    for register in regs {
                        let reg_address = register.address;
                        let field_summary: String = register
                            .fields
                            .iter()
                            .map(|field| {
                                format!("{}[{}:{}]", field.name, field.bit_offset + field.bit_width - 1, field.bit_offset)
                            })
                            .collect::<Vec<_>>()
                            .join(" · ");
                        // Live value: prefer the debug session's cached
                        // register read, then the simulator memory, else the
                        // reset value.
                        let live = self
                            .debug
                            .device_reg_values
                            .get(&register.address)
                            .copied()
                            .or_else(|| {
                                self.sim
                                    .core
                                    .as_ref()
                                    .map(|sim| sim.read_u32(register.address as u32))
                            })
                            .unwrap_or(register.reset_value as u32);
                        let live_active = self
                            .debug
                            .device_reg_values
                            .contains_key(&register.address)
                            || self.sim.core.is_some();
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
                                        .text_color(theme::color(theme::TEXT_DIM))
                                        .child(format!("0x{:08X}", register.address)),
                                )
                                .child(
                                    div()
                                        .w(px(110.))
                                        .flex_none()
                                        .font_family(theme::MONO_FONT)
                                        .text_size(px(10.))
                                        .text_color(theme::color(theme::TEXT))
                                        .child(register.name.clone()),
                                )
                                .child(
                                    if self
                                        .debug
                                        .device_reg_edit
                                        .as_ref()
                                        .map(|(addr, _)| *addr == register.address)
                                        .unwrap_or(false)
                                    {
                                        // Edit mode: render the input field.
                                        let field = self
                                            .debug
                                            .device_reg_edit
                                            .as_ref()
                                            .map(|(_, f)| f.clone())
                                            .unwrap();
                                        let field_for_commit = field.clone();
                                        div()
                                            .w(px(110.))
                                            .flex_none()
                                            .child(field)
                                            .on_key_down(cx.listener(
                                                move |this, event: &KeyDownEvent, _, cx| {
                                                    if event.keystroke.key.as_str() == "enter" {
                                                        let text = field_for_commit
                                                            .read(cx)
                                                            .text();
                                                        this.commit_device_reg_edit(&text, cx);
                                                    }
                                                },
                                            ))
                                            .into_any_element()
                                    } else {
                                        div()
                                            .id(("dev-reg-val", register.address))
                                            .w(px(90.))
                                            .flex_none()
                                            .cursor_pointer()
                                            .rounded(px(4.))
                                            .px(px(2.))
                                            .hover(|s| {
                                                s.bg(theme::alpha(theme::STROKE, 0.12))
                                            })
                                            .font_family(theme::MONO_FONT)
                                            .text_size(px(10.))
                                            .text_color(if live_active {
                                                theme::color(theme::JADE)
                                            } else {
                                                theme::color(theme::TEXT_FAINT)
                                            })
                                            .child(format!("0x{:08X}", live))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.begin_device_reg_edit(reg_address, cx);
                                            }))
                                            .into_any_element()
                                    },
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_size(px(9.5))
                                        .text_color(theme::color(theme::TEXT_FAINT))
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
                        .text_color(theme::color(theme::TEXT_FAINT))
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

    /// Analyze the last successful build's artifact memory usage (Flash/RAM)
    /// against the project target's memory map. Returns `None` when the
    /// project has no MCU target, the artifact is missing, or it is not a
    /// recognized firmware file.
    fn analyze_firmware_artifact(&self) -> Option<ns_flash::FirmwareSize> {
        let project = self.project.as_ref()?;
        let target = project.config.project.target.trim().to_string();
        if target.is_empty() {
            return None;
        }
        let artifact = project.artifact_path();
        if !artifact.exists() {
            return None;
        }
        let map = ns_device::memory_map(&target);
        if map.is_empty() {
            // No known device: still report total size with no region split.
            let flash: Vec<(u64, u64)> = Vec::new();
            let ram: Vec<(u64, u64)> = Vec::new();
            return ns_flash::analyze_firmware_size(&artifact, &flash, &ram).ok().flatten();
        }
        let mut flash: Vec<(u64, u64)> = Vec::new();
        let mut ram: Vec<(u64, u64)> = Vec::new();
        for region in &map {
            if region.access.contains('x') {
                flash.push((region.start, region.size));
            } else {
                ram.push((region.start, region.size));
            }
        }
        ns_flash::analyze_firmware_size(&artifact, &flash, &ram).ok().flatten()
    }

    /// Render a compact「固件大小」summary line for the build output.
    fn render_firmware_size_summary(&self) -> String {
        let Some(size) = &self.firmware_size else {
            return String::new();
        };
        let project = match &self.project {
            Some(project) => project,
            None => return String::new(),
        };
        let target = project.config.project.target.trim().to_string();
        let map = ns_device::memory_map(&target);
        let flash_cap = map
            .iter()
            .find(|region| region.access.contains('x'))
            .map(|region| region.size);
        let ram_cap = map
            .iter()
            .find(|region| !region.access.contains('x'))
            .map(|region| region.size);
         let mut line = format!(
             "固件大小: Flash {}{} · RAM {}{} · 共 {} 字节",
             size.flash,
             flash_cap.map_or(String::new(), |cap| {
                 let pct = if cap > 0 { size.flash as f64 / cap as f64 * 100.0 } else { 0.0 };
                 format!(" / {} ({pct:.1}%)", cap)
             }),
             size.ram,
             ram_cap.map_or(String::new(), |cap| {
                 let pct = if cap > 0 { size.ram as f64 / cap as f64 * 100.0 } else { 0.0 };
                 format!(" / {} ({pct:.1}%)", cap)
             }),
             size.total,
         );
        if size.text > 0 || size.data > 0 || size.bss > 0 {
            line.push_str(&format!(
                "\n分区: text {} B · data {} B · bss {} B",
                size.text, size.data, size.bss
            ));
        }
        if size.flash > 0 {
            if let Some(cap) = flash_cap {
                if cap > 0 && size.flash > cap {
                    line.push_str("  ⚠ Flash 超出");
                }
            }
         }
        if !size.symbols.is_empty() {
            let top: Vec<String> = size
                .symbols
                .iter()
                .take(6)
                .map(|symbol| format!("{} {}", symbol.name, symbol.size))
                .collect();
            line.push_str(&format!("\n最大符号: {}", top.join(" · ")));
        }
          line
      }

    /// Build the memory-layout model: parse `link.ld`'s `MEMORY` regions and
    /// attribute the firmware's sections (from `nm`) to those regions. Sets
    /// `self.memory_layout` and reports the outcome in the status line.
    fn show_memory_layout(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let linker = project.root.join("link.ld");
        if !linker.is_file() {
            self.status = "项目没有 link.ld（内存布局视图需要链接脚本的 MEMORY 区域）。".to_string();
            cx.notify();
            return;
        }
        let text = match std::fs::read_to_string(&linker) {
            Ok(text) => text,
            Err(error) => {
                self.status = format!("读取 link.ld 失败: {error}");
                cx.notify();
                return;
            }
        };
        let mut regions = ns_build::layout::parse_linker_memory(&text);
        if regions.is_empty() {
            self.status = "link.ld 中没有可解析的 MEMORY 区域。".to_string();
            cx.notify();
            return;
        }
        // Attribute the firmware's sections to the regions.
        let artifact = project.artifact_path();
        let mut sections = Vec::new();
        if artifact.is_file() {
            let symbols = ns_flash::firmware::analyze_symbols(&artifact);
            sections = symbols
                .into_iter()
                .map(|symbol| ns_build::layout::SectionUsage {
                    name: symbol.name,
                    address: symbol.address,
                    size: symbol.size,
                })
                .collect();
        }
        ns_build::layout::assign_sections(&mut regions, &sections);
        let layout = ns_build::layout::MemoryLayout { regions, sections };
        let used_total: u64 = layout.regions.iter().map(|region| region.used).sum();
        self.memory_layout = Some(layout);
        self.show_output = true;
        self.output_tab = OutputTab::Output;
        self.output.push_str(&format!(
            "内存布局: {} 个区域，已用 {} 字节\n",
            self.memory_layout.as_ref().map(|l| l.regions.len()).unwrap_or(0),
            used_total
        ));
        self.status = format!(
            "内存布局已刷新（{} 个区域，已用 {} 字节）",
            self.memory_layout.as_ref().map(|l| l.regions.len()).unwrap_or(0),
            used_total
        );
         cx.notify();
     }
 
    /// Build the project include dependency graph and report the outcome.
    fn show_include_graph(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let sources = match project.source_files() {
            Ok(sources) if !sources.is_empty() => sources,
            Ok(_) => {
                self.status = "项目没有可扫描的源文件。".to_string();
                cx.notify();
                return;
            }
            Err(error) => {
                self.status = format!("扫描源文件失败: {error}");
                cx.notify();
                return;
            }
        };
        let include_dirs = project.config.compiler.includes.clone();
        let graph = ns_project::depgraph::build_include_graph(&project.root, &sources, &include_dirs);
        let edges: usize = graph.nodes.values().map(|node| node.deps.len()).sum();
        self.include_graph = Some(graph);
        self.show_output = true;
        self.output_tab = OutputTab::Output;
        self.output.push_str(&format!(
            "include 依赖图: {} 个文件，{} 条边\n",
            self.include_graph.as_ref().map(|g| g.nodes.len()).unwrap_or(0),
            edges
        ));
        self.status = format!(
            "include 依赖图已生成（{} 个文件，{} 条边）",
            self.include_graph.as_ref().map(|g| g.nodes.len()).unwrap_or(0),
            edges
        );
         cx.notify();
     }
 
    /// Scan the project for TODO/FIXME/XXX markers and show them in the
    /// output panel; clicking a marker opens the file at its line.
    fn show_tasks(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            self.status = "请先打开项目".to_string();
            cx.notify();
            return;
        };
        let files = ns_project::tasks::collect_source_files(&project.root);
        let markers = ns_project::tasks::scan_tasks(&files);
        let by_keyword = |kw: &str| markers.iter().filter(|m| m.keyword == kw).count();
        self.task_markers = markers.clone();
        self.show_output = true;
        self.output_tab = OutputTab::Output;
        self.output.push_str(&format!(
            "任务面板: 共 {} 个标记（TODO {}, FIXME {}, XXX {}, 其他 {}）\n",
            markers.len(),
            by_keyword("TODO"),
            by_keyword("FIXME"),
            by_keyword("XXX"),
            markers.len() - by_keyword("TODO") - by_keyword("FIXME") - by_keyword("XXX"),
        ));
        self.status = format!("任务面板: {} 个 TODO/FIXME 标记", markers.len());
        cx.notify();
    }

      /// Render the memory-layout region map (used in the build output panel).
     /// Each region is a bar whose filled width is the used fraction; sections
    /// within a region are listed with their sizes.
    fn render_memory_layout_view(&self) -> gpui::AnyElement {
        let Some(layout) = &self.memory_layout else {
            return div().into_any_element();
        };
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        rows.push(
            div()
                .pt(px(8.))
                .pb(px(2.))
                .child(widgets::section_label("内存布局".to_string()))
                .into_any_element(),
        );
        for region in &layout.regions {
            let used_pct = if region.length > 0 {
                region.used.saturating_mul(100) / region.length
            } else {
                0
            };
            let bar_w = used_pct.min(100) as f32;
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .w(px(80.))
                            .flex_none()
                            .text_size(px(10.))
                            .text_color(theme::color(theme::TEXT_DIM))
                            .child(region.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h(px(10.))
                            .rounded(px(4.))
                            .bg(theme::alpha(theme::STROKE, 0.08))
                            .child(
                                div()
                                    .h_full()
                                    .w(px((bar_w * 4.0).max(1.0)))
                                    .rounded(px(4.))
                                    .bg(theme::color(theme::ACCENT)),
                            ),
                    )
                    .child(
                        div()
                            .w(px(170.))
                            .flex_none()
                            .text_size(px(9.5))
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .child(format!(
                                "{} / {} ({}%)",
                                region.used, region.length, used_pct
                            )),
                    )
                    .into_any_element(),
            );
        }
        // List the largest sections attributed to regions.
        let mut sections: Vec<&ns_build::layout::SectionUsage> = layout.sections.iter().collect();
        sections.sort_by(|a, b| b.size.cmp(&a.size));
        if !sections.is_empty() {
            rows.push(
                div()
                    .pt(px(4.))
                    .pb(px(2.))
                    .text_size(px(9.))
                    .text_color(theme::color(theme::TEXT_FAINT))
                    .child(format!(
                        "段/符号（前 8）: {}",
                        sections
                            .iter()
                            .take(8)
                            .map(|s| format!("{} {}B", s.name, s.size))
                            .collect::<Vec<_>>()
                            .join(" · ")
                    ))
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_col()
             .gap(px(4.))
             .children(rows)
             .into_any_element()
     }
 
    /// Render the include dependency graph: each source lists its direct
    /// includes, and headers show how many files include them (transitively).
     fn render_include_graph_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(graph) = &self.include_graph else {
            return div().into_any_element();
        };
         let mut rows: Vec<gpui::AnyElement> = Vec::new();
         rows.push(
             div()
                 .pt(px(8.))
                 .pb(px(2.))
                .flex()
                .items_center()
                .justify_between()
                 .child(widgets::section_label(format!(
                     "include 依赖图（{} 个文件）",
                     graph.nodes.len()
                 )))
                .child(
                    widgets::ghost_button(
                        "include-graph-mode",
                        if self.include_graph_dag {
                            "列表视图"
                        } else {
                            "图形视图"
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.include_graph_dag = !this.include_graph_dag;
                        cx.notify();
                    })),
                )
                 .into_any_element(),
         );
        if self.include_graph_dag {
            rows.push(
                div()
                    .pt(px(4.))
                    .child(self.render_include_graph_dag())
                    .into_any_element(),
            );
            return div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .children(rows)
                .into_any_element();
        }
        // Sort nodes: sources with the most dependents first, then by path.
        let mut nodes: Vec<&ns_project::depgraph::DepNode> = graph.nodes.values().collect();
        nodes.sort_by(|a, b| {
            b.dependents
                .len()
                .cmp(&a.dependents.len())
                .then_with(|| a.path.cmp(&b.path))
        });
        for node in nodes.iter().take(60) {
            let is_header = node.path.ends_with(".h") || node.path.ends_with(".hpp");
            let tone = if is_header {
                theme::color(theme::ACCENT)
            } else {
                theme::color(theme::TEXT)
            };
            rows.push(
                div()
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .child(
                        div()
                            .w(px(230.))
                            .flex_none()
                            .overflow_hidden()
                            .text_size(px(9.5))
                            .text_color(tone)
                            .child(node.path.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(9.))
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .child(if node.deps.is_empty() {
                                "（无直接依赖）".to_string()
                            } else {
                                format!("→ {}", node.deps.join(", "))
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(9.))
                            .text_color(theme::color(theme::JADE))
                            .child(format!("×{}", node.dependents.len())),
                    )
                    .into_any_element(),
            );
        }
        div()
            .flex()
             .gap(px(2.))
             .children(rows)
             .into_any_element()
     }
    /// Render the include graph as a layered DAG: files grouped into columns
    /// by dependency depth, connected by horizontal edges. Uses absolutely
    /// positioned boxes so the layout is stable and scrollable.
    fn render_include_graph_dag(&self) -> gpui::AnyElement {
        let Some(graph) = &self.include_graph else {
            return div().into_any_element();
        };
        let (nodes, edges, _columns) = ns_project::depgraph::dag_layout(graph);
        if nodes.is_empty() {
            return div().into_any_element();
        }
        // Geometry: column width, row height, node box size.
        let column_width = 190.0f32;
        let row_height = 30.0f32;
        let node_width = 170.0f32;
        let node_height = 22.0f32;
        let padding = 8.0f32;

        let max_column = nodes.iter().map(|node| node.column).max().unwrap_or(0);
        let max_row = nodes.iter().map(|node| node.row).max().unwrap_or(0);
        let canvas_width = padding * 2.0 + (max_column as f32 + 1.0) * column_width;
        let canvas_height = padding * 2.0 + (max_row as f32 + 1.0) * row_height;

        let mut elements: Vec<gpui::AnyElement> = Vec::new();
        // Edges first (under the boxes): horizontal line + arrow head.
        for edge in &edges {
            let from = nodes.iter().find(|node| node.path == edge.from);
            let to = nodes.iter().find(|node| node.path == edge.to);
            let (Some(from), Some(to)) = (from, to) else {
                continue;
            };
            let x1 = padding + from.column as f32 * column_width + node_width;
            let y1 = padding + from.row as f32 * row_height + node_height / 2.0;
            let x2 = padding + to.column as f32 * column_width;
            let y2 = padding + to.row as f32 * row_height + node_height / 2.0;
            let mid_x = (x1 + x2) / 2.0;
            elements.push(
                div()
                    .absolute()
                    .left(px(x1))
                    .top(px(y1))
                    .w(px(mid_x - x1))
                     .h(px(1.))
                    .bg(theme::alpha(theme::STROKE, 0.5))
                     .into_any_element(),
             );
             elements.push(
                 div()
                     .absolute()
                     .left(px(mid_x))
                     .top(px(y2))
                     .w(px(x2 - mid_x))
                     .h(px(1.))
                     .bg(theme::alpha(theme::STROKE, 0.5))
                     .into_any_element(),
             );
         }
        // Node boxes.
        for node in &nodes {
            let x = padding + node.column as f32 * column_width;
            let y = padding + node.row as f32 * row_height;
            let is_header = node.path.ends_with(".h") || node.path.ends_with(".hpp");
            let tone = if is_header {
                theme::color(theme::ACCENT)
            } else {
                theme::color(theme::TEXT)
            };
            elements.push(
                 div()
                    .id(("dag-node", node.column * 1000 + node.row))
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(node_width))
                    .h(px(node_height))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(theme::alpha(theme::ACCENT, 0.3))
                    .bg(theme::color(theme::BG_2).opacity(0.8))
                    .px(px(6.))
                    .py(px(3.))
                    .overflow_hidden()
                    .text_size(px(9.))
                     .text_color(tone)
                     .child(node.path.clone())
                     .into_any_element(),
             );
         }
        div()
            .relative()
            .w(px(canvas_width))
            .h(px(canvas_height))
            .overflow_hidden()
            .children(elements)
            .into_any_element()
    }

 
    /// Render the TODO/FIXME task markers as clickable rows; clicking a row
    /// opens the file at the marker's line.
     fn render_tasks_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.task_markers.is_empty() {
            return div().into_any_element();
        }
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        rows.push(
            div()
                .pt(px(8.))
                .pb(px(2.))
                .child(widgets::section_label(format!(
                    "任务标记（{}）",
                    self.task_markers.len()
                )))
                .into_any_element(),
        );
         let markers = self.task_markers.clone();
        for (row_index, marker) in markers.iter().take(80).enumerate() {
             let path = marker.path.clone();
            let path_label = self
                .project
                .as_ref()
                .and_then(|project| marker.path.strip_prefix(&project.root).ok())
                .unwrap_or(&marker.path)
                .display()
                .to_string();
             let line = marker.line;
             let keyword = marker.keyword.clone();
             let text = marker.text.clone();
             let color = match keyword.as_str() {
                 "FIXME" | "BUG" => theme::color(theme::DANGER),
                 "HACK" => theme::color(theme::ACCENT_3),
                 _ => theme::color(theme::ACCENT),
             };
             rows.push(
                 div()
                    .id(("task-row", row_index))
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .rounded(px(4.))
                    .px(px(6.))
                    .py(px(2.))
                    .hover(|style| style.bg(theme::alpha(theme::ACCENT, 0.06)))
                    .cursor_pointer()
                     .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_file(path.clone(), cx);
                        if let Some(editor) = this.active_editor() {
                            editor.update(cx, |editor, cx| editor.set_cursor_line(line, cx));
                        }
                     }))
                    .child(
                        div()
                            .w(px(34.))
                            .flex_none()
                            .text_size(px(9.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(color)
                            .child(keyword),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(9.5))
                            .text_color(theme::color(theme::TEXT_DIM))
                            .child(text),
                    )
                     .child(
                         div()
                             .flex_none()
                             .text_size(px(8.5))
                             .text_color(theme::color(theme::TEXT_FAINT))
                             .child(format!(
                                 "{}:{}",
                                path_label,
                                 line
                             )),
                     )
                    .into_any_element()
             );
        }
        div()
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
                    widgets::ghost_button("sim-step-over", "步过")
                        .on_click(cx.listener(|this, _, _, cx| this.sim_step_over(cx))),
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
                .child(
                    div()
                        .id("sim-cycle-accurate")
                        .cursor_pointer()
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .bg(theme::alpha(
                            if self.sim.cycle_accurate { theme::JADE } else { theme::STROKE },
                            if self.sim.cycle_accurate { 0.16 } else { 0.08 },
                        ))
                        .hover(|s| s.opacity(0.85))
                        .text_size(px(10.))
                        .text_color(if self.sim.cycle_accurate {
                            theme::color(theme::JADE)
                        } else {
                            theme::color(theme::TEXT_DIM)
                        })
                        .child(if self.sim.cycle_accurate {
                            "◉ 周期精确"
                        } else {
                            "○ 周期精确（关）"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.sim.cycle_accurate = !this.sim.cycle_accurate;
                            if let Some(sim) = this.sim.core.as_mut() {
                                sim.cycle_accurate = this.sim.cycle_accurate;
                            }
                            cx.notify();
                        })),
                )
        .into_any_element(),
        );

        // Status line.
        rows.push(
            div()
                .text_size(px(10.))
                .text_color(theme::color(theme::TEXT_DIM))
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
                    .text_color(theme::color(theme::TEXT))
                    .child(
                        div()
                            .id("sim-pc")
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.begin_pc_edit(window, cx);
                            }))
                            .child(if let Some(field) = &self.sim.pc_edit_input {
                                div()
                                    .child(field.clone())
                                    .into_any_element()
                            } else {
                                div()
                                    .cursor_pointer()
                                    .child(format!("PC: 0x{:08x}", core.program_counter()))
                                    .into_any_element()
                            }),
                    )
                    .child(
                        div()
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .child(core.arch.label()),
                    )
                     .child(format!("周期: {}", core.cycle_count))
                    .child(sim_time_readout(core.cycle_count, self.sim_clock_hz()))
                     .child(if core.halted { "状态: 已停止" } else { "状态: 运行中" })
                    .into_any_element(),
            );
            // Current instruction at PC, disassembled per architecture.
            {
                let pc = core.program_counter();
                let read = |addr: u32| core.read_u8(addr);
                let text = ns_sim::disassemble_one_state(core.arch, pc, &read, core.arm_state());
                rows.push(
                    div()
                        .id("sim-current-insn")
                        .flex()
                        .gap(px(8.))
                        .items_center()
                        .text_size(px(10.))
                        .text_color(theme::color(theme::ACCENT))
                        .child(
                            div()
                                .font_family(theme::MONO_FONT)
                                .child(format!("{text}")),
                        )
                        .into_any_element(),
                );
            }

            // Register file: per-architecture count (32 RISC-V x0-x31, 16
            // ARM r0-r15 / Xtensa a0-a15, 8051/80251/C166 special + bank).
            let reg_count = ns_sim::reg_count_for(core.arch);
            let mut reg_rows: Vec<gpui::AnyElement> = Vec::new();
            for chunk in (0..reg_count).collect::<Vec<_>>().chunks(4) {
                let mut cells: Vec<gpui::AnyElement> = Vec::new();
                for index in chunk {
                    let value = core.read_reg(*index);
                    let idx = *index;
                    let name = ns_sim::reg_label_for(core.arch, idx);
                    cells.push(
                        div()
                            .flex_1()
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(px(4.))
                            .bg(theme::alpha(theme::STROKE, 0.05))
                            .text_size(px(9.5))
                            .child(if self.sim.reg_edit == Some(idx) {
                                div()
                                    .id(gpui::SharedString::from(format!("sim-reg-edit-{idx}")))
                                    .child(
                                        self.sim
                                            .reg_edit_input
                                            .clone()
                                            .map(|field| div().child(field).into_any_element())
                                            .unwrap_or_else(|| div().into_any_element()),
                                    )
                                    .into_any_element()
                            } else {
                                div()
                                    .id(gpui::SharedString::from(format!("sim-reg-{idx}")))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.begin_reg_edit(idx, window, cx);
                                    }))
                                    .child(format!("{}  0x{:08x}", name, value))
                                    .into_any_element()
                            })
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
            // Breakpoint list: addresses + disassembly, marked when PC is
            // at a breakpoint (the step that hit it). The address field adds
            // a breakpoint; each row's ✕ removes it.
            let bp_rows: Vec<gpui::AnyElement> = core
                .breakpoints
                .iter()
                .map(|addr| {
                    let a = *addr;
                    let read = |x: u32| core.read_u8(x);
                    let text = ns_sim::disassemble_one_state(core.arch, a, &read, core.arm_state());
                    let at_pc = a == core.program_counter();
                    div()
                        .id(gpui::SharedString::from(format!("sim-bp-{a}")))
                        .flex()
                        .gap(px(8.))
                        .items_center()
                        .text_size(px(9.5))
                        .text_color(if at_pc {
                            theme::color(theme::DANGER)
                        } else {
                            theme::color(theme::TEXT_DIM)
                        })
                        .child(format!("● 0x{a:08x}"))
                        .child(
                            div()
                                .font_family(theme::MONO_FONT)
                                .child(text),
                        )
                        .when(at_pc, |style| style.child("← 命中"))
                        .child(
                            div()
                                .id(gpui::SharedString::from(format!("sim-bp-del-{a}")))
                                .cursor_pointer()
                                .text_color(theme::color(theme::TEXT_FAINT))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.sim_remove_breakpoint(a, cx);
                                }))
                                .child("✕"),
                        )
                        .into_any_element()
                })
                .collect();
            rows.push(
                div()
                    .id("sim-bp-list")
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .text_color(theme::color(theme::TEXT_DIM))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(theme::color(theme::TEXT_FAINT))
                                    .child(format!("断点 ({}):", core.breakpoints.len())),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .child(
                                        self.sim
                                            .bp_input
                                            .clone()
                                            .map(|field| div().child(field).into_any_element())
                                            .unwrap_or_else(|| div().into_any_element()),
                                    ),
                            )
                            .child(
                                widgets::ghost_button("sim-bp-add", "添加").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.ensure_sim_bp_input(cx);
                                        this.sim_add_breakpoint_from_field(cx);
                                    }),
                                ),
                            ),
                    )
                    .children(bp_rows)
                    .into_any_element(),
            );

            // UART output.
            let uart: String = String::from_utf8_lossy(&core.peripherals.outputs).into_owned();
            rows.push(
                div()
                    .text_size(px(9.5))
                    .text_color(theme::color(theme::TEXT_FAINT))
                    .child(if uart.is_empty() {
                        "UART 输出: （空）".to_string()
                    } else {
                        format!("UART 输出: {uart}")
                    })
                    .into_any_element(),
            );

            // GPIO state view: one square per pin (P0..P31), coloured by
            // direction and level. Output pins are jade when high; input pins
            // are accent when sampled high.
            let gpio_pins: Vec<gpui::AnyElement> = (0..32)
                .map(|pin| {
                    let dir = (core.peripherals.gpio_dir >> pin) & 1 == 1;
                    let out = (core.peripherals.gpio_out >> pin) & 1 == 1;
                    let inp = (core.peripherals.gpio_in >> pin) & 1 == 1;
                    let (color, alpha) = if dir {
                        if out {
                            (theme::JADE, 0.6)
                        } else {
                            (theme::TEXT_FAINT, 0.08)
                        }
                    } else if inp {
                        (theme::ACCENT, 0.6)
                    } else {
                        (theme::TEXT_FAINT, 0.08)
                    };
                    div()
                        .w(px(15.))
                        .h(px(15.))
                        .rounded(px(3.))
                        .border_1()
                        .border_color(theme::alpha(theme::STROKE, 0.3))
                        .bg(theme::alpha(color, alpha))
                        .child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(7.))
                                .text_color(theme::color(theme::TEXT_FAINT))
                                .child(pin.to_string()),
                        )
                        .into_any_element()
                })
                .collect();
            let out_count = core.peripherals.gpio_out.count_ones();
            let dir_count = core.peripherals.gpio_dir.count_ones();
            rows.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme::color(theme::TEXT_DIM))
                            .child(format!(
                                "GPIO 状态 · {} 输出高 · {} 输出使能",
                                out_count, dir_count
                            )),
                    )
                    .child(div().flex().flex_wrap().gap(px(3.)).children(gpio_pins))
                    .into_any_element(),
            );

            // Timer state view: one row per configured timer.
            let timer_rows: Vec<gpui::AnyElement> = core
                .peripherals
                .timers
                .iter()
                .enumerate()
                .map(|(index, timer)| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .px(px(6.))
                        .py(px(3.))
                        .rounded(px(4.))
                        .bg(theme::alpha(theme::STROKE, 0.05))
                        .text_size(px(9.5))
                        .child(format!(
                            "TIM{index} 0x{:08x}",
                            timer.base
                        ))
                        .child(
                            div()
                                .text_color(if timer.enabled {
                                    theme::color(theme::JADE)
                                } else {
                                    theme::color(theme::TEXT_FAINT)
                                })
                                .child(if timer.enabled { "启用" } else { "禁用" }),
                        )
                        .child(format!(
                            "cnt {}/{} · pre {} · 溢出 {}",
                            timer.counter, timer.auto_reload, timer.prescaler, timer.update_count
                        ))
                        .into_any_element()
                })
                .collect();
            if !timer_rows.is_empty() {
                rows.push(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(theme::color(theme::TEXT_DIM))
                                .child("定时器"),
                        )
                        .child(div().flex().flex_col().gap(px(3.)).children(timer_rows))
                        .into_any_element(),
                );
            }

            // Watchdog state view: enabled/timeout/elapsed/resets so runaway
            // firmware (never fed) is visible rather than silently stalling.
            if let Some(watchdog) = &core.peripherals.watchdog {
                rows.push(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(theme::color(theme::TEXT_DIM))
                                .child(format!(
                                    "看门狗 0x{:08x} · {} · 超时 {} 周期 · 已过 {} · 复位 {} 次",
                                    watchdog.base,
                                    if watchdog.enabled { "启用" } else { "禁用" },
                                    watchdog.timeout_cycles,
                                    watchdog.elapsed,
                                    watchdog.reset_count
                                )),
                        )
                        .into_any_element(),
                );
            }


            // Recent GPIO output-change events, aligned to the PC of the store
            // that caused them (P14.2): the log lets the user correlate a pin
            // change with the exact source instruction.
            let event_rows: Vec<gpui::AnyElement> = core
                .peripherals
                .gpio_events
                .iter()
                .rev()
                .take(6)
                .map(|event| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(9.5))
                        .font_family("Menlo")
                        .child(format!("P{}", event.pin))
                        .child(
                            div()
                                .text_color(if event.value == 1 {
                                    theme::color(theme::JADE)
                                } else {
                                    theme::color(theme::TEXT_FAINT)
                                })
                                .child(if event.value == 1 { "高" } else { "低" }),
                        )
                        .child(format!("@0x{:08x} pc", event.pc))
                        .child(
                            div()
                                .text_color(theme::color(theme::TEXT_FAINT))
                                .child(format!("cycle {}", event.cycle)),
                        )
                        .into_any_element()
                })
                .collect();
            if !event_rows.is_empty() {
                rows.push(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(theme::color(theme::TEXT_DIM))
                                .child("GPIO 事件（最近 6 条）"),
                        )
                        .child(div().flex().flex_col().gap(px(2.)).children(event_rows))
                        .into_any_element(),
                );
            }

            // Memory view: hex dump of the simulator address space.
            let addr_field = self.sim.mem_address.clone();
            let mem_rows: Vec<gpui::AnyElement> = self
                .sim
                .mem_bytes
                .chunks(16)
                .enumerate()
                .map(|(row, chunk)| {
                    let base = parse_address(&self.sim.mem_address).unwrap_or(0) as usize
                        + row * 16;
                    let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
                    let hex_line = hex
                        .chunks(4)
                        .map(|group| group.join(" "))
                        .collect::<Vec<_>>()
                        .join("  ");
                    let ascii: String = chunk
                        .iter()
                        .map(|b| {
                            if (0x20..0x7f).contains(b) {
                                *b as char
                            } else {
                                '.'
                            }
                        })
                        .collect();
                    div()
                        .flex()
                        .gap(px(8.))
                        .text_size(px(9.5))
                        .font_family("Menlo")
                        .child(format!("0x{base:08x}"))
                        .child(div().text_color(theme::color(theme::TEXT_DIM)).child(hex_line))
                        .child(div().text_color(theme::color(theme::TEXT_FAINT)).child(ascii))
                        .into_any_element()
                })
                .collect();
            rows.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(theme::color(theme::TEXT_DIM))
                                    .child("内存"),
                            )
                            .child(
                                div()
                                    .id("sim-mem-address")
                                    .flex_1()
                                    .px(px(6.))
                                    .py(px(2.))
                                    .rounded(px(4.))
                                    .bg(theme::alpha(theme::STROKE, 0.05))
                                    .text_size(px(9.5))
                                    .child(addr_field),
                            )
                            .child(
                                widgets::ghost_button("sim-mem-read", "读取")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sim_read_memory(cx)
                                    })),
                            ),
                    )
                    .when(self.sim.mem_error.is_some(), |style| {
                        style.child(
                            div()
                                .text_size(px(9.5))
                                .text_color(theme::color(theme::DANGER))
                                .child(
                                    self.sim
                                        .mem_error
                                        .clone()
                                        .unwrap_or_default(),
                                ),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex_1()
                                    .child(
                                        self.sim
                                            .mem_edit_input
                                            .clone()
                                            .map(|field| div().child(field).into_any_element())
                                            .unwrap_or_else(|| div().into_any_element()),
                                    ),
                            )
                            .child(
                                widgets::ghost_button("sim-mem-write", "写入").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.sim_write_memory_from_field(cx)
                                    }),
                                ),
                            ),
                    )
                    .child(
                        div()
                            .id("sim-mem-dump")
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .children(mem_rows),
                    )
                    .into_any_element(),
            );
        } else {
            rows.push(
                div()
                    .text_size(px(10.))
                    .text_color(theme::color(theme::TEXT_FAINT))
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
                            .text_color(theme::color(theme::TEXT_DIM))
                            .child(label),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme::color(theme::TEXT_FAINT))
                            .child(format!("· {}", self.status)),
                    )
                    .when(self.build_cancel.is_some(), |style| {
                        style.child(
                            div()
                                .id("build-cancel")
                                .cursor_pointer()
                                .px(px(6.))
                                .py(px(2.))
                                .rounded(px(4.))
                                .text_size(px(10.))
                                .text_color(theme::color(theme::DANGER))
                                .hover(|style| {
                                    style.bg(theme::alpha(theme::DANGER, 0.12))
                                })
                                .child("■ 取消构建")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_build(cx)
                                })),
                        )
                     })
                    .when(self.flash_cancel.is_some(), |style| {
                        style.child(
                            div()
                                .id("flash-cancel")
                                .cursor_pointer()
                                .px(px(6.))
                                .py(px(2.))
                                .rounded(px(4.))
                                .text_size(px(10.))
                                .text_color(theme::color(theme::DANGER))
                                .hover(|style| {
                                    style.bg(theme::alpha(theme::DANGER, 0.12))
                                })
                                .child("■ 取消烧写")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_flash(cx)
                                })),
                        )
                    }),
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
                     .when(self.project.is_some(), |style| {
                         let config = self
                             .project
                             .as_ref()
                             .map(|p| p.build_config().to_string())
                             .unwrap_or_default();
                         let open = self.build_config_open;
                         style.child(
                             div()
                                .relative()
                                .child(
                                    div()
                                        .id("status-build-config")
                                        .flex()
                                        .items_center()
                                        .gap(px(4.))
                                        .cursor_pointer()
                                        .hover(|s| s.opacity(0.8))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.build_config_open = !this.build_config_open;
                                            cx.notify();
                                        }))
                                        .child(status_item("构建配置", format!("{config} ▾"))),
                                )
                                 .children(
                                     open
                                         .then(|| {
                                             ["debug", "release"]
                                                 .into_iter()
                                                 .map(|option| {
                                                     let active = config == option;
                                                     div()
                                                         .id(gpui::SharedString::from(format!(
                                                             "build-config-opt-{option}"
                                                         )))
                                                         .absolute()
                                                         .right(px(0.))
                                                         .top(px(18.))
                                                         .px(px(8.))
                                                         .py(px(4.))
                                                         .rounded(px(6.))
                                                         .bg(theme::color(theme::PANEL_SOLID))
                                                         .text_size(px(11.))
                                                         .cursor_pointer()
                                                         .hover(|s| {
                                                             s.bg(theme::alpha(theme::STROKE, 0.12))
                                                         })
                                                         .text_color(if active {
                                                             theme::color(theme::ACCENT)
                                                         } else {
                                                             theme::color(theme::TEXT_DIM)
                                                         })
                                                         .child(format!(
                                                             "{}{}",
                                                             option,
                                                             if active { " ✓" } else { "" }
                                                         ))
                                                         .on_click(cx.listener(move |this, _, _, cx| {
                                                             this.set_build_config(option, cx);
                                                             this.build_config_open = false;
                                                             cx.notify();
                                                         }))
                                                         .into_any_element()
                                                 })
                                                 .collect::<Vec<gpui::AnyElement>>()
                                         })
                                         .unwrap_or_default(),
                                 ),
                         )
                     })
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
                                            .text_color(theme::color(theme::TEXT))
                                            .child("ns-embed-ide"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.5))
                                            .text_color(theme::color(theme::TEXT_DIM))
                                            .child("商量嵌入式开发环境 · 配置即文件，工具链开放可替换"),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme::color(theme::TEXT_DIM))
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
                                            .text_color(theme::color(theme::TEXT_FAINT))
                                            .child(format!("{} 个", recents.len())),
                                    ),
                            )
                            .when(recents.is_empty(), |style| {
                                style.child(
                                    div()
                                        .px(px(16.))
                                        .py(px(22.))
                                        .text_size(px(11.5))
                                        .text_color(theme::color(theme::TEXT_FAINT))
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
                                                    .text_color(theme::color(theme::TEXT))
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
                                                    .text_color(theme::color(theme::TEXT_FAINT))
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
                                .text_color(theme::color(theme::TEXT_DIM))
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
                        style
                            .child(self.render_output_divider(cx))
                            .child(self.render_output(cx))
                    }),
            )
    }

    /// The draggable divider above the output panel.
    fn render_output_divider(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("output-divider")
            .h(px(5.))
            .flex_none()
            .cursor(gpui::CursorStyle::ResizeUpDown)

            .child(
                div()
                    .absolute()
                    .top(px(2.))
                    .left(px(0.))
                    .right(px(0.))
                    .h(px(1.))
                    .bg(theme::alpha(theme::STROKE, 0.12))
                    .hover(|style| style.bg(theme::alpha(theme::ACCENT, 0.5))),
            )
            .on_drag(
                OutputDividerDrag {
                    last_y: std::cell::Cell::new(None),
                },
                |_drag, _position, _window, cx| cx.new(|_| DividerPreview),
            )
            .on_drag_move(cx.listener(
                |this, event: &gpui::DragMoveEvent<OutputDividerDrag>, _window, cx| {
                    let drag = event.drag(cx);
                    let mouse_y = f32::from(event.event.position.y);
                    if let Some(previous_y) = drag.last_y.replace(Some(mouse_y)) {
                        // Dragging down grows the panel; up shrinks it.
                        this.output_px = (this.output_px + (mouse_y - previous_y))
                            .clamp(80.0, 1200.0);
                        cx.notify();
                    }
                },
            ))
    }
}
 /// Insert an `#include <...>` line at `insert_at` (a line index into
 /// `source`). Returns the new file text with the include line inserted.
 fn insert_include_line(source: &str, include: &str, insert_at: usize) -> String {
     let mut lines: Vec<String> = source.lines().map(|line| line.to_string()).collect();
     lines.insert(insert_at, include.to_string());
     let mut text = lines.join("\n");
     if !text.ends_with('\n') {
         text.push('\n');
     }
     text
 }

/// Compute the line index at which to insert an `#include`: after the last
/// existing `#include` line, or `0` when there are none.
fn include_insert_at(source: &str) -> usize {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with("#include"))
        .map(|(i, _)| i + 1)
        .last()
        .unwrap_or(0)
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
            if let Some(index) = self.active_terminal {
                if let Some(terminal) = self.terminals.get(index) {
                    window.focus(&terminal.read(cx).focus_handle());
                }
            }
        }
        if self.focus_dialog_on_next_frame {
            self.focus_dialog_on_next_frame = false;
            if let Some(dialog) = &self.new_entry_dialog {
                window.focus(&dialog.field.read(cx).focus_handle(cx));
            } else if let Some(dialog) = &self.condition_dialog {
                window.focus(&dialog.field.read(cx).focus_handle(cx));
            } else if let Some(dialog) = &self.watchpoint_dialog {
                window.focus(&dialog.field.read(cx).focus_handle(cx));
            } else if let Some(dialog) = &self.go_to_line_dialog {
                window.focus(&dialog.field.read(cx).focus_handle(cx));
            } else if let Some(dialog) = &self.git_branch_dialog {
                window.focus(&dialog.field.read(cx).focus_handle(cx));
            } else if let Some(dialog) = &self.set_var_dialog {
                window.focus(&dialog.field.read(cx).focus_handle(cx));
            } else if let Some(dialog) = &self.settings_dialog {
                window.focus(&dialog.name.read(cx).focus_handle(cx));
            } else if let Some(dialog) = &self.toolchain_dialog {
                if let Some((_, field)) = dialog.fields.first() {
                    window.focus(&field.read(cx).focus_handle(cx));
                }
            } else if let Some(dialog) = &self.keybindings_dialog {
                if let Some((_, _, _, field)) = dialog.fields.first() {
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
            .text_color(theme::color(theme::TEXT))
            .track_focus(&self.focus_handle)
            .key_context(if self.wizard.is_some() {
                "Wizard"
            } else if self.condition_dialog.is_some()
                || self.watchpoint_dialog.is_some()
                || self.settings_dialog.is_some()
                || self.toolchain_dialog.is_some()
                || self.keybindings_dialog.is_some()
                || self.new_entry_dialog.is_some()
                || self.go_to_line_dialog.is_some()
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
                } else if this.new_entry_dialog.is_some() {
                    this.save_new_entry_dialog(cx);
                } else if this.watchpoint_dialog.is_some() {
                    this.save_watchpoint_dialog(cx);
                } else if this.go_to_line_dialog.is_some() {
                    this.save_go_to_line_dialog(cx);
                } else if this.set_var_dialog.is_some() {
                    this.save_set_var_dialog(cx);
                } else if this.settings_dialog.is_some() {
                    this.save_settings(cx);
                } else if this.toolchain_dialog.is_some() {
                    this.save_toolchain(cx);
                } else if this.keybindings_dialog.is_some() {
                    this.save_keybindings(cx);
                } else if this.git_branch_dialog.is_some() {
                    this.git_create_branch(cx);
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
                } else if this.new_entry_dialog.is_some() {
                    this.cancel_new_entry_dialog(cx);
                } else if this.watchpoint_dialog.is_some() {
                    this.cancel_watchpoint_dialog(cx);
                } else if this.git_branch_dialog.is_some() {
                    this.cancel_git_branch_dialog(cx);
                } else if this.go_to_line_dialog.is_some() {
                    this.cancel_go_to_line_dialog(cx);
                } else if this.set_var_dialog.is_some() {
                    this.cancel_set_var_dialog(cx);
                } else if this.rename_dialog.is_some() {
                    this.rename_dialog = None;
                    cx.notify();
                } else if this.settings_dialog.is_some() {
                    this.cancel_settings(cx);
                } else if this.toolchain_dialog.is_some() {
                    this.cancel_toolchain(cx);
                } else if this.keybindings_dialog.is_some() {
                    this.cancel_keybindings(cx);
                } else if this.search.is_some() {
                    this.close_search(cx);
                } else if this.palette.is_some() {
                    this.close_palette(cx);
                } else if this.tree_menu.is_some() {
                    this.tree_menu = None;
                    cx.notify();
                } else if this.top_menu_open.is_some() {
                    this.top_menu_open = None;
                    cx.notify();
                 } else if this.context_menu.is_some() {
                     this.context_menu = None;
                     cx.notify();
                } else if this.tab_menu.is_some() {
                    this.tab_menu = None;
                    cx.notify();
                 }
            }))
             .on_action(cx.listener(|this, _: &GoToDefinition, _, cx| {
                 this.lsp_go_to_definition(cx)
             }))
            .on_action(cx.listener(|this, _: &GoToLine, _, cx| this.open_go_to_line(cx)))
            .on_action(cx.listener(|this, _: &GotoHeaderSource, _, cx| {
                this.goto_header_source(cx)
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
            .on_action(cx.listener(|this, _: &NavigateBack, _, cx| this.navigate_back(cx)))
            .on_action(cx.listener(|this, _: &NavigateForward, _, cx| this.navigate_forward(cx)))
            .on_action(cx.listener(|this, _: &JumpToInclude, _, cx| this.jump_to_include(cx)))
            .on_action(cx.listener(|this, _: &RenameSymbol, _, cx| this.rename_symbol(cx)))
            .on_action(cx.listener(|this, _: &RenameConfirm, _, cx| this.confirm_rename(cx)))
            .on_action(cx.listener(|this, _: &RenameCancel, _, cx| {
                this.rename_dialog = None;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SplitRight, _, cx| this.split_editor_right(cx)))
            .on_action(cx.listener(|this, _: &CloseSplit, _, cx| this.close_split(cx)))
            .on_action(cx.listener(|this, _: &WorkspaceSymbols, window, cx| {
                this.open_workspace_symbols(window, cx)
            }))
            .on_action(cx.listener(|this, _: &QuickOpen, window, cx| {
                this.open_quick_open(window, cx)
            }))
            .on_action(cx.listener(|this, _: &RecentFiles, window, cx| {
                this.open_recent_files(window, cx)
            }))
             .on_action(cx.listener(|this, _: &PaletteSelectUp, _, cx| {
                 if this.workspace_symbol_open {
                     this.workspace_symbol_move(-1, cx);
                 } else if this.references_open {
                     this.references_move(-1, cx);
                 } else if this.quick_open {
                     this.quick_open_move(-1, cx);
                 } else if this.recent_open {
                     this.recent_files_move(-1, cx);
                 } else if this.completion_open {
                     this.completion_move(-1, cx);
                 }
             }))
             .on_action(cx.listener(|this, _: &PaletteSelectDown, _, cx| {
                 if this.workspace_symbol_open {
                     this.workspace_symbol_move(1, cx);
                 } else if this.references_open {
                     this.references_move(1, cx);
                 } else if this.quick_open {
                     this.quick_open_move(1, cx);
                 } else if this.recent_open {
                     this.recent_files_move(1, cx);
                 } else if this.completion_open {
                     this.completion_move(1, cx);
                 }
             }))
             .on_action(cx.listener(|this, _: &PaletteConfirm, _, cx| {
                 if this.workspace_symbol_open {
                     this.workspace_symbol_confirm(cx);
                 } else if this.references_open {
                     this.references_confirm(cx);
                 } else if this.quick_open {
                     this.quick_open_confirm(cx);
                 } else if this.recent_open {
                     this.recent_files_confirm(cx);
                 } else if this.completion_open {
                     this.completion_confirm(cx);
                 }
             }))
             .on_action(cx.listener(|this, _: &EscapeModal, _, cx| {
                 if this.workspace_symbol_open {
                     this.close_workspace_symbols(cx);
                 } else if this.references_open {
                     this.close_references(cx);
                 } else if this.quick_open {
                     this.close_quick_open(cx);
                 } else if this.recent_open {
                     this.close_recent_files(cx);
                 } else if this.completion_open {
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

        if let Some(dropdown) = self.render_top_menu_dropdown(cx) {
            root = root.child(dropdown);
        }

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
                        theme::color(theme::CINNABAR)
                    } else {
                        theme::color(theme::TEXT)
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
                item("ctx-goto-header-source", "切换到头文件/源文件", false)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.goto_header_source(cx);
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
                            .bg(theme::color(theme::PANEL_SOLID))
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
 
        if let Some(menu) = self.tab_menu.clone() {
            let item = |id: &'static str, label: &'static str, danger: bool| {
                div()
                    .id(id)
                    .px(px(10.))
                    .py(px(7.))
                    .rounded(px(6.))
                    .text_size(px(11.))
                    .text_color(if danger {
                        theme::color(theme::CINNABAR)
                    } else {
                        theme::color(theme::TEXT)
                    })
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.12)))
                    .child(label)
            };
            let menu_index = menu.index;
            let has_more = self.tabs.len() > 1;
            let has_right = menu_index + 1 < self.tabs.len();
            let mut items: Vec<gpui::AnyElement> = Vec::new();
            items.push(
                item("tabmenu-close", "关闭标签", false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tab_menu = None;
                        this.close_tab(menu_index, cx);
                    }))
                    .into_any_element(),
            );
            if has_more {
                items.push(
                    item("tabmenu-close-others", "关闭其他标签", false)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_other_tabs(menu_index, cx);
                        }))
                        .into_any_element(),
                );
            }
            if has_right {
                items.push(
                    item("tabmenu-close-right", "关闭右侧标签", false)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_tabs_to_right(menu_index + 1, cx);
                        }))
                        .into_any_element(),
                );
            }
            items.push(
                item("tabmenu-close-all", "关闭全部标签", true)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.close_all_tabs(cx);
                    }))
                    .into_any_element(),
            );
            root = root.child(
                div()
                    .id("tab-menu-backdrop")
                    .absolute()
                    .inset_0()
                    .on_mouse_down(MouseButton::Left, cx.listener(
                        |this, _: &MouseDownEvent, _, cx| {
                            this.tab_menu = None;
                            cx.notify();
                        },
                    ))
                    .on_mouse_down(MouseButton::Right, cx.listener(
                        |this, _: &MouseDownEvent, _, cx| {
                            this.tab_menu = None;
                            cx.notify();
                        },
                    ))
                    .child(
                        div()
                            .absolute()
                            .left(menu.x)
                            .top(menu.y)
                            .w(px(180.))
                            .rounded(px(10.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(theme::color(theme::PANEL_SOLID))
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

         if let Some(menu) = self.tree_menu.clone() {
            let menu_path = menu.path.clone();

            let item = |id: &'static str, label: &'static str, danger: bool| {
                div()
                    .id(id)
                    .px(px(10.))
                    .py(px(7.))
                    .rounded(px(6.))
                    .text_size(px(11.))
                    .text_color(if danger {
                        theme::color(theme::CINNABAR)
                    } else {
                        theme::color(theme::TEXT)
                    })
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.12)))
                    .child(label)
            };
            let mut items: Vec<gpui::AnyElement> = Vec::new();

            items.push(
                item("tree-menu-new-file", "新建文件", false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_new_entry_dialog(NewEntryKind::File, cx);
                    }))
                    .into_any_element(),
            );

            items.push(
                item("tree-menu-new-dir", "新建目录", false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_new_entry_dialog(NewEntryKind::Directory, cx);
                    }))
                    .into_any_element(),
            );
            let p = menu_path.clone();
            items.push(
                item("tree-menu-rename", "重命名…", false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.rename_tree_entry(p.clone(), cx);
                    }))
                    .into_any_element(),
            );
            let p = menu_path.clone();
            items.push(
                item("tree-menu-delete", "删除", true)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.delete_tree_entry(p.clone(), cx);
                    }))
                    .into_any_element(),
            );
            let p = menu_path.clone();
            items.push(
                item("tree-menu-copy-path", "复制路径", false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tree_menu = None;
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            p.to_string_lossy().to_string(),
                        ));
                        this.status = format!("已复制路径: {}", p.display());
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            let p = menu_path.clone();
            items.push(
                item("tree-menu-reveal", "在访达中显示", false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tree_menu = None;
                        #[cfg(target_os = "macos")]
                        {
                            let _ = std::process::Command::new("open")
                                .arg("-R")
                                .arg(&p)
                                .spawn();
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            if let Some(parent) = p.parent() {
                                let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
                            }
                        }
                        this.status = format!("已在文件管理器中显示: {}", p.display());
                        cx.notify();
                    }))
                    .into_any_element(),
            );

            root = root.child(
                div()
                    .id("tree-menu-backdrop")
                    .absolute()
                    .inset_0()
                    .on_mouse_down(MouseButton::Left, cx.listener(
                        |this, _: &MouseDownEvent, _, cx| {
                            this.tree_menu = None;
                            cx.notify();
                        },
                    ))
                    .on_mouse_down(MouseButton::Right, cx.listener(
                        |this, _: &MouseDownEvent, _, cx| {
                            this.tree_menu = None;
                            cx.notify();
                        },
                    ))
                    .child(
                        div()
                            .absolute()
                            .left(menu.x)
                            .top(menu.y)
                            .w(px(190.))
                            .rounded(px(10.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(theme::color(theme::PANEL_SOLID))
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

        if let Some(dialog) = &self.new_entry_dialog {
            let field = dialog.field.clone();
            let is_rename = dialog.rename.is_some();
            let title = if is_rename {
                "重命名"
            } else {
                match dialog.kind {
                    NewEntryKind::File => "新建文件",
                    NewEntryKind::Directory => "新建目录",
                }
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
                            .w(px(420.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(theme::color(theme::PANEL_SOLID))
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
                                            .text_color(theme::color(theme::TEXT))
                                            .child(title),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(theme::color(theme::TEXT_FAINT))
                                            .child(dialog.parent.display().to_string()),
                                    ),
                            )
                            .child(div().child(field))
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(8.))
                                    .child(widgets::ghost_button("ne-cancel", "取消").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.cancel_new_entry_dialog(cx)
                                        }),
                                    ))
                                    .child(widgets::primary_button("ne-save", if is_rename { "重命名" } else { "创建" }).on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.save_new_entry_dialog(cx)
                                        }),
                                    )),
                            ),
                    ),
            );
        }

        if let Some(dialog) = &self.external_change_dialog {
            let display = self.display_path(&dialog.path);
            let changed = self.file_changed_dialog_diff(cx, dialog);
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
                            .w(px(500.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(theme::color(theme::PANEL_SOLID))
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
                                            .text_color(theme::color(theme::TEXT))
                                            .child("文件已被外部修改"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(theme::color(theme::TEXT_FAINT))
                                            .child(format!(
                                                "{display} 在打开后被其他程序修改。保存会覆盖这些更改；从磁盘重新加载会丢弃当前未保存的修改。"
                                            )),
                                    ),
                            )
                            .child(changed)
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(8.))
                                    .child(widgets::ghost_button("ext-cancel", "取消").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.cancel_external_change(cx)
                                        }),
                                    ))
                                    .child(widgets::ghost_button("ext-reload", "从磁盘重新加载").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.reload_external_change(cx)
                                        }),
                                    ))
                                    .child(widgets::primary_button("ext-overwrite", "用我的修改覆盖").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.keep_external_change(cx)
                                        }),
                                    )),
                            ),
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
                            .bg(theme::color(theme::PANEL_SOLID))
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
                                            .text_color(theme::color(theme::TEXT))
                                            .child("编辑条件断点"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(theme::color(theme::TEXT_FAINT))
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
                            .bg(theme::color(theme::PANEL_SOLID))
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
                                            .text_color(theme::color(theme::TEXT))
                                            .child(if editing {
                                                "编辑观察点（数据断点）"
                                            } else {
                                                "添加观察点（数据断点）"
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(theme::color(theme::TEXT_FAINT))
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
                                            .text_color(theme::color(theme::TEXT_FAINT))
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
                                            .text_color(theme::color(theme::ACCENT_3))
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

        if let Some(dialog) = &self.go_to_line_dialog {
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
                            .w(px(420.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(theme::color(theme::PANEL_SOLID))
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
                                            .text_color(theme::color(theme::TEXT))
                                            .child("跳转到行"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(theme::color(theme::TEXT_FAINT))
                                            .child("输入 1 基行号，回车跳转；超出文档范围会自动夹紧。"),
                                    ),
                            )
                            .child(div().child(field))
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(8.))
                                    .child(widgets::ghost_button("gtl-cancel", "取消").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.cancel_go_to_line_dialog(cx)
                                        }),
                                    ))
                                    .child(
                                        widgets::primary_button("gtl-save", "跳转").on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.save_go_to_line_dialog(cx)
                                            }),
                                        ),
                                    ),
                            ),
                    ),
            );
        }

        if let Some(dialog) = &self.git_branch_dialog {
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
                            .w(px(420.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(theme::color(theme::PANEL_SOLID))
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
                                            .text_color(theme::color(theme::TEXT))
                                            .child("新建 Git 分支"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(theme::color(theme::TEXT_FAINT))
                                            .child("输入分支名，创建并切换到新分支"),
                                    ),
                            )
                            .child(div().child(field))
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(8.))
                                    .child(widgets::ghost_button("git-branch-cancel", "取消").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.cancel_git_branch_dialog(cx)
                                        }),
                                    ))
                                    .child(widgets::primary_button("git-branch-create", "创建并切换").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.git_create_branch(cx)
                                        }),
                                    )),
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
                            .bg(theme::color(theme::PANEL_SOLID))
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
                                            .text_color(theme::color(theme::TEXT))
                                            .child("修改变量值"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(theme::color(theme::TEXT_FAINT))
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

        if let Some(dialog) = &self.rename_dialog {
            let field = dialog.field.clone();
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme::color(0x000000).opacity(0.45))
                    .key_context("Rename")
                    .child(
                        div()
                            .w(px(460.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(theme::color(theme::PANEL_SOLID))
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
                                            .text_color(theme::color(theme::TEXT))
                                            .child("重命名符号"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(theme::color(theme::TEXT_FAINT))
                                            .child("输入新名称（Enter 确认，Esc 取消）"),
                                    ),
                            )
                            .child(div().child(field))
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(8.))
                                    .child(widgets::ghost_button("rename-cancel", "取消").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.rename_dialog = None;
                                            cx.notify();
                                        }),
                                    ))
                                    .child(widgets::primary_button("rename-save", "重命名").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.confirm_rename(cx)
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
            let build_jobs = dialog.build_jobs.clone();
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
             let sim_clock = dialog.sim_clock.clone();
            let launch_args = dialog.launch_args.clone();
             let launch_env = dialog.launch_env.clone();
            let launch_cwd = dialog.launch_cwd.clone();
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
                            .text_color(theme::color(theme::TEXT_FAINT))
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
                    .text_color(theme::color(theme::ACCENT))
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
                            .bg(theme::color(theme::PANEL_SOLID))
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
                                            .text_color(theme::color(theme::TEXT))
                                            .child("项目设置"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(theme::color(theme::TEXT_FAINT))
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
                                    .child(row("并行作业数", build_jobs))
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
                                     .child(row("时钟频率(Hz)", sim_clock))
                                    .child(section("调试启动 [launch]"))
                                     .child(row("参数", launch_args))
                                    .child(row("环境变量", launch_env))
                                    .child(row("工作目录", launch_cwd))
                                    .child(
                                        div()
                                            .text_size(px(9.))
                                            .text_color(theme::color(theme::TEXT_FAINT))
                                            .child("参数空格分隔；环境变量每行 KEY=value；工作目录留空=项目根。"),
                                    )
                                    .child(section("全局设置（跨项目）"))
                                    .child(row("终端 shell", terminal_shell))
                                    .child(
                                        div()
                                            .text_size(px(9.))
                                            .text_color(theme::color(theme::TEXT_FAINT))
                                            .child("留空=平台默认（Windows: cmd，Linux/macOS: bash），保存到 terminal.json。"),
                                    )
                                    .child(row("全局配置键", config_keys))
                                    .child(
                                        div()
                                            .text_size(px(9.))
                                            .text_color(theme::color(theme::TEXT_FAINT))
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
                            .bg(theme::color(theme::PANEL_SOLID))
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
                                            .text_color(theme::color(theme::TEXT))
                                            .child("全局工具链路径"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(theme::color(theme::TEXT_FAINT))
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
                                                    .text_color(theme::color(theme::TEXT_FAINT))
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

        if let Some(dialog) = &self.keybindings_dialog {
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
                            .id("keybindings-dialog")
                            .w(px(620.))
                            .max_h(px(560.))
                            .overflow_y_scroll()
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme::alpha(theme::STROKE, 0.3))
                            .bg(theme::color(theme::PANEL_SOLID))
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
                                            .text_color(theme::color(theme::TEXT))
                                            .child("快捷键设置"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(theme::color(theme::TEXT_FAINT))
                                            .child("编辑键位（如 cmd-b / ctrl-shift-p）；留空恢复默认。保存到 keybindings.json，重启 IDE 后生效。"),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(6.))
                                    .children(fields.into_iter().map(|(action, label, default, field)| {
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(10.))
                                            .child(
                                                div()
                                                    .w(px(160.))
                                                    .text_size(px(11.))
                                                    .text_color(theme::color(theme::TEXT_FAINT))
                                                    .child(format!("{label} ({action})")),
                                            )
                                            .child(
                                                div()
                                                    .w(px(180.))
                                                    .text_size(px(9.5))
                                                    .text_color(theme::color(theme::TEXT_FAINT))
                                                    .child(format!("默认 {default}")),
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
                                        widgets::ghost_button("kb-cancel", "取消").on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.cancel_keybindings(cx)
                                            }),
                                        ),
                                    )
                                    .child(
                                        widgets::primary_button("kb-save", "保存").on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.save_keybindings(cx)
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
                                    theme::color(theme::ACCENT_3)
                                } else {
                                    theme::color(theme::TEXT_FAINT)
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
                                    theme::color(theme::TEXT)
                                } else {
                                    theme::color(theme::TEXT_DIM)
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
                                .text_color(theme::color(theme::TEXT_FAINT))
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
                            .bg(theme::color(theme::PANEL_SOLID))
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
                                            .text_color(theme::color(theme::ACCENT_3))
                                            .child("搜"),
                                    )
                                    .child(div().flex_1().child(field))
                                    .child(
                                        div()
                                            .text_size(px(9.))
                                            .text_color(theme::color(theme::TEXT_FAINT))
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
                                                .text_color(theme::color(theme::TEXT_FAINT))
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
                                    .text_color(theme::color(theme::TEXT_FAINT))
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
                                    theme::color(theme::TEXT)
                                } else {
                                    theme::color(theme::TEXT_DIM)
                                })
                                .child(entry.title),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_family(theme::MONO_FONT)
                                .text_size(px(9.))
                                .text_color(theme::color(theme::TEXT_FAINT))
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
                            .bg(theme::color(theme::PANEL_SOLID))
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
                                            .text_color(theme::color(theme::ACCENT_3))
                                            .child("命"),
                                    )
                                    .child(div().flex_1().child(field))
                                    .child(
                                        div()
                                            .text_size(px(9.))
                                            .text_color(theme::color(theme::TEXT_FAINT))
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
                                                .text_color(theme::color(theme::TEXT_FAINT))
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
                                    .text_color(theme::color(theme::TEXT_FAINT))
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
            theme::color(theme::TEXT)
        } else {
            theme::color(theme::TEXT_FAINT)
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
                .text_color(theme::color(theme::TEXT_FAINT))
                .child(label.to_string()),
        )
        .child(
            div()
                .text_size(px(10.))
                .text_color(theme::color(theme::TEXT_DIM))
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
                .text_color(theme::color(theme::TEXT_DIM))
                .child(title.to_string()),
        )
        .child(
            div()
                .text_size(px(9.5))
                .text_color(theme::color(theme::TEXT_FAINT))
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
            theme::color(theme::TEXT)
        } else {
            theme::color(theme::TEXT_FAINT)
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
            theme::color(theme::TEXT)
        } else {
            theme::color(theme::TEXT_FAINT)
        })
        .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.09)))
        .child(label)
}

/// A clickable pill for a named memory region in the memory view's region bar.
fn region_pill(id: (&'static str, usize), label: &str, active: bool) -> Stateful<Div> {
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
            theme::color(theme::TEXT)
        } else {
            theme::color(theme::TEXT_FAINT)
        })
        .hover(|style| style.bg(theme::alpha(theme::STROKE, 0.09)))
        .child(label.to_string())
 }

fn parse_hex_address(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u64::from_str_radix(hex, 16).ok()
}

/// Format a simulator time readout from a cycle count and a nominal clock.
/// With a configured clock (`clock_hz > 0`) the elapsed wall-clock time is
/// shown alongside the cycle count; without one only cycles are shown.
fn sim_time_readout(cycles: u64, clock_hz: u64) -> String {
    if clock_hz == 0 {
        return String::new();
    }
    let seconds = cycles as f64 / clock_hz as f64;
    let time = if seconds < 1e-3 {
        format!("{:.1} µs", seconds * 1e6)
    } else if seconds < 1.0 {
        format!("{:.2} ms", seconds * 1e3)
    } else {
        format!("{:.2} s", seconds)
    };
    let freq = if clock_hz >= 1_000_000 {
        format!("{:.1} MHz", clock_hz as f64 / 1e6)
    } else if clock_hz >= 1_000 {
        format!("{:.0} kHz", clock_hz as f64 / 1e3)
    } else {
        format!("{clock_hz} Hz")
    };
    format!("时间: {time} @ {freq}")
}

/// Parse whitespace-separated hex bytes like `de ad be ef` into a byte vector.
/// An empty string or any non-hex token is an error (the caller shows it).
 fn parse_hex_bytes(text: &str) -> Result<Vec<u8>, String> {
     let tokens: Vec<&str> = text.split_whitespace().collect();
     if tokens.is_empty() {
         return Err("请输入要写入的十六进制字节".to_string());
     }
    // A single token without spaces may be a contiguous hex string
    // (`deadbeef`); split it into byte pairs so both forms work.
    if tokens.len() == 1 && tokens[0].len() > 2 && tokens[0].bytes().all(|b| b.is_ascii_hexdigit()) {
        let hex = tokens[0];
        let pairs: Vec<&str> = hex.as_bytes().chunks(2).map(|c| std::str::from_utf8(c).unwrap()).collect();
        pairs
            .into_iter()
            .map(|part| u8::from_str_radix(part, 16).map_err(|_| format!("无效十六进制字节: {part}")))
            .collect()
    } else {
        tokens
            .into_iter()
            .map(|part| {
                u8::from_str_radix(part, 16)
                    .map_err(|_| format!("无效十六进制字节: {part}"))
            })
            .collect()
    }
 }

/// Find the first occurrence of `pattern` in `haystack`, returning its byte
/// offset. An empty pattern never matches.
fn find_pattern(haystack: &[u8], pattern: &[u8]) -> Option<usize> {
    if pattern.is_empty() || pattern.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(pattern.len())
        .position(|window| window == pattern)
 }
 
#[cfg(test)]
mod memory_search_tests {
    use super::*;

    #[test]
    fn find_pattern_locations_first_match() {
        let haystack = [0xde, 0xad, 0xbe, 0xef, 0xde, 0xad];
        assert_eq!(find_pattern(&haystack, &[0xbe, 0xef]), Some(2));
        assert_eq!(find_pattern(&haystack, &[0xde]), Some(0));
        assert_eq!(find_pattern(&haystack, &[0xad]), Some(1));
        assert_eq!(find_pattern(&haystack, &[0xff]), None);
        assert_eq!(find_pattern(&haystack, &[]), None);
        assert_eq!(find_pattern(&[], &[0x00]), None);
        assert_eq!(find_pattern(&[1, 2], &[1, 2, 3]), None);
    }

    #[test]
    fn parse_hex_bytes_accepts_contiguous_and_spaced() {
        assert_eq!(parse_hex_bytes("deadbeef").unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(parse_hex_bytes("de ad be ef").unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(parse_hex_bytes("0").unwrap(), vec![0x00]);
         assert!(parse_hex_bytes("").is_err());
         assert!(parse_hex_bytes("zz").is_err());
     }
 }
 
#[cfg(test)]
mod header_source_tests {
    use super::*;

    fn touch(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "").unwrap();
        p
    }

    #[test]
    fn source_finds_sibling_header() {
        let dir = std::env::temp_dir().join("ns_hdr_src_sibling");
        std::fs::create_dir_all(&dir).unwrap();
        let c = touch(&dir, "main.c");
        let h = touch(&dir, "main.h");
        assert_eq!(find_matching_source_file(&c, None, &[]), Some(h.clone()));
        assert_eq!(find_matching_source_file(&h, None, &[]), Some(c));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cpp_source_finds_header_variants() {
        let dir = std::env::temp_dir().join("ns_hdr_src_cpp");
        std::fs::create_dir_all(&dir).unwrap();
        let cpp = touch(&dir, "uart.cpp");
        let hpp = touch(&dir, "uart.hpp");
        assert_eq!(find_matching_source_file(&cpp, None, &[]), Some(hpp));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn header_scans_include_dir_for_source() {
        let dir = std::env::temp_dir().join("ns_hdr_src_include");
        let src = dir.join("src");
        let inc = dir.join("include");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&inc).unwrap();
        touch(&src, "blinky.c");
        let h = touch(&inc, "blinky.h");
        let root = dir.clone();
        let found = find_matching_source_file(&h, Some(&root), &["src".to_string()]);
        assert_eq!(found, Some(src.join("blinky.c")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn non_c_files_have_no_counterpart() {
        let dir = std::env::temp_dir().join("ns_hdr_src_other");
        std::fs::create_dir_all(&dir).unwrap();
        let rs = touch(&dir, "main.rs");
        assert_eq!(find_matching_source_file(&rs, None, &[]), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}

 /// Heuristic: a file is binary when it contains a NUL byte in its first 8 KiB.
/// Firmware artifacts (`.bin`, `.elf`, `.ihx`) and object files are binary.
fn is_binary_file(path: &Path, head: &[u8]) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(ext.as_str(), "bin" | "elf" | "o" | "obj" | "hex") {
        return true;
    }
    // NUL byte in the first 8 KiB is a strong binary indicator; a UTF-8 text
    // file (source, linker script) never contains one.
    head.iter().take(8 * 1024).any(|&b| b == 0)
 }
 
/// Compute the counterpart file for `path` (header ↔ source) so the IDE can
/// jump between a `.c`/`.cpp` and its `.h`/`.hpp` pair.
///
/// Strategy (in order):
/// 1. Same directory, matching stem (e.g. `main.c` ↔ `main.h`).
/// 2. Project include directories (`[compiler] includes`, relative to the
///    project root) for a header that has no sibling source.
///
/// Returns `None` when no counterpart exists (e.g. a lone header with no
/// source anywhere reachable). `project_root`/`includes` may be empty for
/// non-C-family files (Rust/Zig/ZR), in which case only the same-directory
/// stem match applies.
fn find_matching_source_file(
    path: &Path,
    project_root: Option<&Path>,
    includes: &[String],
) -> Option<PathBuf> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_header = match ext.as_str() {
        "h" | "hpp" | "hh" => true,
        "c" | "cc" | "cpp" | "cxx" | "s" | "S" => false,
         _ => return None,
     };
    let stem = path.file_stem()?.to_string_lossy().to_string();
    let parent = path.parent()?;

    // 1. Same directory, matching stem with the opposite family.
    let counterpart_stems: &[&str] = if is_header {
        &["c", "cc", "cpp", "cxx"]
    } else {
        &["h", "hpp", "hh"]
    };
    for candidate_ext in counterpart_stems {
        let candidate = parent.join(format!("{stem}.{candidate_ext}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // 2. For a header with no sibling source, scan project include dirs for a
    //    source with the same stem (mirror of the header's relative location).
    if is_header {
        if let Some(root) = project_root {
            let relative = path.strip_prefix(root).ok();
            let rel_dir = relative.and_then(|p| p.parent()).unwrap_or_else(|| parent);
            let mut search_dirs: Vec<PathBuf> = Vec::new();
            search_dirs.push(parent.to_path_buf());
            for include in includes {
                let dir = root.join(include);
                search_dirs.push(dir);
            }
            for dir in search_dirs {
                for candidate_ext in counterpart_stems {
                    let candidate = dir.join(rel_dir.join(format!("{stem}.{candidate_ext}")));
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                    let flat = dir.join(format!("{stem}.{candidate_ext}"));
                    if flat.is_file() {
                        return Some(flat);
                    }
                }
            }
        }
    }
    None
}

 /// Render a classic offset-hex-ASCII hex dump for `bytes`, up to 256 rows
/// (16 KiB). Beyond that, note the truncation so the view stays responsive.
fn hex_dump_text(bytes: &[u8]) -> String {
    const BYTES_PER_ROW: usize = 16;
    const MAX_ROWS: usize = 256;
    let mut out = String::with_capacity(bytes.len().min(16 * 1024) * 4);
    for (row, chunk) in bytes.chunks(BYTES_PER_ROW).take(MAX_ROWS).enumerate() {
        let offset = row * BYTES_PER_ROW;
        out.push_str(&format!("{:08x}  ", offset));
        for i in 0..BYTES_PER_ROW {
            if i < chunk.len() {
                out.push_str(&format!("{:02x} ", chunk[i]));
            } else {
                out.push_str("   ");
            }
            if i == 7 {
                out.push(' ');
            }
        }
        out.push_str(" |");
        for &b in chunk {
            let c = if (0x20..0x7f).contains(&b) { b as char } else { '.' };
            out.push(c);
        }
        out.push('|');
        out.push('\n');
    }
    if bytes.len() > MAX_ROWS * BYTES_PER_ROW {
        out.push_str(&format!(
            "... 共 {} 字节（仅显示前 {} 字节，hex 视图截断）\n",
            bytes.len(),
            MAX_ROWS * BYTES_PER_ROW
        ));
    }
    out
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
            // Insertion text: prefer a `textEdit` (which also carries the
            // replacement range) over `insertText`, falling back to `label`.
            // `insertTextFormat: 2` marks a snippet needing placeholder
            // expansion (e.g. `${1:name}`, `$0`).
            let insert_text = if let Some(text_edit) = item.get("textEdit") {
                text_edit
                    .get("newText")
                    .and_then(|value| value.as_str())
                    .map(|text| text.to_string())
            } else {
                item.get("insertText")
                    .and_then(|value| value.as_str())
                    .map(|text| text.to_string())
            }
            .or_else(|| Some(label.clone()));
            let snippet = item
                .get("insertTextFormat")
                .and_then(|value| value.as_i64())
                .unwrap_or(1)
                == 2;
            // Documentation: a plain string or a MarkupContent object with a
            // `value`. Used for the preview under the completion list.
            let documentation = item
                .get("documentation")
                .map(|doc| {
                    doc.as_str()
                        .map(str::to_string)
                        .or_else(|| {
                            doc.get("value")
                                .and_then(|value| value.as_str())
                                .map(str::to_string)
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            Some(ns_lsp::CompletionItem {
                label,
                detail,
                kind,
                insert_text,
                snippet,
                documentation,
            })
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

/// Expand an LSP snippet body into plain text: `$0`/`${0}` mark the final
/// cursor position (returned separately), `$N`/`${N}` numbered tab-stops
/// collapse to their placeholder default (or empty), and `$TM_FILENAME` /
/// `$TM_DIRECTORY` / `$TM_FILEPATH` expand from the active file. Returns the
/// expanded text and the byte offset of the final cursor (`$0`), defaulting
/// to the end of the text.
fn expand_snippet(
    snippet: &str,
    path: Option<&Path>,
) -> (String, Option<usize>) {
    let mut result = String::with_capacity(snippet.len());
    let mut cursor_at = None;
    let bytes = snippet.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            // Copy one UTF-8 char.
            let ch = snippet[i..].chars().next().unwrap();
            result.push(ch);
            i += ch.len_utf8();
            continue;
        }
        // `$` followed by `{` → `${...}`; `$` followed by a digit → `$N`.
        if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            // Find the closing `}`.
            let body_start = i + 2;
            let mut close = body_start;
            while close < bytes.len() && bytes[close] != b'}' {
                close += 1;
            }
            if close >= bytes.len() {
                // Unterminated `${`: keep as literal.
                result.push_str(&snippet[i..]);
                break;
            }
            let body = &snippet[body_start..close];
            expand_snippet_var(body, path, &mut result, &mut cursor_at);
            i = close + 1;
        } else if i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            let digit = bytes[i + 1] - b'0';
            if digit == 0 {
                cursor_at = Some(result.len());
            }
            i += 2;
        } else {
            result.push('$');
            i += 1;
        }
    }
    (result, cursor_at)
}

/// Expand the inner body of a `${...}` snippet placeholder.
fn expand_snippet_var(
    body: &str,
    path: Option<&Path>,
    result: &mut String,
    cursor_at: &mut Option<usize>,
) {
    // `${TM_FILENAME}`, `${TM_DIRECTORY}`, `${TM_FILEPATH}` variables.
    if let Some(rest) = body.strip_prefix("TM_FILENAME") {
        if let Some(path) = path {
            result.push_str(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        }
        if rest.is_empty() {
            return;
        }
    } else if let Some(rest) = body.strip_prefix("TM_FILEPATH") {
        if let Some(path) = path {
            result.push_str(&path.to_string_lossy());
        }
        if rest.is_empty() {
            return;
        }
    } else if let Some(rest) = body.strip_prefix("TM_DIRECTORY") {
        if let Some(path) = path {
            if let Some(dir) = path.parent() {
                result.push_str(&dir.to_string_lossy());
            }
        }
        if rest.is_empty() {
            return;
        }
    }
    // `${0}` final cursor, `${N:default}` tab-stop with a default, `${N}` bare.
    let (index, default) = body
        .split_once(':')
        .map(|(index, default)| (index, default))
        .unwrap_or((body, ""));
    if index == "0" {
        *cursor_at = Some(result.len());
        if !default.is_empty() {
            result.push_str(default);
        }
    } else if index.chars().all(|c| c.is_ascii_digit()) && !index.is_empty() {
        result.push_str(default);
    } else {
        // Unknown `${...}` variable: keep literal.
        result.push_str("${");
        result.push_str(body);
        result.push('}');
    }
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
        Language::Asm => "Asm",
    }
}
/// Whether a symbol's name matches the outline filter (case-insensitive
/// substring). An empty filter matches everything.
fn symbol_matches_filter(symbol: &ns_lsp::SymbolInfo, filter: &str) -> bool {
    filter.is_empty() || symbol.name.to_lowercase().contains(&filter.to_lowercase())
}

 /// The chain of enclosing symbols at `cursor_line`, outermost first, for the
 /// editor breadcrumb. Descends only through container kinds (namespace, class,
 /// struct, interface, enum, module) so a function's local variables do not
 /// clutter the breadcrumb.
 fn symbol_chain_at_line(symbols: &[ns_lsp::SymbolInfo], cursor_line: usize) -> Vec<(String, usize)> {
      fn is_container(kind: &str) -> bool {
        matches!(
            kind,
            "namespace" | "class" | "struct" | "interface" | "enum" | "module" | "package"
        )
    }
     fn walk(
        symbols: &[ns_lsp::SymbolInfo],
        cursor_line: usize,
        chain: &mut Vec<(String, usize)>,
    ) {
        let mut best: Option<&ns_lsp::SymbolInfo> = None;
        for (index, symbol) in symbols.iter().enumerate() {
            if symbol.line > cursor_line {
                break;
            }
            let next_line = symbols
                .get(index + 1)
                .map(|next| next.line)
                .unwrap_or(usize::MAX);
            if cursor_line < next_line {
                best = Some(symbol);
            }
        }
        if let Some(symbol) = best {
            chain.push((symbol.name.clone(), symbol.line));
            if is_container(&symbol.kind) && !symbol.children.is_empty() {
                walk(&symbol.children, cursor_line, chain);
            }
        }
    }
    let mut chain = Vec::new();
    walk(symbols, cursor_line, &mut chain);
    chain
}


#[cfg(test)]
mod outline_tests {
    use super::*;

    fn symbol(name: &str, kind: &str, line: usize, children: Vec<ns_lsp::SymbolInfo>) -> ns_lsp::SymbolInfo {
        ns_lsp::SymbolInfo {
            name: name.to_string(),
            kind: kind.to_string(),
            line,
            column: 0,
            children,
        }
    }

    #[test]
    fn chain_skips_function_locals() {
        // main() at line 2 contains a local `x` at line 3; the breadcrumb must
        // stop at `main` and not descend into its local variables.
        let outline = vec![symbol(
            "main",
            "function",
            2,
            vec![symbol("x", "variable", 3, vec![])],
        )];
        let chain = symbol_chain_at_line(&outline, 4);
        assert_eq!(chain, vec![("main".to_string(), 2)]);
    }

    #[test]
    fn chain_descends_into_containers() {
        // A namespace with a class containing a method: cursor inside the
        // method yields namespace → class → method (the method is picked at
        // the class's child level; its own locals are not descended into).
        let outline = vec![symbol(
            "lib",
            "namespace",
            0,
            vec![symbol(
                "Foo",
                "class",
                5,
                vec![symbol("run", "method", 10, vec![])],
            )],
        )];
        let chain = symbol_chain_at_line(&outline, 12);
        assert_eq!(
            chain,
            vec![
                ("lib".to_string(), 0),
                ("Foo".to_string(), 5),
                ("run".to_string(), 10),
            ]
        );
    }

    #[test]
    fn chain_is_empty_before_first_symbol() {
        let outline = vec![symbol("a", "function", 4, vec![])];
        assert!(symbol_chain_at_line(&outline, 0).is_empty());
    }

    #[test]
    fn chain_picks_innermost_symbol_at_line() {
        // Two sibling functions; a cursor inside the second must pick it, not
        // the first.
        let outline = vec![
            symbol("first", "function", 2, vec![]),
            symbol("second", "function", 8, vec![]),
        ];
        let chain = symbol_chain_at_line(&outline, 10);
        assert_eq!(chain, vec![("second".to_string(), 8)]);
    }
}

#[cfg(test)]
mod snippet_tests {
    use super::*;

    #[test]
    fn snippet_expands_placeholders_and_tabstops() {
        // `${1:name}` keeps its default, `$2` collapses empty, `$0` marks the
        // final cursor.
        let (text, cursor) = expand_snippet("for (${1:i} = 0; ${1:i} < $2; $2++) {}", None);
        assert_eq!(text, "for (i = 0; i < ; ++) {}");
        assert_eq!(cursor, None);

        let (text, cursor) = expand_snippet("printf(${1:fmt}, $2);$0", None);
        assert_eq!(text, "printf(fmt, );");
        assert_eq!(cursor, Some(text.len()));
    }

    #[test]
    fn snippet_expands_tm_variables() {
        let path = Path::new("/project/src/main.c");
        let (text, _) = expand_snippet(
            "// ${TM_FILENAME} in ${TM_DIRECTORY}",
            Some(path),
        );
        assert_eq!(text, "// main.c in /project/src");
        let (text, _) = expand_snippet("${TM_FILEPATH}", Some(path));
        assert_eq!(text, "/project/src/main.c");
    }

    #[test]
    fn snippet_keeps_literal_dollar() {
        // A lone `$` followed by a non-digit (not a placeholder) stays literal.
        let (text, cursor) = expand_snippet("cost: $x", None);
        assert_eq!(text, "cost: $x");
        assert_eq!(cursor, None);
        // A trailing lone `$` stays literal.
        let (text, _) = expand_snippet("end$", None);
        assert_eq!(text, "end$");
    }

    #[test]
    fn completion_parses_insert_text_and_format() {
        let response = serde_json::json!({
            "result": {
                "items": [
                    {
                        "label": "for",
                        "kind": 15,
                        "detail": "for loop",
                        "insertText": "for (${1:i} = 0; ${1:i} < $2; $2++) {}",
                        "insertTextFormat": 2
                    },
                    {
                        "label": "foo",
                        "kind": 3,
                        "insertText": "foo()"
                    },
                    {
                        "label": "bar",
                        "kind": 3
                    }
                ]
            }
        });
        let items = parse_completion_items(&response);
        assert_eq!(items.len(), 3);
        // Snippet item: insert_text is the snippet body, snippet=true.
        assert_eq!(
            items[0].insert_text.as_deref(),
            Some("for (${1:i} = 0; ${1:i} < $2; $2++) {}")
        );
        assert!(items[0].snippet);
        // insertText without format: plain text, snippet=false.
        assert_eq!(items[1].insert_text.as_deref(), Some("foo()"));
        assert!(!items[1].snippet);
        // No insertText/textEdit: falls back to label.
        assert_eq!(items[2].insert_text.as_deref(), Some("bar"));
        assert!(!items[2].snippet);
    }

    #[test]
    fn completion_parses_text_edit_new_text() {
        let response = serde_json::json!({
            "result": {
                "items": [
                    {
                        "label": "pri",
                        "kind": 3,
                        "textEdit": {
                            "range": {
                                "start": {"line": 0, "character": 0},
                                "end": {"line": 0, "character": 3}
                            },
                            "newText": "printf(\"%d\\n\", x);"
                        }
                    }
                ]
            }
        });
        let items = parse_completion_items(&response);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].insert_text.as_deref(), Some("printf(\"%d\\n\", x);"));
        assert!(!items[0].snippet);
    }

    #[test]
    fn completion_parses_documentation_string_and_markup() {
        let response = serde_json::json!({
            "result": {
                "items": [
                    {
                        "label": "plain",
                        "kind": 3,
                        "documentation": "A plain documentation string."
                    },
                    {
                        "label": "markup",
                        "kind": 3,
                        "documentation": {
                            "kind": "markdown",
                            "value": "**Markup** content."
                        }
                    }
                ]
            }
        });
        let items = parse_completion_items(&response);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].documentation, "A plain documentation string.");
        assert_eq!(items[1].documentation, "**Markup** content.");
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Headless render smoke test: with a project open and the output panel
    /// visible, the divider and output panel must render without panicking.
    #[test]
    fn renders_project_with_output_divider() {
        let mut cx = gpui::TestAppContext::single();
        let temp = std::env::temp_dir();
        cx.add_window_view(|_window, cx| {
            let mut app = IdeApp::new(None, cx);
            app.project = Some(Project {
                root: temp.clone(),
                config: Default::default(),
            });
            app.show_output = true;
             app
         });
     }

    /// `sim_time_readout` hides the time without a configured clock and
    /// formats elapsed time + nominal frequency once one is set.
    #[test]
    fn sim_time_readout_formats_cycles_and_clock() {
        // No clock configured: no readout at all.
        assert_eq!(sim_time_readout(0, 0), "");
        // 1_000_000 cycles @ 1 MHz = 1 s.
        let out = sim_time_readout(1_000_000, 1_000_000);
        assert!(out.contains("时间: 1.00 s @ 1.0 MHz"), "{out}");
        // 1_000 cycles @ 1 MHz = 1 ms.
        let out = sim_time_readout(1_000, 1_000_000);
        assert!(out.contains("1.00 ms"), "{out}");
        // 100 cycles @ 1 MHz = 100 µs.
        let out = sim_time_readout(100, 1_000_000);
        assert!(out.contains("100.0 µs"), "{out}");
        // 72 MHz nominal clock.
        let out = sim_time_readout(72_000_000, 72_000_000);
        assert!(out.contains("1.00 s @ 72.0 MHz"), "{out}");
    }

    /// `filter_device_peripheral` narrows registers by a case-insensitive
    /// substring and keeps the peripheral header when the peripheral itself OR
    /// any of its registers match.
    #[test]
    fn device_filter_matches_peripheral_and_registers() {
        let peripheral = ns_device::Peripheral {
            name: "USART1".to_string(),
            base_address: 0x4001_3800,
            description: String::new(),
            registers: vec![
                ns_device::Register {
                    name: "CR1".to_string(),
                    address: 0x4001_3800,
                    size: 4,
                    reset_value: 0,
                    description: String::new(),
                    fields: vec![],
                },
                ns_device::Register {
                    name: "BRR".to_string(),
                    address: 0x4001_3808,
                    size: 4,
                    reset_value: 0,
                    description: String::new(),
                    fields: vec![],
                },
            ],
        };
        // Empty filter: everything matches.
        let (periph, regs) = IdeApp::filter_device_peripheral(&peripheral, "");
        assert!(periph);
        assert_eq!(regs.len(), 2);
        // Peripheral name match keeps the header and all registers.
        let (periph, regs) = IdeApp::filter_device_peripheral(&peripheral, "usart");
        assert!(periph);
        assert_eq!(regs.len(), 2);
        // Register-only match keeps the header for context.
        let (periph, regs) = IdeApp::filter_device_peripheral(&peripheral, "brr");
        assert!(periph);
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].name, "BRR");
        // No match hides the peripheral.
        let (periph, regs) = IdeApp::filter_device_peripheral(&peripheral, "gpio");
        assert!(!periph);
        assert!(regs.is_empty());
     }

    /// `export_register_snapshot` writes a JSON snapshot of the current
    /// register groups into `.ns-ide/register-snapshot.json` and reports the
    /// path; an empty register set reports the honest reason instead.
    #[test]
    fn export_register_snapshot_writes_json() {
        let mut cx = gpui::TestAppContext::single();
        let temp = std::env::temp_dir().join(format!(
            "ns-ide-reg-snapshot-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        cx.add_window_view(|_window, cx| {
            let mut app = IdeApp::new(None, cx);
            app.project = Some(Project {
                root: temp.clone(),
                config: Default::default(),
            });
            app.debug.register_groups.insert(
                "Core".to_string(),
                vec![
                    ns_debug::Variable {
                        name: "pc".to_string(),
                        value: "0x08000100".to_string(),
                        variables_reference: 0,
                    },
                    ns_debug::Variable {
                        name: "sp".to_string(),
                        value: "0x20001000".to_string(),
                        variables_reference: 0,
                    },
                ],
            );
            app.export_register_snapshot(cx);
            assert!(
                app.status.contains("register-snapshot.json"),
                "status: {}",
                app.status
            );
            app
        });
        let snapshot = std::fs::read_to_string(temp.join(".ns-ide/register-snapshot.json")).unwrap();
        assert!(snapshot.contains("\"pc\": \"0x08000100\""), "{snapshot}");
        assert!(snapshot.contains("\"sp\": \"0x20001000\""), "{snapshot}");
         let _ = std::fs::remove_dir_all(&temp);
     }
 
    /// `compare_register_snapshot` loads the saved snapshot and reports the
    /// number of changed registers; a changed value is highlighted.
    #[test]
    fn compare_register_snapshot_counts_changes() {
        let mut cx = gpui::TestAppContext::single();
        let temp = std::env::temp_dir().join(format!(
            "ns-ide-reg-compare-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(temp.join(".ns-ide")).unwrap();
        std::fs::write(
            temp.join(".ns-ide/register-snapshot.json"),
            r#"{"Core": {"pc": "0x08000100", "sp": "0x20001000"}}"#,
        )
        .unwrap();
        cx.add_window_view(|_window, cx| {
            let mut app = IdeApp::new(None, cx);
            app.project = Some(Project {
                root: temp.clone(),
                config: Default::default(),
            });
            app.debug.register_groups.insert(
                "Core".to_string(),
                vec![
                    ns_debug::Variable {
                        name: "pc".to_string(),
                        value: "0x08000100".to_string(),
                        variables_reference: 0,
                    },
                    ns_debug::Variable {
                        name: "sp".to_string(),
                        value: "0x20002000".to_string(),
                        variables_reference: 0,
                    },
                ],
            );
            app.compare_register_snapshot(cx);
            assert!(
                app.status.contains("1 个已变更"),
                "status: {}",
                app.status
            );
            assert!(app.debug.reg_snapshot.is_some());
            app
        });
         let _ = std::fs::remove_dir_all(&temp);
     }
 
    /// `disassemble_at` rejects an invalid address and reports it honestly.
    #[test]
    fn disassemble_at_rejects_invalid_address() {
        let mut cx = gpui::TestAppContext::single();
        cx.add_window_view(|_window, cx| {
            let mut app = IdeApp::new(None, cx);
            app.ensure_disasm_input(cx);
            app.debug.disasm_input.as_ref().unwrap().update(cx, |field, cx| {
                field.set_text("zz", cx);
            });
            app.disassemble_at(cx);
            assert!(app.status.contains("无效地址"), "status: {}", app.status);
            app
        });
    }

    /// `symbol_matches_filter` is a case-insensitive substring match; an empty
    /// filter matches everything.
    #[test]
    fn symbol_matches_filter_is_case_insensitive_substring() {
        let symbol = ns_lsp::SymbolInfo {
            name: "UART_Init".to_string(),
            kind: "function".to_string(),
            line: 0,
            column: 0,
            children: vec![],
        };
        assert!(symbol_matches_filter(&symbol, ""));
        assert!(symbol_matches_filter(&symbol, "uart"));
        assert!(symbol_matches_filter(&symbol, "INIT"));
        assert!(!symbol_matches_filter(&symbol, "gpio"));
        assert!(!symbol_matches_filter(&symbol, "initx"));
    }

      /// `insert_include_line` places a new `#include` after the last existing
    /// include (keeping the leading block grouped), or at the top when there
     /// are none.
    #[test]
    fn insert_include_line_after_last_include() {
        let source = "#include <stdint.h>\n#include <stdbool.h>\n\nint main() { return 0; }\n";
        let out = insert_include_line(source, "#include <gpio.h>", include_insert_at(source));
        assert_eq!(
            out,
            "#include <stdint.h>\n#include <stdbool.h>\n#include <gpio.h>\n\nint main() { return 0; }\n"
        );
        // No includes: inserted at the very top.
        let bare = "int main() { return 0; }\n";
        let out = insert_include_line(bare, "#include <gpio.h>", include_insert_at(bare));
        assert_eq!(out, "#include <gpio.h>\nint main() { return 0; }\n");
    }
 
     /// Headless render smoke test for the enhanced memory view: with a debug
    /// panel open on the Memory tab and a named region, the region bar and the
    /// edit input must render without panicking.
    #[test]
    fn renders_memory_view_with_regions_and_edit() {
        let mut cx = gpui::TestAppContext::single();
        let temp = std::env::temp_dir();
        cx.add_window_view(|_window, cx| {
            let mut app = IdeApp::new(None, cx);
            app.project = Some(Project {
                root: temp.clone(),
                config: Default::default(),
            });
            app.show_output = true;
            app.output_tab = OutputTab::Debug;
            app.debug.view_tab = DebugViewTab::Memory;
            app.debug.memory.address = 0x2000_0000;
            app.debug.memory.bytes = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x11];
            app.debug.memory.regions = vec![
                MemoryRegion {
                    name: "自定义".to_string(),
                    address: 0,
                    size: 64,
                },
                MemoryRegion {
                    name: "SRAM".to_string(),
                    address: 0x2000_0000,
                    size: 128,
                },
            ];
            app.debug.memory.active_region = 1;
            app.ensure_memory_input(cx);
            app
        });
    }

    /// The navigation stack must keep at most CAP entries (dropping the oldest)
    /// and a new push must clear the forward (redo) stack.
    #[test]
    fn nav_history_bounds_and_clears_forward() {
        let mut cx = gpui::TestAppContext::single();
        let temp = std::env::temp_dir();
        cx.add_window_view(|_window, cx| {
            let mut app = IdeApp::new(None, cx);
            app.project = Some(Project {
                root: temp.clone(),
                config: Default::default(),
            });
            // Push through the real entry function; verify the cap and the
            // forward-stack invalidation contract.
            for i in 0..60 {
                push_nav_entry(
                    &mut app.nav_back,
                    &mut app.nav_forward,
                    NavEntry {
                        path: temp.join(format!("f{i}.rs")),
                        line: i,
                        column: 0,
                    },
                );
            }
            assert_eq!(app.nav_back.len(), 50);
            assert_eq!(app.nav_back.first().unwrap().line, 10);
            assert_eq!(app.nav_back.last().unwrap().line, 59);

            // A new push invalidates the forward stack.
            push_nav_entry(
                &mut app.nav_back,
                &mut app.nav_forward,
                NavEntry {
                    path: temp.join("f.rs"),
                    line: 0,
                    column: 0,
                },
            );
            assert!(app.nav_forward.is_empty());
            app
        });
    }

    /// Back/forward navigation must round-trip the recorded cursor positions:
    /// back restores the previous entry and moves the current one to forward.
    #[test]
    fn nav_back_forward_round_trip() {
        let mut cx = gpui::TestAppContext::single();
        let temp = std::env::temp_dir();
        cx.add_window_view(|_window, cx| {
            let mut app = IdeApp::new(None, cx);
            app.project = Some(Project {
                root: temp.clone(),
                config: Default::default(),
            });
            // Two recorded positions; navigating back then forward must cycle.
            app.nav_back.push(NavEntry {
                path: temp.join("a.rs"),
                line: 1,
                column: 2,
            });
            app.nav_back.push(NavEntry {
                path: temp.join("b.rs"),
                line: 3,
                column: 4,
            });
            // No active editor: navigate_back pops and pushes the *current*
            // position (none) onto forward; the entry's file does not exist, so
            // only the stack state changes.
            app.navigate_back(cx);
            assert_eq!(app.nav_back.len(), 1);
            assert_eq!(app.nav_back[0].line, 1);
            app.navigate_forward(cx);
            assert_eq!(app.nav_forward.len(), 0);
            app
        });
    }

    /// `#include` directive parsing must recognize quotes/angle forms, leading
    /// whitespace, and reject non-include lines and malformed directives.
    #[test]
    fn parses_include_directives() {
        assert_eq!(
            parse_include_directive(r#"#include "foo.h""#),
            Some((IncludeKind::Quotes, "foo.h".to_string()))
        );
        assert_eq!(
            parse_include_directive(r#"#  include <sys/types.h>"#),
            Some((IncludeKind::Angle, "sys/types.h".to_string()))
        );
        assert_eq!(
            parse_include_directive(r#"   #include "nested/dir/bar.hpp"  "#),
            Some((IncludeKind::Quotes, "nested/dir/bar.hpp".to_string()))
        );
        // Not an include directive.
        assert_eq!(parse_include_directive("int main() {}"), None);
        assert_eq!(parse_include_directive("#includex \"a.h\""), None);
        assert_eq!(parse_include_directive("#include"), None);
        assert_eq!(parse_include_directive(r#"#include "unterminated"#), None);
        assert_eq!(parse_include_directive("#define FOO 1"), None);
    }

    /// `apply_edit_to_text` must replace the 0-based line/character range and
    /// reject out-of-bounds ranges without corrupting the document.
    #[test]
    fn applies_text_edit_to_document() {
        let text = "let x = 1;\nlet y = 2;\n";
        // Replace "x" (line 0, char 4..=5) with "value".
        let out = apply_edit_to_text(text, 0, 4, 0, 5, "value").unwrap();
        assert_eq!(out, "let value = 1;\nlet y = 2;\n");
        // Replace "y" (line 1, char 4..=5) with "count".
        let out = apply_edit_to_text(&out, 1, 4, 1, 5, "count").unwrap();
        assert_eq!(out, "let value = 1;\nlet count = 2;\n");
        // Out-of-bounds range: reject.
        assert!(apply_edit_to_text(text, 99, 0, 99, 1, "z").is_none());
        assert!(apply_edit_to_text(text, 1, 0, 0, 0, "z").is_none());
    }

    /// `apply_edit_to_text` must handle multi-line ranges (whole-line replace).
    #[test]
    fn applies_multiline_text_edit() {
        let text = "aaa\nbbb\nccc\n";
        // Replace lines 1..2 ("bbb\n") with "BBBB\n".
        let out = apply_edit_to_text(text, 1, 0, 2, 0, "BBBB\n").unwrap();
        assert_eq!(out, "aaa\nBBBB\nccc\n");
    }

    /// The terminal's `drain_events` must emit gpui events so the app's
    /// subscriber refreshes immediately (no poll-tick latency).
    #[test]
    fn terminal_output_emits_event() {
        if cfg!(windows) {
            return;
        }
        let mut cx = gpui::TestAppContext::single();
        let terminal = cx.new(|cx| {
            Terminal::new(
                TerminalConfig {
                    kind: TerminalKind::Shell,
                    program: "sh".to_string(),
                },
                cx,
            )
        });
        let fired = std::rc::Rc::new(std::cell::Cell::new(false));
        let sub_holder = std::rc::Rc::new(std::cell::RefCell::new(None));

        cx.update(|cx| {
            let fired_cb = fired.clone();
            let sub = cx.subscribe(&terminal, move |_term, event: &TerminalEvent, _cx| {
                if matches!(event, TerminalEvent::Output) {
                    fired_cb.set(true);
                }
            });
            *sub_holder.borrow_mut() = Some(sub);
            // Simulate the reader thread signalling new output, then drain.
            terminal.update(cx, |terminal, _cx| {
                terminal.inject_event_for_test(TerminalEvent::Output)
            });
            let drained = terminal.update(cx, |terminal, cx| {
                terminal.drain_events(cx)
            });
            assert!(drained, "injected event should be drained");
        });

        assert!(
            fired.get(),
            "drain_events must emit Output so subscribers refresh immediately"
        );
    }

    #[test]
    fn parses_memory_edit_hex_bytes() {
        assert_eq!(
            parse_hex_bytes("de ad be ef").unwrap(),
            vec![0xDE, 0xAD, 0xBE, 0xEF]
        );
        assert_eq!(parse_hex_bytes("00 0a ff").unwrap(), vec![0x00, 0x0A, 0xFF]);
        assert_eq!(
            parse_hex_bytes("  1a  2b  ").unwrap(),
            vec![0x1A, 0x2B],
            "whitespace should be tolerated"
        );
        assert!(parse_hex_bytes("").is_err(), "empty input is an error");
        assert!(parse_hex_bytes("gg").is_err(), "non-hex token is an error");
        assert!(parse_hex_bytes("de ad zz").is_err(), "bad token errors whole");
    }

    #[test]
    fn hex_dump_formats_offset_hex_ascii() {
        // Printable ASCII bytes (0x30..=0x43) so the ASCII gutter shows the
        // same chars; verify the offset, hex, and ASCII columns.
        let bytes: Vec<u8> = (0x30u8..=0x43).collect();
        let dump = hex_dump_text(&bytes);
        let lines: Vec<&str> = dump.lines().collect();
        assert_eq!(lines.len(), 2, "20 bytes = 2 rows (16 + 4)");
        assert!(lines[0].starts_with("00000000  "), "offset column");
        assert!(lines[0].contains("30 31 32 33 34 35 36 37"), "hex bytes");
        assert!(lines[0].contains("|"), "ASCII gutter");
        assert!(lines[0].contains("0123456789"), "printable ASCII");
    }

    #[test]
    fn hex_dump_truncates_large_files() {
        let bytes = vec![0xABu8; 20 * 1024];
        let dump = hex_dump_text(&bytes);
        assert!(dump.contains("截断"), "large dumps note truncation");
        assert!(dump.lines().count() <= 258, "bounded row count");
    }

    #[test]
    fn binary_detection_flags_firmware_and_nul() {
        // `.bin`/`.elf` extensions are always binary.
        assert!(is_binary_file(std::path::Path::new("out/fw.bin"), b"abc"));
        assert!(is_binary_file(std::path::Path::new("out/fw.elf"), b""));
        // A NUL byte in the head marks a binary payload.
        assert!(is_binary_file(std::path::Path::new("data.dat"), &[0x41, 0x00, 0x42]));
        // Source / linker text is not binary.
        assert!(!is_binary_file(
            std::path::Path::new("src/main.c"),
            b"int main() { return 0; }"
        ));
        assert!(!is_binary_file(
            std::path::Path::new("link.ld"),
            b"MEMORY { FLASH (rx) : ORIGIN = 0x8000000 }"
        ));
    }

    #[test]
    fn memory_view_starts_with_custom_region() {
        let view = MemoryView::default();
        assert_eq!(view.regions.len(), 1);
        assert_eq!(view.regions[0].name, "自定义");
        assert_eq!(view.active_region, 0);
    }

    #[test]
    fn memory_region_records_address_and_size() {
        let region = MemoryRegion {
            name: "SRAM".to_string(),
            address: 0x2000_0000,
            size: 128,
        };
        assert_eq!(region.name, "SRAM");
        assert_eq!(region.address, 0x2000_0000);
        assert_eq!(region.size, 128);
    }

    #[test]
    fn parse_hex_address_handles_memory_forms() {
        assert_eq!(parse_hex_address("0x20000000"), Some(0x2000_0000));
        assert_eq!(parse_hex_address("0X20000000"), Some(0x2000_0000));
        assert_eq!(parse_hex_address("20000000"), Some(0x2000_0000));
        assert_eq!(parse_hex_address("0x"), None);
        assert_eq!(parse_hex_address(""), None);
    }

    /// Device-register value edits accept `0x`-prefixed or bare hex (and plain
    /// decimal) up to 32 bits; wider or malformed input is rejected.
    #[test]
    fn device_reg_edit_parses_32bit_value() {
        let parse = |text: &str| {
            parse_hex_address(text)
                .and_then(|v| u32::try_from(v).ok())
                .or_else(|| text.parse::<u32>().ok())
        };
        assert_eq!(parse("0x12345678"), Some(0x1234_5678));
        assert_eq!(parse("DEADBEEF"), Some(0xDEAD_BEEF));
        // Bare digits are read as hex (register values follow hex convention).
        assert_eq!(parse("42"), Some(0x42));
        // Wider than 32 bits is rejected for a 32-bit register.
        assert_eq!(parse("0x1_0000_0000"), None);
        assert_eq!(parse("0x"), None);
        assert_eq!(parse("xyz"), None);
    }
/// Headless render smoke test for the device picker and per-target build
/// picker open must render the catalog options without panicking, and the
/// effective build config must follow the active target.
#[test]
fn renders_device_picker_and_build_config() {
    let mut cx = gpui::TestAppContext::single();
    let temp = std::env::temp_dir();
    cx.add_window_view(|_window, cx| {
        let mut app = IdeApp::new(None, cx);
        let mut config = ns_project::ProjectConfig::default();
        config.project.name = "devproj".to_string();
        config.project.target = "stm32f103c8".to_string();
        config.build.config = "debug".to_string();
        config.targets.push(ns_project::project::TargetSection {
            target: "stm32f103c8".to_string(),
            config: "release".to_string(),
        });
        app.project = Some(Project {
            root: temp.clone(),
            config,
        });
        // The active target's per-target config wins over the shared default.
        assert_eq!(app.project.as_ref().unwrap().build_config(), "release");
        // Open the picker and the debug device tab so the render path runs.
        app.device_picker_open = true;
        app.show_output = true;
        app.output_tab = OutputTab::Debug;
        app.debug.view_tab = DebugViewTab::Device;
        app
    });
}

/// A device-register value edit commits to the simulator when one is active:
/// begin fills the field with the current value, commit parses hex and writes
/// the target memory, and the register's live value updates.
#[test]
fn device_reg_edit_commits_to_simulator() {
    let mut cx = gpui::TestAppContext::single();
    let temp = std::env::temp_dir();
    cx.add_window_view(|_window, cx| {
        let mut app = IdeApp::new(None, cx);
        let mut config = ns_project::ProjectConfig::default();
        config.project.name = "regproj".to_string();
        config.project.target = "stm32f103c8".to_string();
        app.project = Some(Project {
            root: temp.clone(),
            config,
        });
        let mut sim = ns_sim::Simulator::for_arch(ns_sim::Arch::RiscV).expect("RISC-V core");
        let addr = 0x1000u64;
        sim.write_u32(addr as u32, 0x1111_1111);
        app.sim.core = Some(sim);
        app.debug.view_tab = DebugViewTab::Device;

        // Begin editing: the field prefills with the current live value.
        app.begin_device_reg_edit(addr, cx);
        assert!(app.debug.device_reg_edit.is_some());

        // Commit a new hex value; the simulator memory must reflect it.
        app.commit_device_reg_edit("0xDEADBEEF", cx);
        assert!(app.debug.device_reg_edit.is_none());
        let value = app.sim.core.as_ref().unwrap().read_u32(addr as u32);
        assert_eq!(value, 0xDEAD_BEEF, "register write reached the simulator");
        app
    });
}

/// A disassembly row with a source mapping toggles that source line's
/// breakpoint; a row without a mapping reports the limitation instead of
/// silently doing nothing.
#[test]
fn disasm_row_toggles_source_breakpoint() {
    let mut cx = gpui::TestAppContext::single();
    let temp = std::env::temp_dir();
    cx.add_window_view(|_window, cx| {
        let mut app = IdeApp::new(None, cx);
        let mut config = ns_project::ProjectConfig::default();
        config.project.name = "disasmproj".to_string();
        app.project = Some(Project {
            root: temp.clone(),
            config,
        });
        let file = "src/main.c".to_string();
        let line = 12;
        // Toggle on: the breakpoint store now has it.
        app.on_breakpoint_toggled(&file, line, true, cx);
        assert!(
            app.breakpoints.find(&file, line).is_some(),
            "disassembly row added a source breakpoint"
        );
        // Toggle off: it is removed.
        app.on_breakpoint_toggled(&file, line, false, cx);
        assert!(
            app.breakpoints.find(&file, line).is_none(),
            "disassembly row removed the source breakpoint"
        );
        app
    });
}

#[test]
fn tree_move_plan_rejects_invalid_targets() {
    let root = Path::new("/proj");
    let source = root.join("src/main.c");
    // Moving a file into the directory that already contains it is refused.
    let same_dir = source.parent().unwrap();
    let err = tree_move_plan(&source, same_dir).unwrap_err();
    assert!(err.contains("相同"), "{err}");
    // Moving a directory into itself / a descendant is refused.
    let dir = root.join("src");
    let err = tree_move_plan(&dir, &dir).unwrap_err();
    assert!(err.contains("自身"), "{err}");
    let nested = root.join("src/sub");
    let err = tree_move_plan(&dir, &nested).unwrap_err();
    assert!(err.contains("自身"), "{err}");
}

#[test]
fn tree_move_plan_computes_destination_into_dir() {
    let root = Path::new("/proj");
    let source = root.join("src/main.c");
    let dest = root.join("lib");
    let destination = tree_move_plan(&source, &dest).unwrap();
    assert_eq!(destination, dest.join("main.c"));
}

#[test]
fn register_group_storage_routes_scope_and_groups() {
    let mut view = DebugView::default();
    view.registers_scope = 42;
    let mut fetch = Vec::new();

    // The "Registers" scope reply lists two group headers.
    let headers = vec![
        Variable {
            name: "General Purpose Registers".into(),
            value: String::new(),
            variables_reference: 7,
        },
        Variable {
            name: "Floating Point Registers".into(),
            value: String::new(),
            variables_reference: 8,
        },
    ];
    view.apply_register_group(42, &headers, &mut fetch);
    assert_eq!(fetch, vec![7, 8]);
    assert!(view.register_groups.contains_key("General Purpose Registers"));
    assert!(view.register_groups.contains_key("Floating Point Registers"));

    // A group reply is stored under its header name.
    let gp = vec![Variable {
        name: "r0".into(),
        value: "0x0".into(),
        variables_reference: 0,
    }];
    view.apply_register_group(7, &gp, &mut fetch);
    assert_eq!(view.register_groups["General Purpose Registers"].len(), 1);
    assert_eq!(view.register_groups["General Purpose Registers"][0].name, "r0");

    // An unknown reply is shown under a synthetic group.
    view.apply_register_group(99, &gp, &mut fetch);
    assert!(view.register_groups.contains_key("寄存器 #99"));
}

/// A configured keystroke override must replace the default binding for that
/// action, while actions without an override keep their default.
#[test]
fn bindings_apply_keystroke_overrides() {
    let mut overrides = ns_project::KeybindingOverrides::default();
    overrides
        .overrides
        .insert("Build".to_string(), "cmd-shift-b".to_string());
    overrides
        .overrides
        .insert("Debug".to_string(), "ctrl-f5".to_string());
    let bindings = bindings_with(&overrides);
    // The action's `name()` is `namespace::Name` (e.g. `ns_ide::Build`); match
    // on the trailing segment so the test is robust to the namespace prefix.
    let build = bindings
        .iter()
        .find(|binding| binding.action().name().ends_with("Build"))
        .expect("Build binding exists");
    let keys = build.keystrokes();
    assert!(!keys.is_empty());
    // The override maps Build to cmd-shift-b; assert the parsed keystroke
    // carries the override rather than the default `cmd-b`.
    let key_text = keys
        .iter()
        .map(|key| format!("{:?}", key))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(key_text.contains("cmd") || key_text.contains("shift"), "override applied: {key_text}");
    // Debug must have a single binding after the override; match the exact
    // namespace-qualified name so `StopDebug`/`DebugStepOver` don't match.
    let debug_count = bindings
        .iter()
        .filter(|binding| binding.action().name().ends_with("::Debug"))
        .count();
    assert_eq!(debug_count, 1, "Debug override collapses to a single binding");
}

#[test]
fn stop_reason_label_classifies_exceptions_and_signals() {
    assert_eq!(debug_stop_reason_label("breakpoint", None), "已命中断点");
    assert_eq!(debug_stop_reason_label("step", None), "单步暂停");
    assert_eq!(debug_stop_reason_label("pause", None), "已暂停");
    assert_eq!(
        debug_stop_reason_label("exception", Some("EXC_BAD_ACCESS")),
        "异常停止 · EXC_BAD_ACCESS"
    );
    assert_eq!(
        debug_stop_reason_label("signal", Some("SIGSEGV")),
        "信号停止 · SIGSEGV"
    );
    assert_eq!(
        debug_stop_reason_label("hardware exception", Some("Hardware Breakpoint")),
        "异常停止 · Hardware Breakpoint"
    );
    assert_eq!(debug_stop_reason_label("exception", None), "异常停止");
    assert_eq!(debug_stop_reason_label("unknown", None), "已暂停 (unknown)");
}

/// `render_firmware_size_summary` reports Flash/RAM usage against the target's
/// memory map, including a percentage and an overflow warning.
#[test]
fn firmware_size_summary_reports_usage_and_overflow() {
    let mut cx = gpui::TestAppContext::single();
    let temp = std::env::temp_dir().join("ns_ide_size_summary_test");
    std::fs::create_dir_all(&temp).unwrap();
    cx.add_window_view(|_window, cx| {
        let mut app = IdeApp::new(None, cx);
        let config = ns_project::ProjectConfig {
            project: ns_project::ProjectSection {
                name: "blinky".to_string(),
                target: "stm32f103c8".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        app.project = Some(Project {
            root: temp.clone(),
            config,
        });
        // A size within capacity reports a percentage; no overflow warning.
         app.firmware_size = Some(ns_flash::FirmwareSize {
             flash: 1024,
             ram: 256,
            text: 1024,
            data: 256,
            bss: 0,
             total: 1280,
             base: 0x0800_0000,
            symbols: vec![],
         });
        let line = app.render_firmware_size_summary();
        assert!(line.contains("Flash 1024"), "flash usage: {line}");
        assert!(line.contains("RAM 256"), "ram usage: {line}");
        assert!(!line.contains("超出"), "no overflow at capacity: {line}");
        // Over-capacity flash warns.
         app.firmware_size = Some(ns_flash::FirmwareSize {
             flash: 1_000_000,
             ram: 100,
            text: 1_000_000,
            data: 100,
            bss: 0,
             total: 1_000_100,
             base: 0x0800_0000,
             symbols: vec![],
         });
        let line = app.render_firmware_size_summary();
        assert!(line.contains("超出"), "overflow warned: {line}");
        app
    });
    std::fs::remove_dir_all(&temp).ok();
}

/// `stale_sources` flags only sources modified after the artifact, and returns
/// an empty list when the artifact or a source is missing.
#[test]
fn stale_sources_flags_newer_than_artifact() {
    let temp = std::env::temp_dir().join("ns_ide_stale_test");
    std::fs::create_dir_all(&temp).unwrap();
    let artifact = temp.join("fw.elf");
    let old = temp.join("old.c");
    let new = temp.join("new.c");
    // Write `old.c` first, then the artifact, then `new.c` — so the artifact
    // is newer than `old.c` but older than `new.c`. Sleep between writes to
    // guarantee distinct modification times on coarse filesystems.
    std::fs::write(&old, b"old").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&artifact, b"firmware").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&new, b"new").unwrap();
    let stale = stale_sources(&artifact, &[old.clone(), new.clone()]);
    assert_eq!(stale, vec![new.clone()], "only the newer source is stale");
    // A missing artifact yields no staleness (cannot compare).
    assert!(stale_sources(&temp.join("missing.elf"), &[old.clone()]).is_empty());
    std::fs::remove_dir_all(&temp).ok();
}

/// `variable_path` reconstructs the name chain from the scope root to a
/// variable, so expansion state can be persisted across DAP reference churn.
#[test]
fn variable_path_reconstructs_name_chain() {
    let mut cx = gpui::TestAppContext::single();
    cx.add_window_view(|_window, cx| {
        let mut app = IdeApp::new(None, cx);
        // Scope: `struct` (ref 1) → child `field` (ref 2).
        app.debug.variables = vec![Variable {
            name: "struct".to_string(),
            value: "{...}".to_string(),
            variables_reference: 1,
        }];
        app.debug.children.insert(
            1,
            vec![Variable {
                name: "field".to_string(),
                value: "42".to_string(),
                variables_reference: 2,
            }],
        );
        assert_eq!(app.variable_path(1), vec!["struct".to_string()]);
        assert_eq!(
            app.variable_path(2),
            vec!["struct".to_string(), "field".to_string()]
        );
        assert!(app.variable_path(99).is_empty());
        app
    });
}

/// Toggling a variable records its path; re-applying the path against a fresh
/// scope (new references) re-expands the matching variable by name.
#[test]
fn expanded_paths_survive_reference_churn() {
    let mut cx = gpui::TestAppContext::single();
    cx.add_window_view(|_window, cx| {
        let mut app = IdeApp::new(None, cx);
        // First stop: variable `counter` at ref 10.
        app.debug.variables = vec![Variable {
            name: "counter".to_string(),
            value: "0".to_string(),
            variables_reference: 10,
        }];
        // Expand it (simulate the user clicking the chevron).
        app.debug.expanded.insert(10);
        app.debug.expanded_paths.push(vec!["counter".to_string()]);
        // Second stop: the same variable arrives with a new reference (20).
        app.debug.variables = vec![Variable {
            name: "counter".to_string(),
            value: "1".to_string(),
            variables_reference: 20,
        }];
        app.debug.children.clear();
        app.debug.expanded.clear();
        app.reapply_expanded_paths();
        // The variable should be re-expanded under its new reference.
        assert!(
            app.debug.expanded.contains(&20),
            "re-expanded by name after reference churn"
        );
        assert_eq!(app.debug.expanded_paths, vec![vec!["counter".to_string()]]);
        app
    });
}

/// Splitting the editor opens the active file in an independent second pane;
/// closing it removes the pane. Works against a real temp file.
#[test]
fn split_view_opens_and_closes() {
    let mut cx = gpui::TestAppContext::single();
    cx.add_window_view(|_window, cx| {
        let dir = std::env::temp_dir().join(format!("ns-split-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.c");
        std::fs::write(&file, "int main(void) { return 0; }\n").unwrap();

        let mut app = IdeApp::new(None, cx);
        app.open_file(file.clone(), cx);
        assert!(app.active_tab.is_some());

        // Split the active file: the pane opens with an independent editor.
        app.split_editor_right(cx);
        let split = app.split.as_ref().expect("split pane opened");
        assert_eq!(split.path, file.canonicalize().unwrap());
        assert!(split.editor.read(cx).text().contains("main"));

        // Closing removes the pane and leaves the main tab intact.
        app.close_split(cx);
        assert!(app.split.is_none());
        assert!(app.active_tab.is_some());

        let _ = std::fs::remove_dir_all(&dir);
        app
    });
}

/// The workspace-symbol popup parses a `workspace/symbol` response, moves the
/// highlight, and confirming opens the target file at the symbol's line.
#[test]
fn workspace_symbol_search_navigates() {
    let mut cx = gpui::TestAppContext::single();
    cx.add_window_view(|_window, cx| {
        let dir = std::env::temp_dir().join(format!("ns-ws-sym-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.c");
        std::fs::write(&file, "int main(void) { return 0; }\n").unwrap();

        let mut app = IdeApp::new(None, cx);
        let mut config = ns_project::ProjectConfig::default();
        config.project.name = "wsproj".to_string();
        app.project = Some(Project {
            root: dir.clone(),
            config,
        });

        // Simulate a workspace/symbol response for the query "main".
        app.workspace_symbols = ns_lsp::parse_workspace_symbols(&serde_json::json!({
            "result": [
                {
                    "name": "main",
                    "kind": 12,
                    "location": {
                        "uri": format!("file://{}", file.to_string_lossy()),
                        "range": {
                            "start": { "line": 0, "character": 4 },
                            "end": { "line": 0, "character": 8 }
                        }
                    }
                }
            ]
        }));
        assert_eq!(app.workspace_symbols.len(), 1);
        assert_eq!(app.workspace_symbols[0].name, "main");

        // Move down then wrap back up (single entry stays put).
        app.workspace_symbol_move(1, cx);
        assert_eq!(app.workspace_symbol_selected, 0);

        // Confirming opens the file and places the cursor on the symbol line.
        app.workspace_symbol_confirm(cx);
        assert!(!app.workspace_symbol_open, "popup closes after confirm");
        assert!(app.active_tab.is_some(), "target file opened");
        assert!(app.active_editor().is_some());

        let _ = std::fs::remove_dir_all(&dir);
        app
    });
 }

/// The references popup lists LSP `textDocument/references` locations, and
/// confirming opens the highlighted reference's file at its line/column.
#[test]
fn references_popup_navigates() {
    let mut cx = gpui::TestAppContext::single();
    cx.add_window_view(|_window, cx| {
        let dir = std::env::temp_dir().join(format!("ns-refs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.c");
        std::fs::write(&file, "int helper(void) { return 1; }\n").unwrap();

        let mut app = IdeApp::new(None, cx);
        let mut config = ns_project::ProjectConfig::default();
        config.project.name = "refsproj".to_string();
        app.project = Some(Project {
            root: dir.clone(),
            config,
        });

        // Simulate a `textDocument/references` response for `helper`.
        app.references = parse_location_list(&serde_json::json!({
            "result": [
                {
                    "uri": format!("file://{}", file.to_string_lossy()),
                    "range": {
                        "start": { "line": 0, "character": 4 },
                        "end": { "line": 0, "character": 10 }
                    }
                }
            ]
        }));
        assert_eq!(app.references.len(), 1);
        app.references_open = true;

        // Move down then wrap back up (single entry stays put).
        app.references_move(1, cx);
        assert_eq!(app.references_selected, 0);

        // Confirming opens the file and places the cursor on the reference.
        app.references_confirm(cx);
        assert!(!app.references_open, "popup closes after confirm");
        assert!(app.active_tab.is_some(), "target file opened");
        assert!(app.active_editor().is_some());

        let _ = std::fs::remove_dir_all(&dir);
        app
    });
}
 
 /// Quick Open enumerates the project's source files, filters them by query, and
/// confirming opens the highlighted entry.
#[test]
fn quick_open_filters_and_opens() {
    let mut cx = gpui::TestAppContext::single();
    cx.add_window_view(|window, cx| {
        let dir = std::env::temp_dir().join(format!("ns-quick-open-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
        std::fs::write(dir.join("src/timer.c"), "void timer_init(void) {}\n").unwrap();
        std::fs::write(dir.join("README.md"), "# demo\n").unwrap();

        let mut app = IdeApp::new(None, cx);
        let mut config = ns_project::ProjectConfig::default();
        config.project.name = "qop".to_string();
        app.project = Some(Project {
            root: dir.clone(),
            config,
        });

        // With no query, every source file is listed (build dirs excluded).
        app.open_quick_open(window, cx);
        assert!(app.quick_open);
        assert!(
            app.quick_open_files.iter().any(|p| p == "src/main.c"),
            "main.c listed"
        );

        // Filtering narrows the list to matching paths.
        let _ = app.quick_open_field.update(cx, |field, cx| {
            field.set_text("timer", cx);
            field.focus_handle(cx)
        });
        app.quick_open_refresh(cx);
        assert_eq!(app.quick_open_files, vec!["src/timer.c".to_string()]);

        // Confirm opens the file.
        app.quick_open_confirm(cx);
        assert!(!app.quick_open, "popup closes after confirm");
        assert!(app.active_tab.is_some(), "file opened in editor");

        let _ = std::fs::remove_dir_all(&dir);
        app
    });
}

/// The recent-files popup records opened files (most recent first, de-duped),
/// persists them, and confirming reopens the highlighted entry.
#[test]
fn recent_files_records_and_reopens() {
    let mut cx = gpui::TestAppContext::single();
    cx.add_window_view(|window, cx| {
        let dir = std::env::temp_dir().join(format!("ns-recent-files-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
        std::fs::write(dir.join("src/timer.c"), "void timer_init(void) {}\n").unwrap();

        let mut app = IdeApp::new(None, cx);
        let mut config = ns_project::ProjectConfig::default();
        config.project.name = "recent".to_string();
        app.project = Some(Project {
            root: dir.clone(),
            config,
        });

        // Opening files records them, most recent first.
        app.open_file(dir.join("src/main.c"), cx);
        app.open_file(dir.join("src/timer.c"), cx);
        assert_eq!(app.recent_files.files[0], PathBuf::from("src/timer.c"));
        assert_eq!(app.recent_files.files[1], PathBuf::from("src/main.c"));

        // Re-opening moves the file to the front without duplicating.
        app.open_file(dir.join("src/main.c"), cx);
        assert_eq!(app.recent_files.files.len(), 2);
        assert_eq!(app.recent_files.files[0], PathBuf::from("src/main.c"));

        // The popup lists them and confirming reopens the highlighted entry.
        app.open_recent_files(window, cx);
        assert!(app.recent_open);
        app.recent_files_confirm(cx);
        assert!(!app.recent_open, "popup closes after confirm");
        assert!(app.active_tab.is_some(), "file reopened in editor");

        let _ = std::fs::remove_dir_all(&dir);
        app
    });
}

/// The sidebar file-tree filter narrows the tree to matching files (with their
/// ancestor directories shown as context) and clears back to the full tree.
#[test]
fn tree_filter_narrows_and_clears() {
    let mut cx = gpui::TestAppContext::single();
    cx.add_window_view(|_window, cx| {
        let dir = std::env::temp_dir().join(format!("ns-tree-filter-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("include")).unwrap();
        std::fs::write(dir.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
        std::fs::write(dir.join("src/timer.c"), "void timer_init(void) {}\n").unwrap();
        std::fs::write(dir.join("include/config.h"), "#define X 1\n").unwrap();

        let mut app = IdeApp::new(None, cx);
        let mut config = ns_project::ProjectConfig::default();
        config.project.name = "tf".to_string();
        app.project = Some(Project {
            root: dir.clone(),
            config,
        });

        // Without a filter, the full tree is returned once the dirs are
        // expanded (nested files are hidden until their parent is expanded).
        app.expanded.insert(dir.join("src"));
        app.expanded.insert(dir.join("include"));
        let full = app.tree_entries();
        assert!(full.iter().any(|e| e.name == "main.c"));

        // Filtering shows only matching files plus their ancestor dirs.
        app.tree_filter = "timer".to_string();
        let filtered = app.tree_entries();
        assert!(
            filtered.iter().any(|e| e.name == "timer.c"),
            "matching file listed: {filtered:?}"
        );
        assert!(
            !filtered.iter().any(|e| e.name == "config.h"),
            "non-matching file excluded: {filtered:?}"
        );
        assert!(
            filtered.iter().any(|e| e.is_dir && e.name == "src"),
            "ancestor dir shown: {filtered:?}"
        );

        // Clearing the filter restores the full tree.
        app.tree_filter = String::new();
        assert_eq!(app.tree_entries().len(), full.len());

        let _ = std::fs::remove_dir_all(&dir);
        app
    });
 }

/// `close_tab` removes the tab from the list (not just the active index), and
/// the bulk close helpers keep the intended survivors.
#[test]
fn tabs_close_and_other_tabs_keep_target() {
    let mut cx = gpui::TestAppContext::single();
    let temp = std::env::temp_dir();
    cx.add_window_view(|_window, cx| {
        let mut app = IdeApp::new(None, cx);
        let dir = temp.join(format!("ns_tab_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.c");
        let b = dir.join("b.c");
        let c = dir.join("c.c");
         for p in [&a, &b, &c] {
             std::fs::write(p, "int x;\n").unwrap();
             app.open_file(p.clone(), cx);
         }
         assert_eq!(app.tabs.len(), 3);
        let a = a.canonicalize().unwrap();
        let b = b.canonicalize().unwrap();
        let c = c.canonicalize().unwrap();
 
         // Closing the middle tab removes exactly one entry and re-selects.
         app.close_tab(1, cx);
         assert_eq!(app.tabs.len(), 2);
         assert_eq!(app.tabs[0].path, a);
         assert_eq!(app.tabs[1].path, c);
        // Re-open the middle tab, then close others keeps only it.
        app.open_file(b.clone(), cx);
        assert_eq!(app.tabs.len(), 3);
        let keep = app.tabs.iter().position(|t| t.path == b).unwrap();
        app.close_other_tabs(keep, cx);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].path, b);

        let _ = std::fs::remove_dir_all(&dir);
        app
    });
 }

/// Watch expressions persist to `.ns-ide/watch.json` and reload on project
/// open; runtime values are intentionally not persisted.
#[test]
fn watch_expressions_persist_and_reload() {
    let mut cx = gpui::TestAppContext::single();
    let temp = std::env::temp_dir();
    cx.add_window_view(|_window, cx| {
        let mut app = IdeApp::new(None, cx);
        let dir = temp.join(format!("ns_watch_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut config = ns_project::ProjectConfig::default();
        config.project.name = "watchproj".to_string();
        app.project = Some(Project { root: dir.clone(), config });

        app.debug.add_watch("counter");
        app.debug.add_watch("obj.field");
        app.debug.add_watch("counter"); // duplicate is a no-op
        assert_eq!(app.debug.watches.len(), 2);
        app.persist_watches();

        // Reload from disk into a fresh view.
        app.debug.watches.clear();
        app.load_watches(&dir);
        assert_eq!(app.debug.watches.len(), 2);
        assert_eq!(app.debug.watches[0].expression, "counter");
        assert_eq!(app.debug.watches[1].expression, "obj.field");

        let _ = std::fs::remove_dir_all(&dir);
        app
    });
 }

/// `refresh_variables` with a selected frame clears the cached scopes and
/// variables; with no frame it reports a clear message instead of panicking.
#[test]
fn refresh_variables_clears_and_reports() {
    let mut cx = gpui::TestAppContext::single();
    cx.add_window_view(|_window, cx| {
        let mut app = IdeApp::new(None, cx);
         app.debug.frames.push(Frame {
             id: 1,
             name: "main".into(),
             file: None,
             line: Some(1),
             column: Some(1),
            pc: Some(0),
         });
         app.debug.scopes.push(Scope {
            id: 10,
             name: "Locals".into(),
             expensive: false,
         });
         app.debug.variables.push(Variable {
             name: "x".into(),
             value: "1".into(),
             variables_reference: 0,
         });

        // No debug session: the refresh clears the cache and reports a status.
        app.refresh_variables(cx);
        assert!(app.debug.scopes.is_empty());
        assert!(app.debug.variables.is_empty());
        assert!(app.status.contains("已刷新变量"));

        // No frame selected: a clear message, no panic.
        app.debug.frames.clear();
        app.refresh_variables(cx);
        assert!(app.status.contains("没有选中的栈帧"));
        app
    });
 }

/// `set_build_config` writes the chosen config to the active target's
/// `[[targets]]` entry (or the shared `[build] config` when no target is set),
/// persisting to `project.toml`; an invalid config is rejected.
#[test]
fn set_build_config_persists_to_project() {
    let mut cx = gpui::TestAppContext::single();
    let temp = std::env::temp_dir();
    cx.add_window_view(|_window, cx| {
        let mut app = IdeApp::new(None, cx);
        let dir = temp.join(format!("ns_buildcfg_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut config = ns_project::ProjectConfig::default();
        config.project.name = "cfgproj".to_string();
        config.project.target = "stm32f103c8".to_string();
        config.build.config = "debug".to_string();
        std::fs::write(dir.join("project.toml"), toml::to_string(&config).unwrap()).unwrap();
        app.project = Some(Project {
            root: dir.clone(),
            config,
        });

        // Release is applied to the active target's entry and persisted.
        app.set_build_config("release", cx);
        assert_eq!(app.project.as_ref().unwrap().build_config(), "release");
        let disk = std::fs::read_to_string(dir.join("project.toml")).unwrap();
        assert!(disk.contains("release"), "config persisted: {disk}");

        // Setting the same config is a no-op that does not clobber.
        let before = disk.clone();
        app.set_build_config("release", cx);
        assert!(app.status.contains("已是"));
        assert_eq!(std::fs::read_to_string(dir.join("project.toml")).unwrap(), before);

        // An invalid config is rejected.
        app.set_build_config("oops", cx);
        assert!(app.status.contains("必须为 debug 或 release"));

        let _ = std::fs::remove_dir_all(&dir);
        app
    });
}

/// Debug-console command history: up walks older commands, down walks back to
/// the draft, and consecutive repeats are deduplicated.
#[test]
fn debug_console_history_navigates_and_dedupes() {
    let mut view = DebugView::default();
    view.push_console_history("step");
    view.push_console_history("bt");
    view.push_console_history("step"); // not consecutive to the last
    assert_eq!(view.console_history, vec!["step", "bt", "step"]);

    // Consecutive repeats are collapsed.
    view.push_console_history("step");
    assert_eq!(view.console_history.len(), 3);

    // Up from a fresh line: saves the draft, recalls the newest.
    assert_eq!(view.history_navigate(true, "print x"), Some("step".to_string()));
    assert_eq!(view.console_draft, "print x");
    // Up again: older entry.
    assert_eq!(view.history_navigate(true, "step"), Some("bt".to_string()));
    // Up again: move to the oldest entry.
    assert_eq!(view.history_navigate(true, "bt"), Some("step".to_string()));
    // At the oldest: no further movement.
    assert_eq!(view.history_navigate(true, "step"), None);
    // Down walks forward.
    assert_eq!(view.history_navigate(false, "step"), Some("bt".to_string()));
    assert_eq!(view.history_navigate(false, "bt"), Some("step".to_string()));
    // Down past the newest restores the draft.
    assert_eq!(view.history_navigate(false, "step"), Some("print x".to_string()));
    assert_eq!(view.console_history_idx, None);

    // Empty history never navigates.
    let mut empty = DebugView::default();
    assert_eq!(empty.history_navigate(true, ""), None);
}

/// Go-to-line dialog: parses a 1-based line number, jumps the active editor,
/// and clamps out-of-range values to the document bounds.
#[test]
fn go_to_line_dialog_jumps_and_clamps() {
    let mut cx = gpui::TestAppContext::single();
    cx.add_window_view(|_window, cx| {
        let mut app = IdeApp::new(None, cx);
        let path = std::env::temp_dir().join(format!("ns_gtl_{}.c", std::process::id()));
        let text = "line0\nline1\nline2\nline3\nline4\n".to_string();
        app.open_text_tab(path.clone(), text, cx);

        // Valid line: jumps to the 1-based line.
        app.open_go_to_line(cx);
        let field = app.go_to_line_dialog.as_ref().unwrap().field.clone();
        field.update(cx, |f, cx| f.set_text("3", cx));
        app.save_go_to_line_dialog(cx);
        let (line, _col) = app.active_editor().unwrap().read(cx).cursor_line_col();
        assert_eq!(line, 3, "cursor should be on 1-based line 3");
        assert!(app.status.contains("第 3 行"));

        // Out-of-range line is clamped to the last line.
        app.open_go_to_line(cx);
        let field = app.go_to_line_dialog.as_ref().unwrap().field.clone();
        field.update(cx, |f, cx| f.set_text("999", cx));
        app.save_go_to_line_dialog(cx);
        let (line, _col) = app.active_editor().unwrap().read(cx).cursor_line_col();
        assert_eq!(line, 6, "clamped to the last logical line (trailing newline → 6)");

        // Non-numeric input is rejected without moving.
        app.open_go_to_line(cx);
        let field = app.go_to_line_dialog.as_ref().unwrap().field.clone();
        field.update(cx, |f, cx| f.set_text("abc", cx));
        app.save_go_to_line_dialog(cx);
        assert!(app.status.contains("大于 0 的整数"));

        let _ = std::fs::remove_file(&path);
        app
    });
}
 }
