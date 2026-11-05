//! Project model for ns-embed-ide.
//!
//! Everything the IDE needs to know about a project lives in the project root
//! `project.toml`. This crate owns that data model, project creation templates,
//! recent-project storage, and lightweight tool discovery.

pub mod breakpoints;
pub mod language;
pub mod project;
pub mod recents;
pub mod toolchain;

pub use breakpoints::{Breakpoint, BreakpointKind, BreakpointStore, breakpoints_path};
pub use language::Language;
pub use project::{
    BuildSection, CompilerSection, CONFIG_FILE, DebuggerSection, FlasherSection, NewProjectSpec,
    Project, ProjectConfig, ProjectSection, SimSection, ValidationIssue,
};
pub use recents::{RecentProject, load_recents, remember_project};
pub use toolchain::{Tool, find_command};
