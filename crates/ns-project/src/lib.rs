//! Project model for ns-embed-ide.
//!
//! Everything the IDE needs to know about a project lives in the project root
//! `project.toml`. This crate owns that data model, project creation templates,
//! recent-project storage, and lightweight tool discovery.

pub mod breakpoints;
pub mod bookmarks;
pub mod ide_settings;

pub mod config_keys;
pub mod depgraph;
pub mod import;
pub mod language;
pub mod project;
pub mod recents;
pub mod recent_files;
pub mod session;
pub mod snippets;
pub mod tasks;
pub mod terminal_settings;
pub mod tools;
pub mod theme_settings;
pub mod keybindings_settings;
pub mod toolchain;
pub mod toolchain_config;
pub use ide_settings::IdeSettings;


pub use bookmarks::{Bookmark, BookmarkStore, BOOKMARKS_FILE, bookmarks_path};
pub use breakpoints::{
    Breakpoint, BreakpointKind, BreakpointStore, WatchAccess, Watchpoint, breakpoints_path,
};
pub use config_keys::{BuiltinVars, ConfigKeys, expand, expand_field, referenced_keys};
pub use import::{ImportedProject, detect_target, parse_import};
pub use keybindings_settings::{KeybindingOverrides, KEYBINDINGS_FILE};
pub use language::{CTemplate, Language};
pub use project::{
    BuildSection, CompilerSection, CONFIG_FILE, DebuggerSection, FlasherSection, NewProjectSpec,
    Project, ProjectConfig, ProjectSection, SimSection, ValidationIssue,
};
pub use recent_files::RecentFiles;
pub use recents::{RecentProject, forget_project, load_recents, remember_project};
pub use session::{FileView, FoldedFile, SESSION_FILE, Session, session_path};
pub use snippets::{Snippet, SnippetStore};
pub use tools::{ToolCommand, ToolStore, TOOLS_FILE, tools_path};
pub use terminal_settings::{TerminalSettings, default_terminal_shell, terminal_label};
pub use theme_settings::{ThemeMode, ThemeSettings};
pub use toolchain::{Tool, find_command};
pub use toolchain_config::{ToolchainConfig, registry, resolve_tool};
