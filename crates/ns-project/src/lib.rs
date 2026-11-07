//! Project model for ns-embed-ide.
//!
//! Everything the IDE needs to know about a project lives in the project root
//! `project.toml`. This crate owns that data model, project creation templates,
//! recent-project storage, and lightweight tool discovery.

pub mod breakpoints;
pub mod config_keys;
pub mod language;
pub mod project;
pub mod recents;
pub mod session;
pub mod toolchain;
pub mod toolchain_config;
pub mod terminal_settings;

pub use config_keys::{BuiltinVars, ConfigKeys, expand, expand_field, referenced_keys};
pub use toolchain_config::{ToolchainConfig, registry, resolve_tool};
pub use terminal_settings::{TerminalSettings, default_terminal_shell, terminal_label};

pub use session::{SESSION_FILE, Session, session_path};

pub use breakpoints::{
    Breakpoint, BreakpointKind, BreakpointStore, WatchAccess, Watchpoint, breakpoints_path,
};
pub use language::{CTemplate, Language};

pub use project::{
    BuildSection, CompilerSection, CONFIG_FILE, DebuggerSection, FlasherSection, NewProjectSpec,
    Project, ProjectConfig, ProjectSection, SimSection, ValidationIssue,
};
pub use recents::{RecentProject, load_recents, remember_project};
pub use toolchain::{Tool, find_command};
