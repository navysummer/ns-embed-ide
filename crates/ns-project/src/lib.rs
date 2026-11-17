//! Project model for ns-embed-ide.
//!
//! Everything the IDE needs to know about a project lives in the project root
//! `project.toml`. This crate owns that data model, project creation templates,
//! recent-project storage, and lightweight tool discovery.

pub mod bookmarks;
pub mod breakpoints;
pub mod ide_settings;

pub mod config_keys;
pub mod depgraph;
pub mod import;
pub mod keybindings_settings;
pub mod language;
pub mod project;
pub mod recent_files;
pub mod recents;
pub mod session;
pub mod snippets;
pub mod tasks;
pub mod terminal_settings;
pub mod theme_settings;
pub mod toolchain;
pub mod toolchain_config;
pub mod tools;
pub use ide_settings::IdeSettings;

pub use bookmarks::{BOOKMARKS_FILE, Bookmark, BookmarkStore, bookmarks_path};
pub use breakpoints::{
    Breakpoint, BreakpointKind, BreakpointStore, WatchAccess, Watchpoint, breakpoints_path,
};
pub use config_keys::{BuiltinVars, ConfigKeys, expand, expand_field, referenced_keys};
pub use import::{ImportedProject, detect_target, parse_import};
pub use keybindings_settings::{KEYBINDINGS_FILE, KeybindingOverrides};
pub use language::{CTemplate, Language};
pub use project::{
    BuildSection, CONFIG_FILE, CompilerSection, DebuggerSection, FlasherSection, NewProjectSpec,
    Project, ProjectConfig, ProjectSection, SimSection, ValidationIssue,
};
pub use recent_files::RecentFiles;
pub use recents::{RecentProject, forget_project, load_recents, remember_project};
pub use session::{FileView, FoldedFile, SESSION_FILE, Session, session_path};
pub use snippets::{Snippet, SnippetStore};
pub use terminal_settings::{TerminalSettings, default_terminal_shell, terminal_label};
pub use theme_settings::{ThemeMode, ThemeSettings};
pub use toolchain::{Tool, find_command};
pub use toolchain_config::{ToolchainConfig, registry, resolve_tool};
pub use tools::{TOOLS_FILE, ToolCommand, ToolStore, tools_path};
