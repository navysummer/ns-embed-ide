//! MCP tools for ns-embed-ide.
//! Each tool wraps a concrete IDE operation and exposes it as an MCP-capable function.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::json;
use walkdir::WalkDir;

use ns_build::{self, Diagnostic, QuickFix, StackUsageEntry, suggest_quick_fix};
use ns_debug::{DebugBreakpoint, DebugCommand, DebugConfig, DebugMessage};
use ns_device::{
    CATALOG, PeripheralConfig, device_for_target, generate_peripheral_init, linker_script,
    memory_map, parse_svd, registers_by_peripheral, startup_file,
};
use ns_flash::firmware::{
    convert_firmware as inner_convert_firmware, decode_intel_hex, detect_format,
};
use ns_flash::{FirmwareFormat, FlashAction, FlashRequest, analyze_firmware_size, load_firmware};
use ns_lsp::{self, LspServer};
use ns_project::recents::load_recents;
use ns_project::session::Session;
use ns_project::toolchain::missing_guide;
use ns_project::{BookmarkStore, BreakpointStore, CTemplate, Language, NewProjectSpec, Project};
use ns_sim::{self, Arch, Simulator};

/// All registered MCP tool names (for validation).
pub const TOOL_NAMES: &[&str] = &[
    "project_info",
    "list_devices",
    "build",
    "clean",
    "read_file",
    "write_file",
    "search_files",
    "read_file_lines",
    "replace_in_files",
    "diff_text",
    "list_files",
    "get_diagnostics",
    "read_binary",
    "open_project",
    "create_project",
    "get_peripherals",
    "generate_linker_script",
    "flash_firmware",
    "run_simulation",
    "mem_search",
    "list_open_files",
    "get_session",
    "save_session",
    "run_tests",
    "toggle_build_config",
    "write_project_field",
    "get_workspace_symbols",
    "get_device_info",
    "read_project_toml",
    "list_build_outputs",
    "run_static_analysis",
    "preprocess_source",
    "generate_compile_commands",
    "read_stack_usage",
    "run_stack_usage",
    "run_coverage",
    "scan_tasks",
    "memory_layout",
    "include_dependencies",
    "suggest_quick_fix",
    "memory_map",
    "get_startup_file",
    "check_toolchain",
    "parse_svd_preview",
    "get_register_details",
    "decode_number",
    "generate_init_code",
    "get_project_issues",
    "get_linker_script",
    "list_catalog_devices",
    "read_recent_projects",
    "compute_artifact_path",
    "analyze_firmware",
    "decode_hex",
    "convert_firmware",
    "decode_hardfault",
    "trace_waveform",
    "profile_hotspots",
    "rtos_tasks",
    "decode_trace",
    "run_command",
    "run_external_tool",
    "update_breakpoint",
    "add_watchpoint",
    "remove_watchpoint",
    "set_watchpoint_enabled",
    "list_external_tools",
    "add_external_tool",
    "remove_external_tool",
    "list_breakpoints",
    "add_breakpoint",
    "remove_breakpoint",
    "list_bookmarks",
    "toggle_bookmark",
    "export_breakpoints",
    "debug_adapter_status",
    "git_status",
    "git_diff",
    "git_stage",
    "git_unstage",
    "git_commit",
    "git_log",
    "git_branches",
    "git_checkout",
    "git_create_branch",
    "git_pull",
    "git_push",
    "import_project",
    "get_toolchain_config",
    "set_toolchain_override",
    "list_toolchain_registry",
    "sim_session_start",
    "sim_session_end",
    "sim_step",
    "sim_run",
    "sim_reset",
    "sim_status",
    "sim_read_mem",
    "sim_write_mem",
    "sim_read_reg",
    "sim_write_reg",
    "sim_disassemble",
    "sim_snapshot_save",
    "sim_snapshot_load",
    "sim_breakpoint_add",
    "sim_breakpoint_remove",
    "sim_breakpoint_list",
    "sim_clear_breakpoints",
    "sim_step_over",
    "sim_step_out",
    "sim_run_to",
    "sim_peripherals",
    "sim_uart_read",
    "sim_load_image",
    "debug_session_start",
    "debug_session_stop",
    "debug_status",
    "debug_continue",
    "debug_pause",
    "debug_step_over",
    "debug_step_into",
    "debug_step_out",
    "debug_step_instruction",
    "debug_breakpoint_set",
    "debug_breakpoint_clear",
    "debug_breakpoint_list",
    "debug_read_memory",
    "debug_write_memory",
    "debug_stacks",
    "debug_variables",
    "debug_read_registers",
    "debug_disassemble",
    "debug_evaluate",
    "lsp_session_start",
    "lsp_session_end",
    "lsp_hover",
    "lsp_definition",
    "lsp_references",
    "lsp_completion",
];

/// Build the schema JSON for each tool.
pub fn list_all() -> Vec<serde_json::Value> {
    [
        ("project_info", "Get info about the current project"),
        ("list_devices", "List all available MCU devices from the catalog"),
        ("build", "Build the current project (compiles source to firmware)"),
        ("clean", "Clean build artifacts for the current project"),
        ("read_file", "Read contents of a file in the project"),
        ("write_file", "Write or overwrite a file in the project"),
        ("search_files", "Search for files by name pattern in the project"),
        ("read_file_lines", "Read a 1-based line range of a text file efficiently"),
        ("replace_in_files", "Replace text across project source files with case/whole-word options and dry-run"),
        ("diff_text", "Compute a unified line diff between two text blobs"),
        ("open_project", "Open (load) a project from a directory path"),
        ("create_project", "Create a new embedded project skeleton"),
        ("get_peripherals", "List peripherals defined in a target's SVD"),
        ("generate_linker_script", "Generate a linker script for a target MCU"),
        ("flash_firmware", "Flash firmware (ELF/HEX/BIN) to target device"),
        ("run_simulation", "Run instruction-level simulation for N steps"),
        ("mem_search", "Search memory for a byte pattern during simulation"),
        ("list_files", "List all source files in the current project"),
        ("get_diagnostics", "Parse and return build diagnostics (errors/warnings) for the current project"),
        ("read_binary", "Read raw binary data from a file at the given offset and length"),
        ("list_open_files", "List files currently open in the IDE editor tabs"),
        ("get_session", "Read the IDE editor session (open tabs + per-file view state)"),
        ("save_session", "Set and persist the IDE editor session (open tabs + active + views)"),
        ("run_tests", "Run unit tests for the current project"),
        ("toggle_build_config", "Toggle build config between debug and release"),
        ("write_project_field", "Write a TOML field in project.toml"),
        ("get_workspace_symbols", "Extract top-level symbols from source files"),
        ("get_device_info", "Get detailed device info (memory map, peripherals, registers) from catalog or SVD file"),
        ("read_project_toml", "Read the raw project.toml contents from the current project root"),
        ("list_build_outputs", "List all files in the project build output directory"),
        ("run_static_analysis", "Run static analysis (clang-tidy/clippy/zig) on the project"),
        ("preprocess_source", "Preprocess C/C++ source files (macro expansion, -E -P)"),
        ("generate_compile_commands", "Regenerate compile_commands.json for clangd"),
        ("read_stack_usage", "Read stack usage analysis (.su files) from build output"),
        ("run_stack_usage", "Compile C/C++ sources with -fstack-usage and return per-function stack depths"),
        ("run_coverage", "Compile with --coverage, run the test binary, and return gcov line coverage"),
        ("scan_tasks", "Scan the project for TODO/FIXME/XXX/HACK/BUG marker tasks"),
        ("memory_layout", "Parse link.ld MEMORY regions and attribute firmware sections to them"),
        ("include_dependencies", "Build the project include dependency graph and DAG layout"),
        ("suggest_quick_fix", "Suggest quick fixes for a build diagnostic by message pattern"),
        ("memory_map", "Get memory regions (FLASH/RAM/PERIPHERALS) for a target MCU"),
        ("get_startup_file", "Get auto-generated startup assembly code for a target MCU"),
        ("check_toolchain", "Check which toolchains are installed and report their versions"),
        ("parse_svd_preview", "Parse an SVD XML file and return top-level structure overview"),
        ("get_register_details", "Get detailed register info (address, reset value, bit fields) for a peripheral on a target MCU"),
        ("decode_number", "Decode a number string from hex/binary/decimal to all four numeric formats"),
        ("generate_init_code", "Generate C init function enabling and configuring named peripherals for a target MCU"),
        ("get_project_issues", "Return project.toml validation issues and missing tool warnings for the current project"),
        ("get_linker_script", "Return the auto-generated linker script (.ld) text for a target MCU"),
        ("list_catalog_devices", "List all built-in catalog device entries with key/label/core/memory sizes"),
        ("read_recent_projects", "List recently opened projects from the IDE recents database"),
        ("compute_artifact_path", "Compute the output artifact path (ELF/BIN/HEX) for a project without building"),
        ("analyze_firmware", "Analyze firmware image size breakdown (FLASH/RAM regions) for a target MCU"),
        ("decode_hex", "Decode an Intel HEX (.hex/.ihx) file into raw bytes with base address"),
        ("convert_firmware", "Convert firmware between formats (ELF -> HEX, HEX -> BIN, etc.)"),
        ("decode_hardfault", "Decode an ARM Cortex-M fault snapshot (CFSR/HFSR/stacked frame) into a human-readable fault report"),
        ("trace_waveform", "Build waveform channels from decoded SWV/ITM trace packets"),
        ("profile_hotspots", "Aggregate sampled program counters into per-function hotspots using the firmware symbol table"),
        ("rtos_tasks", "Enumerate FreeRTOS tasks by walking the scheduler's intrusive lists in a memory image"),
        ("decode_trace", "Decode raw SWO/ITM bytes into SWV packets (Stimulus/Dwt/Exception/Timestamp) and trace statistics"),
        ("run_command", "Run a shell command in the project directory and return its stdout/stderr"),
        ("run_external_tool", "Run a user-defined external tool by id (from .ns-ide/tools.json)"),
        ("update_breakpoint", "Update breakpoint properties (condition/hit-count/log-message/hardware/enabled) at a source location"),
        ("add_watchpoint", "Add a data watchpoint (or change its access type) for a variable expression"),
        ("remove_watchpoint", "Remove a data watchpoint by expression"),
        ("set_watchpoint_enabled", "Enable or disable a data watchpoint"),
        ("list_external_tools", "List user-defined external tool commands in .ns-ide/tools.json"),
        ("add_external_tool", "Add or replace an external tool command"),
        ("remove_external_tool", "Remove an external tool command by id"),
        ("list_breakpoints", "List all breakpoints defined in the current project's .ns-ide/breakpoints.json"),
        ("add_breakpoint", "Add a breakpoint at a specific file path and line number"),
        ("remove_breakpoint", "Remove a breakpoint at a specific file path and line number"),
        ("list_bookmarks", "List all editor bookmarks in the current project"),
        ("toggle_bookmark", "Toggle a bookmark at a specific file path and line number"),
        ("export_breakpoints", "Export all breakpoints to a JSON file for sharing or backup"),
        ("debug_adapter_status", "Check whether the debug adapter (lldb-dap) is installed and accessible"),
        ("git_status", "Show Git repository status (branch + changed files) for the project"),
        ("git_diff", "Show the unified diff for a file, or the full working diff"),
        ("git_stage", "Stage a file (or all changes) in the Git index"),
        ("git_unstage", "Unstage a file (or all changes) from the Git index"),
        ("git_commit", "Commit all staged changes with a message"),
        ("git_log", "List recent Git commits"),
        ("git_branches", "List local branches and mark the current one"),
        ("git_checkout", "Switch to a local Git branch"),
        ("git_create_branch", "Create and switch to a new Git branch"),
        ("git_pull", "Pull from the upstream remote"),
        ("git_push", "Push to the upstream remote"),
        ("import_project", "Import an existing vendor project (.ewp/.uvprojx/.ioc) into ns-embed-ide"),
        ("get_toolchain_config", "Read the global toolchain path overrides"),
        ("set_toolchain_override", "Set (or clear) a global tool path override"),
        ("list_toolchain_registry", "List the available compiler/tool registry for a language"),
        ("sim_session_start", "Start a stateful simulator session and load firmware"),
        ("sim_session_end", "End (drop) a simulator session"),
        ("sim_step", "Step the simulator one or more instructions"),
        ("sim_run", "Run the simulator for N instructions"),
        ("sim_reset", "Reset the simulator to the entry point"),
        ("sim_status", "Read simulator status (PC, cycles, halted, registers)"),
        ("sim_read_mem", "Read a memory range from the simulator"),
        ("sim_write_mem", "Write bytes into the simulator memory"),
        ("sim_read_reg", "Read a register value from the simulator"),
        ("sim_write_reg", "Write a register value in the simulator"),
        ("sim_disassemble", "Disassemble instructions at an address in the simulator"),
        ("sim_snapshot_save", "Save the simulator state to a JSON snapshot file"),
        ("sim_snapshot_load", "Restore the simulator state from a JSON snapshot file"),
        ("sim_breakpoint_add", "Add (or update) a breakpoint at an address in the simulator"),
        ("sim_breakpoint_remove", "Remove a breakpoint from the simulator"),
        ("sim_breakpoint_list", "List simulator breakpoints with hit counts and conditions"),
        ("sim_clear_breakpoints", "Clear all simulator breakpoints"),
        ("sim_step_over", "Step over a call or loop in the simulator"),
        ("sim_step_out", "Step out of the current function in the simulator"),
        ("sim_run_to", "Run the simulator until the PC reaches an address"),
        ("sim_peripherals", "Read simulator peripheral state (GPIO / TIMER / UART)"),
        ("sim_uart_read", "Drain the simulator UART output buffer"),
        ("sim_load_image", "Load a raw byte image into simulator memory"),
        ("debug_session_start", "Start a DAP debug session on a program and return a session id"),
        ("debug_session_stop", "Stop (detach/terminate) a debug session"),
        ("debug_status", "Read debug session status (stopped/continued/terminated, thread, output)"),
        ("debug_continue", "Continue execution in a debug session"),
        ("debug_pause", "Pause execution in a debug session"),
        ("debug_step_over", "Step over a call or loop in a debug session"),
        ("debug_step_into", "Step into a call in a debug session"),
        ("debug_step_out", "Step out of the current function in a debug session"),
        ("debug_step_instruction", "Single-step one machine instruction in a debug session"),
        ("debug_breakpoint_set", "Set (or update) a source breakpoint in a debug session"),
        ("debug_breakpoint_clear", "Clear a source breakpoint in a debug session"),
        ("debug_breakpoint_list", "List source breakpoints in a debug session"),
        ("debug_read_memory", "Read memory from the debuggee"),
        ("debug_write_memory", "Write bytes into the debuggee memory"),
        ("debug_stacks", "Read the call stack for a thread in a debug session"),
        ("debug_variables", "Read variables of a scope in a debug session"),
        ("debug_read_registers", "Read CPU registers in a debug session"),
        ("debug_disassemble", "Disassemble instructions in a debug session"),
        ("debug_evaluate", "Evaluate an expression in a debug session"),
        ("lsp_session_start", "Start (or reuse) a language server session for a language"),
        ("lsp_session_end", "Shut down the language server session for a language"),
        ("lsp_hover", "Request type/signature hover information at a source position"),
        ("lsp_definition", "Jump to the definition of the symbol at a source position"),
        ("lsp_references", "Find references to the symbol at a source position"),
        ("lsp_completion", "Request completion items at a source position"),
    ]
    .iter()
    .map(|(name, desc)| {
        json!({
            "name": name,
            "description": desc,
            "inputSchema": tool_schema(name),
        })
    })
    .collect()
}

fn tool_schema(name: &str) -> serde_json::Value {
    match name {
        "project_info" => json!({"type":"object","properties":{}}),
        "list_devices" => json!({"type":"object","properties":{}}),
        "build" => {
            json!({"type":"object","properties":{"config":{"type":"string","enum":["debug","release"],"default":"debug"}}})
        }
        "clean" => json!({"type":"object","properties":{}}),
        "read_file" => {
            json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"}}})
        }
        "write_file" => {
            json!({"type":"object","required":["path","content"],"properties":{"path":{"type":"string"},"content":{"type":"string"}}})
        }
        "search_files" => {
            json!({"type":"object","properties":{"pattern":{"type":"string"},"max_results":{"type":"integer","default":50}}})
        }
        "read_file_lines" => {
            json!({"type":"object","required":["path"],"properties":{
                "path":{"type":"string"},
                "start":{"type":"integer","default":1},
                "count":{"type":"integer","default":100},
            }})
        }
        "replace_in_files" => {
            json!({"type":"object","required":["from","to"],"properties":{
                "from":{"type":"string"},
                "to":{"type":"string"},
                "case_sensitive":{"type":"boolean","default":false},
                "whole_word":{"type":"boolean","default":false},
                "path":{"type":"string"},
                "dry_run":{"type":"boolean","default":false},
            }})
        }
        "diff_text" => {
            json!({"type":"object","required":["left","right"],"properties":{
                "left":{"type":"string"},
                "right":{"type":"string"},
                "left_label":{"type":"string","default":"left"},
                "right_label":{"type":"string","default":"right"},
            }})
        }
        "open_project" => {
            json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"}}})
        }
        "create_project" => {
            json!({"type":"object","required":["name","location","language"],"properties":{
                "name":{"type":"string"},
                "location":{"type":"string"},
                "language":{"type":"string","enum":["C","C++","Rust","Zig","ZR"]},
                "target":{"type":"string"},
                "compiler":{"type":"string"},
                "simulator":{"type":"string"},
                "debugger":{"type":"string"},
                "flasher":{"type":"string"},
                "template":{"type":"string","enum":["empty","led","uart","freertos"]},
            }})
        }
        "get_peripherals" => {
            json!({"type":"object","required":["target"],"properties":{"target":{"type":"string"}}})
        }
        "generate_linker_script" => {
            json!({"type":"object","required":["target"],"properties":{"target":{"type":"string"}}})
        }
        "flash_firmware" => {
            json!({"type":"object","required":["firmware_path"],"properties":{"firmware_path":{"type":"string"},"verify":{"type":"boolean","default":true}}})
        }
        "run_simulation" => {
            json!({"type":"object","properties":{"steps":{"type":"integer","default":100},"firmware":{"type":"string"}}})
        }
        "mem_search" => {
            json!({"type":"object","required":["start","length","pattern"],"properties":{
                "start":{"type":"integer"},
                "length":{"type":"integer"},
                "pattern":{"type":"array","items":{"type":"integer"}},
                "firmware":{"type":"string"},
            }})
        }
        "list_files" => json!({"type":"object","properties":{}}),
        "get_diagnostics" => json!({"type":"object","properties":{}}),
        "read_binary" => {
            json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"},"offset":{"type":"integer","default":0},"count":{"type":"integer","default":64}}})
        }
        "list_open_files" => json!({"type":"object","properties":{}}),
        "get_session" => json!({"type":"object","properties":{}}),
        "save_session" => {
            json!({"type":"object","properties":{
                "open_files":{"type":"array","items":{"type":"string"},"description":"Project-relative file paths to open, in tab order"},
                "active":{"type":"integer","description":"Index of the active tab"},
                "views":{"type":"array","items":{"type":"object"},"description":"Per-file view state (path, cursor, scroll_ratio, zoom, indent_width, word_wrap)"},
                "split":{"type":"string","description":"Optional project-relative path for the split editor pane"},
            }})
        }
        "run_tests" => json!({"type":"object","properties":{"filter":{"type":"string"}}}),
        "toggle_build_config" => json!({"type":"object","properties":{}}),
        "write_project_field" => {
            json!({"type":"object","required":["path","value"],"properties":{"path":{"type":"string"},"value":{"type":"string"}}})
        }
        "get_workspace_symbols" => json!({"type":"object","properties":{}}),
        "get_device_info" => {
            json!({"type":"object","required":["target"],"properties":{"target":{"type":"string"}}})
        }
        "read_project_toml" => json!({"type":"object","properties":{}}),
        "list_build_outputs" => json!({"type":"object","properties":{}}),
        "run_static_analysis" => json!({"type":"object","properties":{}}),
        "preprocess_source" => json!({"type":"object","properties":{"path":{"type":"string"}}}),
        "generate_compile_commands" => json!({"type":"object","properties":{}}),
        "read_stack_usage" => json!({"type":"object","properties":{}}),
        "run_stack_usage" => json!({"type":"object","properties":{}}),
        "run_coverage" => json!({"type":"object","properties":{}}),
        "scan_tasks" => json!({"type":"object","properties":{"keyword":{"type":"string"}}}),
        "memory_layout" => json!({"type":"object","properties":{}}),
        "include_dependencies" => json!({"type":"object","properties":{}}),
        "suggest_quick_fix" => {
            json!({"type":"object","required":["message"],"properties":{"message":{"type":"string"}}})
        }
        "memory_map" => {
            json!({"type":"object","required":["target"],"properties":{"target":{"type":"string"}}})
        }
        "get_startup_file" => {
            json!({"type":"object","required":["target"],"properties":{"target":{"type":"string"}}})
        }
        "check_toolchain" => json!({"type":"object","properties":{}}),
        "parse_svd_preview" => {
            json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"}}})
        }
        "get_register_details" => {
            json!({"type":"object","required":["target","peripheral"],"properties":{"target":{"type":"string"},"peripheral":{"type":"string"}}})
        }
        "decode_number" => {
            json!({"type":"object","required":["value"],"properties":{"value":{"type":"string"}}})
        }
        "generate_init_code" => {
            json!({"type":"object","required":["target","peripherals"],"properties":{
                "target":{"type":"string"},
                "peripherals":{"type":"array","items":{"type":"object"}},
            }})
        }
        "get_project_issues" => json!({"type":"object","properties":{}}),
        "get_linker_script" => {
            json!({"type":"object","required":["target"],"properties":{"target":{"type":"string"}}})
        }
        "list_catalog_devices" => json!({"type":"object","properties":{}}),
        "read_recent_projects" => json!({"type":"object","properties":{}}),
        "compute_artifact_path" => json!({"type":"object","properties":{}}),
        "analyze_firmware" => {
            json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"},"target":{"type":"string"}}})
        }
        "decode_hex" => {
            json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"}}})
        }
        "convert_firmware" => {
            json!({"type":"object","required":["path","format"],"properties":{"path":{"type":"string"},"format":{"type":"string","enum":["elf","hex","bin"]}}})
        }
        "decode_hardfault" => {
            json!({"type":"object","properties":{
                "snapshot":{"type":"object","description":"A complete FaultSnapshot JSON (cfsr/hfsr/mmfar/bfar/afsr/stacked/lr_exc_return)"},
                "cfsr":{"type":"integer"},
                "hfsr":{"type":"integer"},
                "mmfar":{"type":"integer"},
                "bfar":{"type":"integer"},
                "afsr":{"type":"integer"},
                "lr_exc_return":{"type":"integer"},
                "stacked":{"type":"array","items":{"type":"integer"},"description":"Exception stack frame R0,R1,R2,R3,R12,LR,PC,xPSR"}
            }})
        }
        "trace_waveform" => {
            json!({"type":"object","required":["packets"],"properties":{
                "packets":{"type":"array","items":{"type":"object"},"description":"Decoded SWV/ITM packets (Stimulus/Dwt/Exception/Timestamp)"},
                "channels":{"type":"array","items":{"type":"string"},"description":"Channel ids (stimN/dwt_pc/excN/ts); empty=auto-detect"}
            }})
        }
        "profile_hotspots" => {
            json!({"type":"object","properties":{
                "symbols":{"type":"array","items":{"type":"object"},"description":"Firmware symbols: [{address,size,name}]"},
                "samples":{"type":"array","items":{"type":"integer"},"description":"Sampled program counters (DWT PC-sampling)"}
            }})
        }
        "rtos_tasks" => {
            json!({"type":"object","required":["memory","lists"],"properties":{
                "memory":{"type":"array","items":{"type":"integer"},"description":"Raw RAM image bytes"},
                "base":{"type":"integer","description":"Address of memory[0]"},
                "lists":{"type":"array","items":{"type":"object"},"description":"List heads: [{address,state}] (0=ready,1=blocked,2=suspended,3=deleted)"},
                "running_tcb":{"type":"integer","description":"Address of pxCurrentTCB"},
                "layout":{"type":"object","description":"Optional RtosLayout field overrides"}
            }})
        }
        "decode_trace" => {
            json!({"type":"object","properties":{
                "bytes":{"type":"array","items":{"type":"integer"},"description":"Raw SWO/ITM bytes"},
                "hex":{"type":"string","description":"Hex-encoded SWO/ITM bytes (alternative to bytes)"}
            }})
        }
        "run_command" => {
            json!({"type":"object","required":["command"],"properties":{
                "command":{"type":"string","description":"Shell command line (run via sh -c / cmd /C)"},
                "cwd":{"type":"string","description":"Working directory (empty = project root)"},
                "timeout_ms":{"type":"integer","description":"Optional timeout; the child is killed if it exceeds this many milliseconds"}
            }})
        }
        "run_external_tool" => {
            json!({"type":"object","required":["id"],"properties":{
                "id":{"type":"string","description":"External tool id from .ns-ide/tools.json"},
                "timeout_ms":{"type":"integer","description":"Optional timeout in milliseconds"},
                "args":{"type":"array","items":{"type":"string"},"description":"Optional arguments appended to the tool command"},
            }})
        }
        "update_breakpoint" => {
            json!({"type":"object","required":["path","line"],"properties":{
                "path":{"type":"string"},
                "line":{"type":"integer"},
                "condition":{"type":"string","description":"Non-empty makes it conditional; empty reverts to a line breakpoint"},
                "hit_count":{"type":"string","description":"Pause on the N-th hit; empty clears the limit"},
                "log_message":{"type":"string","description":"Non-empty turns it into a logging breakpoint ({expr} placeholders)"},
                "hardware":{"type":"boolean","description":"Force software or hardware execution breakpoint"},
                "enabled":{"type":"boolean"}
            }})
        }
        "add_watchpoint" => {
            json!({"type":"object","required":["expression"],"properties":{
                "expression":{"type":"string"},
                "access":{"type":"string","enum":["read","write","readwrite"],"default":"write"}
            }})
        }
        "remove_watchpoint" => {
            json!({"type":"object","required":["expression"],"properties":{"expression":{"type":"string"}}})
        }
        "set_watchpoint_enabled" => {
            json!({"type":"object","required":["expression","enabled"],"properties":{
                "expression":{"type":"string"},
                "enabled":{"type":"boolean"}
            }})
        }
        "list_external_tools" => json!({"type":"object","properties":{}}),
        "add_external_tool" => {
            json!({"type":"object","required":["title","command"],"properties":{
                "id":{"type":"string","description":"Stable id (default: slugified title)"},
                "title":{"type":"string"},
                "command":{"type":"string","description":"Shell command line (sh -c / cmd /C)"},
                "cwd":{"type":"string","description":"Working directory (empty = project root)"}
            }})
        }
        "remove_external_tool" => {
            json!({"type":"object","required":["id"],"properties":{"id":{"type":"string"}}})
        }
        "list_breakpoints" => json!({"type":"object","properties":{}}),
        "add_breakpoint" => {
            json!({"type":"object","required":["path","line"],"properties":{"path":{"type":"string"},"line":{"type":"integer"}}})
        }
        "remove_breakpoint" => {
            json!({"type":"object","required":["path","line"],"properties":{"path":{"type":"string"},"line":{"type":"integer"}}})
        }
        "list_bookmarks" => json!({"type":"object","properties":{}}),
        "toggle_bookmark" => {
            json!({"type":"object","required":["path","line"],"properties":{"path":{"type":"string"},"line":{"type":"integer"}}})
        }
        "export_breakpoints" => json!({"type":"object","properties":{"path":{"type":"string"}}}),
        "debug_adapter_status" => json!({"type":"object","properties":{}}),
        "git_status" => json!({"type":"object","properties":{}}),
        "git_diff" => json!({"type":"object","properties":{"path":{"type":"string"}}}),
        "git_stage" => json!({"type":"object","properties":{"path":{"type":"string"}}}),
        "git_unstage" => json!({"type":"object","properties":{"path":{"type":"string"}}}),
        "git_commit" => {
            json!({"type":"object","required":["message"],"properties":{"message":{"type":"string"}}})
        }
        "git_log" => {
            json!({"type":"object","properties":{"limit":{"type":"integer","default":20}}})
        }
        "git_branches" => json!({"type":"object","properties":{}}),
        "git_checkout" => {
            json!({"type":"object","required":["branch"],"properties":{"branch":{"type":"string"}}})
        }
        "git_create_branch" => {
            json!({"type":"object","required":["branch"],"properties":{"branch":{"type":"string"}}})
        }
        "git_pull" => json!({"type":"object","properties":{}}),
        "git_push" => json!({"type":"object","properties":{}}),
        "import_project" => {
            json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"},"location":{"type":"string"}}})
        }
        "get_toolchain_config" => json!({"type":"object","properties":{}}),
        "set_toolchain_override" => {
            json!({"type":"object","required":["tool"],"properties":{"tool":{"type":"string"},"path":{"type":"string"}}})
        }
        "list_toolchain_registry" => {
            json!({"type":"object","properties":{"language":{"type":"string","enum":["C","C++","Rust","Zig","ZR"]}}})
        }
        "sim_session_start" => json!({"type":"object","properties":{"firmware":{"type":"string"}}}),
        "sim_session_end" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"}}})
        }
        "sim_step" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"},"steps":{"type":"integer","default":1}}})
        }
        "sim_run" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"},"steps":{"type":"integer","default":100}}})
        }
        "sim_reset" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"}}})
        }
        "sim_status" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"}}})
        }
        "sim_read_mem" => {
            json!({"type":"object","required":["session","address","count"],"properties":{"session":{"type":"string"},"address":{"type":"integer"},"count":{"type":"integer","default":64}}})
        }
        "sim_write_mem" => {
            json!({"type":"object","required":["session","address","data"],"properties":{"session":{"type":"string"},"address":{"type":"integer"},"data":{"type":"string"},"hex":{"type":"boolean","default":true}}})
        }
        "sim_read_reg" => {
            json!({"type":"object","required":["session","register"],"properties":{"session":{"type":"string"},"register":{"type":"string"}}})
        }
        "sim_write_reg" => {
            json!({"type":"object","required":["session","register","value"],"properties":{"session":{"type":"string"},"register":{"type":"string"},"value":{"type":"integer"}}})
        }
        "sim_disassemble" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"},"address":{"type":"integer"},"count":{"type":"integer","default":16}}})
        }
        "sim_snapshot_save" => {
            json!({"type":"object","required":["session","path"],"properties":{"session":{"type":"string"},"path":{"type":"string"}}})
        }
        "sim_snapshot_load" => {
            json!({"type":"object","required":["session","path"],"properties":{"session":{"type":"string"},"path":{"type":"string"}}})
        }
        "sim_breakpoint_add" => {
            json!({"type":"object","required":["session","address"],"properties":{"session":{"type":"string"},"address":{"type":"integer"},"condition":{"type":"string"},"hit_count":{"type":"integer","default":0}}})
        }
        "sim_breakpoint_remove" => {
            json!({"type":"object","required":["session","address"],"properties":{"session":{"type":"string"},"address":{"type":"integer"}}})
        }
        "sim_breakpoint_list" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"}}})
        }
        "sim_clear_breakpoints" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"}}})
        }
        "sim_step_over" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"},"steps":{"type":"integer","default":10000}}})
        }
        "sim_step_out" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"},"steps":{"type":"integer","default":10000}}})
        }
        "sim_run_to" => {
            json!({"type":"object","required":["session","address"],"properties":{"session":{"type":"string"},"address":{"type":"integer"},"steps":{"type":"integer","default":10000}}})
        }
        "sim_peripherals" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"}}})
        }
        "sim_uart_read" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"}}})
        }
        "sim_load_image" => {
            json!({"type":"object","required":["session","address"],"properties":{"session":{"type":"string"},"address":{"type":"integer"},"data":{"type":"string"},"path":{"type":"string"},"hex":{"type":"boolean","default":true}}})
        }
        "debug_session_start" => {
            json!({"type":"object","required":["program"],"properties":{"program":{"type":"string"},"cwd":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"env":{"type":"array","items":{"type":"string"}},"breakpoints":{"type":"array","items":{"type":"object"}}}})
        }
        "debug_session_stop" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"}}})
        }
        "debug_status" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"}}})
        }
        "debug_continue" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"},"thread":{"type":"integer","default":1}}})
        }
        "debug_pause" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"},"thread":{"type":"integer","default":1}}})
        }
        "debug_step_over" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"},"thread":{"type":"integer","default":1}}})
        }
        "debug_step_into" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"},"thread":{"type":"integer","default":1}}})
        }
        "debug_step_out" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"},"thread":{"type":"integer","default":1}}})
        }
        "debug_step_instruction" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"},"thread":{"type":"integer","default":1}}})
        }
        "debug_breakpoint_set" => {
            json!({"type":"object","required":["session","file","line"],"properties":{"session":{"type":"string"},"file":{"type":"string"},"line":{"type":"integer"},"condition":{"type":"string"},"hit_condition":{"type":"string"},"log_message":{"type":"string"},"hardware":{"type":"boolean","default":false}}})
        }
        "debug_breakpoint_clear" => {
            json!({"type":"object","required":["session","file","line"],"properties":{"session":{"type":"string"},"file":{"type":"string"},"line":{"type":"integer"}}})
        }
        "debug_breakpoint_list" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"}}})
        }
        "debug_read_memory" => {
            json!({"type":"object","required":["session","address","count"],"properties":{"session":{"type":"string"},"address":{"type":"integer"},"count":{"type":"integer","default":64}}})
        }
        "debug_write_memory" => {
            json!({"type":"object","required":["session","address","data"],"properties":{"session":{"type":"string"},"address":{"type":"integer"},"data":{"type":"string"},"hex":{"type":"boolean","default":true}}})
        }
        "debug_stacks" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"},"thread":{"type":"integer","default":1}}})
        }
        "debug_variables" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"},"thread":{"type":"integer","default":1},"frame":{"type":"integer","default":0},"scope":{"type":"string"}}})
        }
        "debug_read_registers" => {
            json!({"type":"object","required":["session"],"properties":{"session":{"type":"string"},"thread":{"type":"integer","default":1},"frame":{"type":"integer","default":0}}})
        }
        "debug_disassemble" => {
            json!({"type":"object","required":["session","address"],"properties":{"session":{"type":"string"},"address":{"type":"integer"},"count":{"type":"integer","default":24}}})
        }
        "debug_evaluate" => {
            json!({"type":"object","required":["session","expression"],"properties":{"session":{"type":"string"},"expression":{"type":"string"},"thread":{"type":"integer","default":1},"frame":{"type":"integer","default":0}}})
        }
        "lsp_session_start" => {
            json!({"type":"object","properties":{"language":{"type":"string","enum":["C","C++","Rust","Zig","ZR"],"description":"Defaults to the project language"}}})
        }
        "lsp_session_end" => {
            json!({"type":"object","properties":{"language":{"type":"string","enum":["C","C++","Rust","Zig","ZR"],"description":"Defaults to the project language"}}})
        }
        "lsp_hover" => {
            json!({"type":"object","required":["path","line","column"],"properties":{
                "path":{"type":"string"},
                "line":{"type":"integer","description":"1-based source line"},
                "column":{"type":"integer","description":"1-based source column"},
                "language":{"type":"string","enum":["C","C++","Rust","Zig","ZR"]},
                "timeout_ms":{"type":"integer","default":10000},
            }})
        }
        "lsp_definition" => {
            json!({"type":"object","required":["path","line","column"],"properties":{
                "path":{"type":"string"},
                "line":{"type":"integer"},
                "column":{"type":"integer"},
                "language":{"type":"string","enum":["C","C++","Rust","Zig","ZR"]},
                "timeout_ms":{"type":"integer","default":10000},
            }})
        }
        "lsp_references" => {
            json!({"type":"object","required":["path","line","column"],"properties":{
                "path":{"type":"string"},
                "line":{"type":"integer"},
                "column":{"type":"integer"},
                "language":{"type":"string","enum":["C","C++","Rust","Zig","ZR"]},
                "timeout_ms":{"type":"integer","default":10000},
            }})
        }
        "lsp_completion" => {
            json!({"type":"object","required":["path","line","column"],"properties":{
                "path":{"type":"string"},
                "line":{"type":"integer"},
                "column":{"type":"integer"},
                "language":{"type":"string","enum":["C","C++","Rust","Zig","ZR"]},
                "timeout_ms":{"type":"integer","default":10000},
            }})
        }
        _ => json!({"type":"object","properties":{}}),
    }
}

/// Execute a tool call by name and arguments. Returns result JSON.
pub fn execute(name: &str, args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    let root = current_project_root();
    match name {
        "project_info" => project_info(&root),
        "list_devices" => Ok(list_devices()),
        "build" => build(&root, args),
        "clean" => clean(&root),
        "read_file" => read_file(&root, arg_str(args, "path")?),
        "write_file" => write_file(&root, arg_str(args, "path")?, arg_str(args, "content")?),
        "search_files" => search_files(&root, &arg_str_opt(args, "pattern").unwrap_or_default()),
        "read_file_lines" => read_file_lines(
            &root,
            arg_str(args, "path")?,
            args.get("start")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or(1),
            args.get("count")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or(100),
        ),
        "replace_in_files" => replace_in_files_tool(&root, args),
        "diff_text" => diff_text_tool(args),
        "open_project" => open_project(arg_str(args, "path")?),
        "create_project" => create_project(args),
        "get_peripherals" => get_peripherals(arg_str(args, "target")?),
        "generate_linker_script" => generate_linker_script(arg_str(args, "target")?),
        "list_files" => list_files(&root),
        "get_diagnostics" => get_diagnostics(&root),
        "read_binary" => read_binary(
            &root,
            arg_str(args, "path")?,
            args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
            args.get("count").and_then(|v| v.as_u64()).unwrap_or(64) as usize,
        ),
        "list_open_files" => list_open_files(&root),
        "get_session" => get_session(&root),
        "save_session" => save_session(&root, args),
        "run_tests" => run_tests(&root, args),
        "toggle_build_config" => toggle_build_config(&root),
        "write_project_field" => {
            write_project_field(&root, arg_str(args, "path")?, arg_str(args, "value")?)
        }
        "get_workspace_symbols" => get_workspace_symbols(&root),
        "get_linker_script" => get_linker_script(arg_str(args, "target")?),
        "list_catalog_devices" => list_catalog_devices(),
        "read_recent_projects" => read_recent_projects(),
        "compute_artifact_path" => compute_artifact_path(&root),
        "get_device_info" => get_device_info(arg_str(args, "target")?),
        "read_project_toml" => read_project_toml(&root),
        "list_build_outputs" => list_build_outputs(&root),
        "run_static_analysis" => run_static_analysis(&root),
        "preprocess_source" => preprocess_source(&root),
        "generate_compile_commands" => generate_compile_commands(&root),
        "read_stack_usage" => read_stack_usage(&root),
        "run_stack_usage" => run_stack_usage(&root),
        "run_coverage" => run_coverage(&root),
        "scan_tasks" => scan_tasks(&root, arg_str_opt(args, "keyword").as_deref()),
        "memory_layout" => memory_layout(&root),
        "include_dependencies" => include_dependencies(&root),
        "suggest_quick_fix" => suggest_quick_fix_tool(arg_str(args, "message")?),
        "memory_map" => get_memory_map(arg_str(args, "target")?),
        "get_startup_file" => get_startup_file(arg_str(args, "target")?),
        "check_toolchain" => check_toolchain(),
        "parse_svd_preview" => parse_svd_preview(arg_str(args, "path")?),
        "get_register_details" => {
            get_register_details(arg_str(args, "target")?, arg_str(args, "peripheral")?)
        }
        "decode_number" => decode_number(arg_str(args, "value")?),
        "generate_init_code" => generate_init_code(args),
        "get_project_issues" => get_project_issues(&root),
        "analyze_firmware" => analyze_firmware(
            arg_str(args, "path")?,
            args.get("target").and_then(|v| v.as_str()).unwrap_or(""),
        ),
        "decode_hex" => decode_hex(arg_str(args, "path")?),
        "convert_firmware" => convert_firmware(arg_str(args, "path")?, arg_str(args, "format")?),
        "decode_hardfault" => decode_hardfault(args),
        "trace_waveform" => trace_waveform(args),
        "profile_hotspots" => profile_hotspots(args),
        "rtos_tasks" => rtos_tasks(args),
        "decode_trace" => decode_trace(args),
        "run_command" => run_command(&root, args),
        "run_external_tool" => run_external_tool(&root, args),
        "update_breakpoint" => update_breakpoint(&root, args),
        "add_watchpoint" => add_watchpoint(&root, args),
        "remove_watchpoint" => remove_watchpoint(&root, arg_str(args, "expression")?),
        "set_watchpoint_enabled" => set_watchpoint_enabled(&root, args),
        "list_external_tools" => list_external_tools(&root),
        "add_external_tool" => add_external_tool(&root, args),
        "remove_external_tool" => remove_external_tool(&root, arg_str(args, "id")?),
        "list_breakpoints" => list_breakpoints(&root),
        "add_breakpoint" => add_breakpoint(&root, arg_str(args, "path")?, arg_usize(args, "line")?),
        "remove_breakpoint" => {
            remove_breakpoint(&root, arg_str(args, "path")?, arg_usize(args, "line")?)
        }
        "list_bookmarks" => list_bookmarks(&root),
        "toggle_bookmark" => {
            toggle_bookmark(&root, arg_str(args, "path")?, arg_usize(args, "line")?)
        }
        "export_breakpoints" => export_breakpoints(&root, args),
        "debug_adapter_status" => debug_adapter_status(),
        "flash_firmware" => flash_firmware(args),
        "run_simulation" => run_simulation(args),
        "mem_search" => mem_search(args),
        "git_status" => git_status(&root),
        "git_diff" => git_diff(&root, arg_str_opt(args, "path").as_deref()),
        "git_stage" => git_stage(&root, arg_str_opt(args, "path").as_deref()),
        "git_unstage" => git_unstage(&root, arg_str_opt(args, "path").as_deref()),
        "git_commit" => git_commit(&root, arg_str(args, "message")?),
        "git_log" => git_log(&root, args),
        "git_branches" => git_branches(&root),
        "git_checkout" => git_checkout(&root, arg_str(args, "branch")?),
        "git_create_branch" => git_create_branch(&root, arg_str(args, "branch")?),
        "git_pull" => git_pull(&root),
        "git_push" => git_push(&root),
        "import_project" => import_project(args),
        "get_toolchain_config" => get_toolchain_config(),
        "set_toolchain_override" => set_toolchain_override(
            arg_str(args, "tool")?,
            arg_str_opt(args, "path").as_deref().unwrap_or(""),
        ),
        "list_toolchain_registry" => {
            list_toolchain_registry(arg_str_opt(args, "language").as_deref().unwrap_or("C"))
        }
        "sim_session_start" => sim_session_start(args),
        "sim_session_end" => sim_session_end(arg_str(args, "session")?),
        "sim_step" => sim_step(arg_str(args, "session")?, args),
        "sim_run" => sim_run(arg_str(args, "session")?, args),
        "sim_reset" => sim_reset(arg_str(args, "session")?),
        "sim_status" => sim_status(arg_str(args, "session")?),
        "sim_read_mem" => sim_read_mem(arg_str(args, "session")?, args),
        "sim_write_mem" => sim_write_mem(arg_str(args, "session")?, args),
        "sim_read_reg" => sim_read_reg(arg_str(args, "session")?, arg_str(args, "register")?),
        "sim_write_reg" => {
            sim_write_reg(arg_str(args, "session")?, arg_str(args, "register")?, args)
        }
        "sim_disassemble" => sim_disassemble(arg_str(args, "session")?, args),
        "sim_snapshot_save" => sim_snapshot_save(arg_str(args, "session")?, arg_str(args, "path")?),
        "sim_snapshot_load" => sim_snapshot_load(arg_str(args, "session")?, arg_str(args, "path")?),
        "sim_breakpoint_add" => sim_breakpoint_add(arg_str(args, "session")?, args),
        "sim_breakpoint_remove" => sim_breakpoint_remove(arg_str(args, "session")?, args),
        "sim_breakpoint_list" => sim_breakpoint_list(arg_str(args, "session")?),
        "sim_clear_breakpoints" => sim_clear_breakpoints(arg_str(args, "session")?),
        "sim_step_over" => sim_step_over(arg_str(args, "session")?, args),
        "sim_step_out" => sim_step_out(arg_str(args, "session")?, args),
        "sim_run_to" => sim_run_to(arg_str(args, "session")?, args),
        "sim_peripherals" => sim_peripherals(arg_str(args, "session")?),
        "sim_uart_read" => sim_uart_read(arg_str(args, "session")?),
        "sim_load_image" => sim_load_image(arg_str(args, "session")?, args),
        "debug_session_start" => debug_session_start(args),
        "debug_session_stop" => debug_session_stop(arg_str(args, "session")?),
        "debug_status" => debug_status(arg_str(args, "session")?),
        "debug_continue" => debug_continue(arg_str(args, "session")?, args),
        "debug_pause" => debug_pause(arg_str(args, "session")?, args),
        "debug_step_over" => debug_step_over(arg_str(args, "session")?, args),
        "debug_step_into" => debug_step_into(arg_str(args, "session")?, args),
        "debug_step_out" => debug_step_out(arg_str(args, "session")?, args),
        "debug_step_instruction" => debug_step_instruction(arg_str(args, "session")?, args),
        "debug_breakpoint_set" => debug_breakpoint_set(arg_str(args, "session")?, args),
        "debug_breakpoint_clear" => debug_breakpoint_clear(arg_str(args, "session")?, args),
        "debug_breakpoint_list" => debug_breakpoint_list(arg_str(args, "session")?),
        "debug_read_memory" => debug_read_memory(arg_str(args, "session")?, args),
        "debug_write_memory" => debug_write_memory(arg_str(args, "session")?, args),
        "debug_stacks" => debug_stacks(arg_str(args, "session")?, args),
        "debug_variables" => debug_variables(arg_str(args, "session")?, args),
        "debug_read_registers" => debug_read_registers(arg_str(args, "session")?, args),
        "debug_disassemble" => debug_disassemble(arg_str(args, "session")?, args),
        "debug_evaluate" => debug_evaluate(arg_str(args, "session")?, args),
        "lsp_session_start" => lsp_session_start(&root, args),
        "lsp_session_end" => lsp_session_end(&root, args),
        "lsp_hover" => lsp_hover(&root, args),
        "lsp_definition" => lsp_definition(&root, args),
        "lsp_references" => lsp_references(&root, args),
        "lsp_completion" => lsp_completion(&root, args),
        _ => Err(anyhow::anyhow!("Unknown tool: {}", name)),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn current_project_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

// ── Stateful simulator sessions ──────────────────────────────────────────────

/// A live simulator session. `Simulator` (not `SimState`) is kept so that
/// memory/register writes persist across MCP calls.
struct SimSession {
    simulator: Simulator,
    name: String,
    /// Program entry captured at session start, used by `sim_reset`.
    entry: u32,
}

fn sim_sessions() -> &'static Mutex<Vec<SimSession>> {
    static SESSIONS: OnceLock<Mutex<Vec<SimSession>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(Vec::new()))
}

static SIM_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Sentinel register index meaning "the program counter" (which is not part of
/// the architectural register file).
const PC_REGISTER: usize = usize::MAX;

fn with_sim_session<R>(session: &str, f: impl FnOnce(&mut Simulator) -> Result<R>) -> Result<R> {
    let mut sessions = sim_sessions().lock().unwrap_or_else(|e| e.into_inner());
    let idx = sessions
        .iter()
        .position(|s| s.name == session)
        .ok_or_else(|| {
            anyhow::anyhow!("模拟器会话不存在: {session}（先调用 sim_session_start）")
        })?;
    f(&mut sessions[idx].simulator)
}

fn step_status(result: ns_sim::StepResult) -> &'static str {
    match result {
        ns_sim::StepResult::Halted => "halted",
        ns_sim::StepResult::Breakpoint => "breakpoint",
        ns_sim::StepResult::Advanced => "advanced",
    }
}

/// Resolve a register argument (`"pc"`, a numeric index, or a name such as
/// `"x5"` / `"r3"`) to an index. `pc` is mapped to [`PC_REGISTER`].
fn resolve_reg_index(arch: Arch, register: &str) -> Result<usize> {
    let lower = register.trim().to_ascii_lowercase();
    if lower == "pc" {
        return Ok(PC_REGISTER);
    }
    // RISC-V registers are commonly named `xN` (e.g. `x5`) as well as by ABI
    // name (`t0`); accept both.
    if arch == Arch::RiscV {
        if let Some(num) = lower.strip_prefix('x') {
            if let Ok(n) = num.parse::<usize>() {
                if n < ns_sim::reg_count_for(arch) {
                    return Ok(n);
                }
            }
        }
    }
    if let Ok(n) = lower.parse::<usize>() {
        if n < ns_sim::reg_count_for(arch) {
            return Ok(n);
        }
        bail!(
            "寄存器索引越界: {n}（{} 共有 {} 个寄存器）",
            arch.label(),
            ns_sim::reg_count_for(arch)
        );
    }
    for i in 0..ns_sim::reg_count_for(arch) {
        if ns_sim::reg_label_for(arch, i).eq_ignore_ascii_case(&lower) {
            return Ok(i);
        }
    }
    bail!("未知寄存器: {register}")
}

fn decode_hex_bytes(input: &str) -> Result<Vec<u8>> {
    let cleaned: String = input.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if cleaned.len() % 2 != 0 {
        bail!(
            "hex 数据长度必须为偶数（收到 {} 个 hex 字符）",
            cleaned.len()
        );
    }
    let mut out = Vec::with_capacity(cleaned.len() / 2);
    for i in (0..cleaned.len()).step_by(2) {
        let byte = u8::from_str_radix(&cleaned[i..i + 2], 16)
            .with_context(|| format!("无效 hex 字节: {}", &cleaned[i..i + 2]))?;
        out.push(byte);
    }
    Ok(out)
}

/// Byte length of the instruction at `pc`, mirroring the decode width the
/// simulator uses for each architecture. Used to walk disassembly.
fn instruction_len(arch: Arch, pc: u32, read: &dyn Fn(u32) -> u8, arm_state: bool) -> u32 {
    match arch {
        Arch::RiscV => 4,
        Arch::Arm => {
            if arm_state {
                4
            } else {
                let half = read(pc) as u16 | ((read(pc.wrapping_add(1)) as u16) << 8);
                if half & 0xE000 == 0xE000 { 4 } else { 2 }
            }
        }
        Arch::Xtensa => 3,
        Arch::Avr => {
            let word = (read(pc.wrapping_add(1)) as u16) << 8 | read(pc) as u16;
            match word & 0xFE0F {
                0x9000 | 0x9200 | 0x940C | 0x940E => 4,
                _ => 2,
            }
        }
        Arch::Msp430 | Arch::V850 | Arch::Stm8 | Arch::Rx | Arch::Rl78 => 2,
        Arch::Mcs51 => match read(pc) {
            0x02 | 0x12 | 0x90 => 3,
            0x74 | 0x80 | 0x60 | 0x70 | 0x40 | 0x50 => 2,
            _ => 1,
        },
        Arch::Mcs251 => match read(pc) {
            0x02 | 0x12 | 0x90 => 3,
            0x80 | 0x60 | 0x70 => 2,
            n if (0x74..=0x7F).contains(&n) => 3,
            _ => 1,
        },
        Arch::C166 => match read(pc) {
            n if (0x20..=0x2F).contains(&n) => 3,
            0xC0 => 3,
            _ => 2,
        },
    }
}

// ── Stateful debug (DAP) sessions ────────────────────────────────────────────

/// A live DAP debug session. `DebugHandle` keeps the adapter process and its
/// command/message channels alive across MCP calls.
struct DebugSession {
    handle: ns_debug::DebugHandle,
    name: String,
    program: PathBuf,
    breakpoints: Vec<DebugBreakpoint>,
    /// Messages received while waiting for a specific response; re-surfaced by
    /// `debug_status` so no event is silently dropped.
    pending: VecDeque<DebugMessage>,
    last_status: String,
    last_thread: u64,
    output_log: String,
}

impl DebugSession {
    fn note_message(&mut self, message: &DebugMessage) {
        match message {
            DebugMessage::Ready => self.last_status = "ready".to_string(),
            DebugMessage::Stopped {
                reason, thread_id, ..
            } => {
                self.last_status = format!("stopped: {reason}");
                self.last_thread = *thread_id;
            }
            DebugMessage::Continued => self.last_status = "continued".to_string(),
            DebugMessage::Terminated => self.last_status = "terminated".to_string(),
            DebugMessage::Error(error) => self.last_status = format!("error: {error}"),
            DebugMessage::Output { text, .. } => {
                if self.output_log.len() < 256 * 1024 {
                    self.output_log.push_str(text);
                }
            }
            _ => {}
        }
    }
}

fn debug_sessions() -> &'static Mutex<Vec<DebugSession>> {
    static SESSIONS: OnceLock<Mutex<Vec<DebugSession>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(Vec::new()))
}

static DEBUG_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

fn with_debug_session<R>(
    session: &str,
    f: impl FnOnce(&mut DebugSession) -> Result<R>,
) -> Result<R> {
    let mut sessions = debug_sessions().lock().unwrap_or_else(|e| e.into_inner());
    let idx = sessions
        .iter()
        .position(|s| s.name == session)
        .ok_or_else(|| {
            anyhow::anyhow!("调试会话不存在: {session}（先调用 debug_session_start）")
        })?;
    f(&mut sessions[idx])
}

/// Wait until a message matching `predicate` arrives, buffering unrelated
/// messages so `debug_status` can still report them. Returns the match.
fn debug_recv_until(
    session: &mut DebugSession,
    predicate: impl Fn(&DebugMessage) -> bool,
    timeout: Duration,
    what: &str,
) -> Result<DebugMessage> {
    if let Some(pos) = session.pending.iter().position(&predicate) {
        let message = session.pending.remove(pos).unwrap();
        session.note_message(&message);
        return Ok(message);
    }
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("等待 {what} 超时（调试会话无响应）");
        }
        match session
            .handle
            .recv_timeout(remaining.min(Duration::from_millis(500)))
        {
            Ok(message) => {
                if predicate(&message) {
                    session.note_message(&message);
                    return Ok(message);
                }
                session.note_message(&message);
                session.pending.push_back(message);
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                bail!("调试会话已断开（等待 {what}）")
            }
        }
    }
}

/// Scope reference for a frame: send `GetScopes`, pick the scope whose name
/// contains `name_substring` (case-insensitive), default to the first scope.
fn debug_scope_reference(
    session: &mut DebugSession,
    frame_id: u64,
    name_substring: &str,
) -> Result<(u64, String, Vec<ns_debug::Scope>)> {
    session
        .handle
        .send(DebugCommand::GetScopes { frame_id })
        .context("发送 GetScopes 失败")?;
    let message = debug_recv_until(
        session,
        |m| matches!(m, DebugMessage::Scopes { .. }),
        Duration::from_secs(5),
        "作用域列表",
    )?;
    let DebugMessage::Scopes { scopes, .. } = message else {
        unreachable!()
    };
    let needle = name_substring.to_ascii_lowercase();
    let scope = scopes
        .iter()
        .find(|s| s.name.to_ascii_lowercase().contains(&needle))
        .or_else(|| scopes.first())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("帧没有作用域"))?;
    Ok((scope.id, scope.name, scopes))
}

fn frame_for_thread(
    session: &mut DebugSession,
    thread_id: u64,
    index: usize,
) -> Result<ns_debug::Frame> {
    session
        .handle
        .send(DebugCommand::GetStackTrace { thread_id })
        .context("发送 GetStackTrace 失败")?;
    let message = debug_recv_until(
        session,
        |m| matches!(m, DebugMessage::StackTrace { .. }),
        Duration::from_secs(5),
        "调用栈",
    )?;
    let DebugMessage::StackTrace { frames } = message else {
        unreachable!()
    };
    frames
        .get(index)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("帧索引 {index} 越界（共 {} 帧）", frames.len()))
}

fn arg_str<'a>(args: &'a HashMap<String, serde_json::Value>, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required argument: {}", key))
}

fn arg_str_opt(args: &HashMap<String, serde_json::Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Parse an argument as usize, accepting either a JSON string or number.
fn arg_usize(args: &HashMap<String, serde_json::Value>, key: &str) -> Result<usize> {
    let v = args
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("missing required argument: {}", key))?;
    if let Some(s) = v.as_str() {
        return s
            .parse::<usize>()
            .with_context(|| format!("{} must be a positive integer, got: {}", key, s));
    }
    let n = v
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("{} must be a positive integer", key))?;
    Ok(n as usize)
}

/// Parse an argument as u32, accepting either a JSON string or number.
fn arg_u32(args: &HashMap<String, serde_json::Value>, key: &str) -> Result<u32> {
    let v = args
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("missing required argument: {}", key))?;
    if let Some(s) = v.as_str() {
        return s
            .trim()
            .parse::<u32>()
            .with_context(|| format!("{} must be a 32-bit integer, got: {}", key, s));
    }
    let n = v
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("{} must be a 32-bit integer", key))?;
    if n > u32::MAX as u64 {
        bail!("{} out of 32-bit range: {}", key, n);
    }
    Ok(n as u32)
}

/// Parse an argument as u64, accepting either a JSON string or number.
fn arg_u64(args: &HashMap<String, serde_json::Value>, key: &str) -> Result<u64> {
    let v = args
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("missing required argument: {}", key))?;
    if let Some(s) = v.as_str() {
        return s
            .trim()
            .parse::<u64>()
            .with_context(|| format!("{} must be a 64-bit integer, got: {}", key, s));
    }
    v.as_u64()
        .ok_or_else(|| anyhow::anyhow!("{} must be a 64-bit integer", key))
}

/// Resolve a possibly project-relative path against `root`.
fn resolve_path(root: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

fn open_project_in(root: &Path) -> Result<Project> {
    Project::open(root).with_context(|| format!("无法打开项目: {}", root.display()))
}

fn project_json(project: &Project) -> serde_json::Value {
    json!({
        "root": project.root.display().to_string(),
        "name": project.config.project.name,
        "version": project.config.project.version,
        "language": project.config.project.language.as_str(),
        "target": project.config.project.target,
        "compiler": project.config.project.compiler,
        "simulator": project.config.project.simulator,
        "debugger": project.config.project.debugger,
        "flasher": project.config.project.flasher,
        "build_config": project.build_config(),
        "build_jobs": project.build_jobs(),
        "artifact_path": project.artifact_path().display().to_string(),
    })
}

// ── Project tools ────────────────────────────────────────────────────────────

fn project_info(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    Ok(project_json(&project))
}

fn list_devices() -> serde_json::Value {
    json!({
        "devices": CATALOG.iter().map(|d| json!({
            "key": d.key,
            "label": d.label,
            "core": d.core,
            "flash_start": d.flash_start,
            "flash_size": d.flash_size,
            "ram_start": d.ram_start,
            "ram_size": d.ram_size,
        })).collect::<Vec<_>>(),
    })
}

fn open_project(path: &str) -> Result<serde_json::Value> {
    let project = open_project_in(Path::new(path))?;
    let _ = ns_project::remember_project(&project.root, &project.config.project.name);
    Ok(json!({
        "opened": true,
        "project": project_json(&project),
    }))
}

fn create_project(args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    let name = arg_str(args, "name")?.to_string();
    let location = PathBuf::from(arg_str(args, "location")?);
    let language_text = arg_str(args, "language")?;
    let language: Language = language_text.parse().map_err(|_| {
        anyhow::anyhow!(
            "不支持的语言: {}（可选: C, C++, Rust, Zig, ZR）",
            language_text
        )
    })?;
    let template = match arg_str_opt(args, "template").as_deref() {
        Some("led") | Some("led_blink") => Some(CTemplate::LedBlink),
        Some("uart") | Some("uart_loopback") => Some(CTemplate::UartLoopback),
        Some("freertos") | Some("rtos") => Some(CTemplate::Freertos),
        _ => Some(CTemplate::Empty),
    };
    let spec = NewProjectSpec {
        name,
        location,
        language,
        compiler: arg_str_opt(args, "compiler").unwrap_or_default(),
        simulator: arg_str_opt(args, "simulator").unwrap_or_default(),
        debugger: arg_str_opt(args, "debugger").unwrap_or_default(),
        flasher: arg_str_opt(args, "flasher").unwrap_or_default(),
        target: arg_str_opt(args, "target").unwrap_or_default(),
        svd: arg_str_opt(args, "target").unwrap_or_default(),
        c_template: if language == Language::C {
            template
        } else {
            None
        },
    };
    let project = Project::create(spec)?;
    let _ = ns_project::remember_project(&project.root, &project.config.project.name);
    Ok(json!({
        "created": true,
        "project": project_json(&project),
    }))
}

fn read_project_toml(root: &Path) -> Result<serde_json::Value> {
    let path = root.join("project.toml");
    let text =
        fs::read_to_string(&path).with_context(|| format!("读取 {} 失败", path.display()))?;
    Ok(json!({
        "path": path.display().to_string(),
        "content": text,
    }))
}

fn write_project_field(root: &Path, field: &str, value: &str) -> Result<serde_json::Value> {
    let path = root.join("project.toml");
    let text =
        fs::read_to_string(&path).with_context(|| format!("读取 {} 失败", path.display()))?;
    let mut doc: toml::Value =
        toml::from_str(&text).with_context(|| format!("解析 {} 失败", path.display()))?;
    let keys: Vec<&str> = field.split('.').filter(|k| !k.is_empty()).collect();
    if keys.is_empty() {
        bail!("字段路径不能为空");
    }
    // Walk every intermediate key, creating missing tables as we go. Each
    // iteration borrows a fresh mutable reference, which NLL releases before
    // the next one, so the borrow checker accepts the nested walk.
    let last = *keys.last().unwrap();
    let mut current = &mut doc;
    for key in &keys[..keys.len() - 1] {
        current = table_or_insert(current, key)?;
    }
    if !current.is_table() {
        bail!("字段 {} 不是表", keys[..keys.len() - 1].join("."));
    }
    let table = current.as_table_mut().unwrap();
    table.insert(last.to_string(), parse_toml_scalar(value));
    let serialized = toml::to_string_pretty(&doc).context("序列化 project.toml 失败")?;
    fs::write(&path, serialized).with_context(|| format!("写入 {} 失败", path.display()))?;
    Ok(json!({
        "path": field,
        "value": value,
        "saved": true,
    }))
}

/// Get the `key` child of `value` as a mutable table, creating it when absent.
fn table_or_insert<'a>(value: &'a mut toml::Value, key: &str) -> Result<&'a mut toml::Value> {
    let table = value
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("字段 {key} 不是表"))?;
    let entry = table
        .entry(key.to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    if !entry.is_table() {
        bail!("字段 {key} 不是表");
    }
    Ok(entry)
}

fn parse_toml_scalar(value: &str) -> toml::Value {
    let trimmed = value.trim();
    if let Ok(v) = trimmed.parse::<i64>() {
        return toml::Value::Integer(v);
    }
    if let Ok(v) = trimmed.parse::<bool>() {
        return toml::Value::Boolean(v);
    }
    toml::Value::String(value.to_string())
}

fn toggle_build_config(root: &Path) -> Result<serde_json::Value> {
    let mut project = open_project_in(root)?;
    let current = project.build_config().to_string();
    let next = if current == "debug" {
        "release"
    } else {
        "debug"
    };
    project.config.build.config = next.to_string();
    project.save_config()?;
    Ok(json!({
        "previous": current,
        "current": next,
        "saved": true,
    }))
}

fn get_project_issues(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let mut issues: Vec<serde_json::Value> = project
        .validate()
        .iter()
        .map(|i| json!({"severity": "error", "field": i.field, "message": i.message}))
        .collect();
    for warning in project.tool_warnings() {
        issues.push(
            json!({"severity": "warning", "field": warning.field, "message": warning.message}),
        );
    }
    Ok(json!({"issues": issues}))
}

fn compute_artifact_path(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let artifact = project.artifact_path();
    Ok(json!({
        "path": artifact.display().to_string(),
        "exists": artifact.exists(),
    }))
}

// ── File tools ───────────────────────────────────────────────────────────────

fn read_file(root: &Path, path: &str) -> Result<serde_json::Value> {
    let resolved = resolve_path(root, path);
    let text = fs::read_to_string(&resolved)
        .with_context(|| format!("读取 {} 失败", resolved.display()))?;
    let lines = text.lines().count();
    Ok(json!({
        "path": resolved.display().to_string(),
        "content": text,
        "lines": lines,
    }))
}

fn write_file(root: &Path, path: &str, content: &str) -> Result<serde_json::Value> {
    let resolved = resolve_path(root, path);
    if let Some(parent) = resolved.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建目录 {} 失败", parent.display()))?;
    }
    fs::write(&resolved, content).with_context(|| format!("写入 {} 失败", resolved.display()))?;
    Ok(json!({
        "path": resolved.display().to_string(),
        "bytes": content.len(),
        "written": true,
    }))
}

fn search_files(root: &Path, pattern: &str) -> Result<serde_json::Value> {
    let lower = pattern.to_lowercase();
    let mut matches = Vec::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != ".git" && name != "target" && name != "node_modules" && name != "build"
        })
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .display()
            .to_string();
        if pattern.is_empty() || name.contains(&lower) || rel.to_lowercase().contains(&lower) {
            matches.push(json!({
                "path": rel,
                "name": entry.file_name().to_string_lossy(),
            }));
            if matches.len() >= 200 {
                break;
            }
        }
    }
    Ok(json!({
        "pattern": pattern,
        "matches": matches,
        "count": matches.len(),
    }))
}

/// Read a 1-based line range of a text file, clamping to the file length.
/// Useful for large files that would be wasteful to read whole.
fn read_file_lines(
    root: &Path,
    path: &str,
    start: usize,
    count: usize,
) -> Result<serde_json::Value> {
    let resolved = resolve_path(root, path);
    let text = fs::read_to_string(&resolved)
        .with_context(|| format!("读取 {} 失败", resolved.display()))?;
    let all: Vec<&str> = text.lines().collect();
    let total = all.len();
    let start = start.max(1);
    let begin = start - 1;
    if begin >= total {
        return Ok(json!({
            "path": resolved.display().to_string(),
            "start": start,
            "lines": [],
            "count": 0,
            "total_lines": total,
            "truncated": false,
        }));
    }
    let end = (begin + count.max(1)).min(total);
    let lines: Vec<String> = all[begin..end].iter().map(|s| s.to_string()).collect();
    Ok(json!({
        "path": resolved.display().to_string(),
        "start": start,
        "lines": lines,
        "count": end - begin,
        "total_lines": total,
        "truncated": end < total,
    }))
}

const TEXT_MAX_FILE_SIZE: u64 = 1_048_576;
const TEXT_SKIP_DIRS: &[&str] = &[".git", "build", "target", "node_modules", ".ns-ide"];
const TEXT_BINARY_EXTENSIONS: &[&str] = &[
    "elf", "axf", "hex", "bin", "o", "a", "d", "rlib", "dylib", "so", "exe", "pdb",
];

/// Replace `from` with `to` across project text files. Returns per-file
/// replacement counts and total statistics. With `dry_run`, reports what
/// *would* change without writing to disk.
fn replace_in_files_tool(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let from = arg_str(args, "from")?;
    let to = arg_str(args, "to")?;
    if from.trim().is_empty() {
        bail!("from 不能为空");
    }
    let case_sensitive = args
        .get("case_sensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let whole_word = args
        .get("whole_word")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let path_filter = arg_str_opt(args, "path");
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut files = Vec::new();
    let mut files_changed = 0usize;
    let mut occurrences = 0usize;
    replace_walk_mcp(
        root,
        root,
        from,
        to,
        case_sensitive,
        whole_word,
        path_filter.as_deref(),
        dry_run,
        &mut files,
        &mut files_changed,
        &mut occurrences,
    );
    Ok(json!({
        "from": from,
        "to": to,
        "case_sensitive": case_sensitive,
        "whole_word": whole_word,
        "dry_run": dry_run,
        "files_changed": files_changed,
        "occurrences": occurrences,
        "files": files,
    }))
}

#[allow(clippy::too_many_arguments)]
fn replace_walk_mcp(
    dir: &Path,
    root: &Path,
    from: &str,
    to: &str,
    case_sensitive: bool,
    whole_word: bool,
    path_filter: Option<&str>,
    dry_run: bool,
    files: &mut Vec<serde_json::Value>,
    files_changed: &mut usize,
    occurrences: &mut usize,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    children.sort();
    for path in children {
        if path.is_dir() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.starts_with('.') || TEXT_SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            replace_walk_mcp(
                &path,
                root,
                from,
                to,
                case_sensitive,
                whole_word,
                path_filter,
                dry_run,
                files,
                files_changed,
                occurrences,
            );
            continue;
        }
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if TEXT_BINARY_EXTENSIONS
                .iter()
                .any(|b| ext.eq_ignore_ascii_case(b))
            {
                continue;
            }
        }
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > TEXT_MAX_FILE_SIZE {
            continue;
        }
        let relative = relative_path_str(root, &path);
        if let Some(filter) = path_filter {
            if !relative.to_lowercase().contains(&filter.to_lowercase()) {
                continue;
            }
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let (replaced, count) = replace_occurrences(&text, from, to, case_sensitive, whole_word);
        if count == 0 {
            continue;
        }
        if !dry_run && fs::write(&path, &replaced).is_err() {
            continue;
        }
        *files_changed += 1;
        *occurrences += count;
        files.push(json!({"path": relative, "replacements": count}));
    }
}

/// Replace every occurrence of `from` with `to`. Case folding is ASCII-only so
/// byte indices stay valid for non-ASCII source text. Whole-word mode requires
/// non-word bytes (or line edges) on both sides of each match.
fn replace_occurrences(
    text: &str,
    from: &str,
    to: &str,
    case_sensitive: bool,
    whole_word: bool,
) -> (String, usize) {
    let needle = from.trim();
    if needle.is_empty() {
        return (text.to_string(), 0);
    }
    let text_bytes = text.as_bytes();
    let needle_bytes = needle.as_bytes();
    let hay: Vec<u8> = if case_sensitive {
        text_bytes.to_vec()
    } else {
        text_bytes.iter().map(|b| b.to_ascii_lowercase()).collect()
    };
    let ndl: Vec<u8> = if case_sensitive {
        needle_bytes.to_vec()
    } else {
        needle_bytes
            .iter()
            .map(|b| b.to_ascii_lowercase())
            .collect()
    };
    let mut result = String::with_capacity(text.len());
    let mut count = 0usize;
    let mut search_from = 0usize;
    let mut copied_until = 0usize;
    while search_from + ndl.len() <= hay.len() {
        let Some(relative) = hay[search_from..].windows(ndl.len()).position(|w| w == ndl) else {
            break;
        };
        let i = search_from + relative;
        let before_ok = i == 0 || !is_word_byte(hay[i - 1]);
        let after = i + ndl.len();
        let after_ok = after >= hay.len() || !is_word_byte(hay[after]);
        if !whole_word || (before_ok && after_ok) {
            result.push_str(&text[copied_until..i]);
            result.push_str(to);
            count += 1;
            copied_until = i + needle_bytes.len();
            search_from = i + needle_bytes.len();
        } else {
            search_from = i + 1;
        }
    }
    result.push_str(&text[copied_until..]);
    (result, count)
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn relative_path_str(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Compute a unified line diff between two text blobs.
fn diff_text_tool(args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    let left = arg_str(args, "left")?;
    let right = arg_str(args, "right")?;
    let left_label = arg_str_opt(args, "left_label").unwrap_or_else(|| "left".to_string());
    let right_label = arg_str_opt(args, "right_label").unwrap_or_else(|| "right".to_string());
    let a: Vec<&str> = left.lines().collect();
    let b: Vec<&str> = right.lines().collect();
    let ops = line_ops(&a, &b)?;
    let (diff, added, removed, changed, hunks) =
        render_unified_diff(&a, &b, &ops, &left_label, &right_label);
    Ok(json!({
        "left_label": left_label,
        "right_label": right_label,
        "left_lines": a.len(),
        "right_lines": b.len(),
        "added": added,
        "removed": removed,
        "changed": changed,
        "hunks": hunks,
        "identical": added == 0 && removed == 0,
        "diff": diff,
    }))
}

enum DiffOp {
    Equal(usize),
    Delete(usize),
    Insert(usize),
}

/// LCS line differ. Returns an ordered operation list. Guards against
/// quadratic blow-up on pathological large inputs.
fn line_ops(a: &[&str], b: &[&str]) -> Result<Vec<DiffOp>> {
    let n = a.len();
    let m = b.len();
    if n.saturating_mul(m) > 4_000_000 {
        bail!("diff 输入过大（{}x{} 行），请分块比较", n, m);
    }
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(DiffOp::Equal(i));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(DiffOp::Delete(i));
            i += 1;
        } else {
            ops.push(DiffOp::Insert(j));
            j += 1;
        }
    }
    while i < n {
        ops.push(DiffOp::Delete(i));
        i += 1;
    }
    while j < m {
        ops.push(DiffOp::Insert(j));
        j += 1;
    }
    Ok(ops)
}

/// Render operations as a unified diff with 3 lines of context.
fn render_unified_diff(
    a: &[&str],
    b: &[&str],
    ops: &[DiffOp],
    left_label: &str,
    right_label: &str,
) -> (Vec<String>, usize, usize, usize, usize) {
    const CONTEXT: usize = 3;
    let mut out = vec![format!("--- {left_label}"), format!("+++ {right_label}")];
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut changed = 0usize;

    // Locate change blocks (maximal runs of non-equal ops).
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    let mut k = 0usize;
    while k < ops.len() {
        if !matches!(ops[k], DiffOp::Equal(_)) {
            let start = k;
            while k < ops.len() && !matches!(ops[k], DiffOp::Equal(_)) {
                k += 1;
            }
            blocks.push((start, k - 1));
            changed += 1;
        } else {
            k += 1;
        }
    }
    if blocks.is_empty() {
        return (out, 0, 0, 0, 0);
    }

    // Expand each block with context and merge overlaps.
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for (start, end) in blocks {
        let hs = start.saturating_sub(CONTEXT);
        let he = (end + CONTEXT).min(ops.len() - 1);
        if let Some(last) = hunks.last_mut() {
            if hs <= last.1 + 1 {
                last.1 = last.1.max(he);
                continue;
            }
        }
        hunks.push((hs, he));
    }

    // Track a/b line counters as we walk ops. `op_pos` is how far into `ops`
    // we have already reflected in a_cur/b_cur.
    let mut a_cur = 0usize;
    let mut b_cur = 0usize;
    let mut op_pos = 0usize;
    for (hs, he) in &hunks {
        // Advance through any context gap before this hunk.
        while op_pos < *hs {
            match &ops[op_pos] {
                DiffOp::Equal(_) => {
                    a_cur += 1;
                    b_cur += 1;
                }
                DiffOp::Delete(_) => a_cur += 1,
                DiffOp::Insert(_) => b_cur += 1,
            }
            op_pos += 1;
        }
        let na = ops[*hs..=*he]
            .iter()
            .filter(|op| matches!(op, DiffOp::Equal(_) | DiffOp::Delete(_)))
            .count();
        let nb = ops[*hs..=*he]
            .iter()
            .filter(|op| matches!(op, DiffOp::Equal(_) | DiffOp::Insert(_)))
            .count();
        let a_start = if na == 0 { a_cur } else { a_cur + 1 };
        let b_start = if nb == 0 { b_cur } else { b_cur + 1 };
        out.push(format!("@@ -{a_start},{na} +{b_start},{nb} @@"));
        for op in &ops[*hs..=*he] {
            match op {
                DiffOp::Equal(ai) => {
                    out.push(format!(" {}", a[*ai]));
                    a_cur += 1;
                    b_cur += 1;
                }
                DiffOp::Delete(ai) => {
                    out.push(format!("-{}", a[*ai]));
                    a_cur += 1;
                    removed += 1;
                }
                DiffOp::Insert(bj) => {
                    out.push(format!("+{}", b[*bj]));
                    b_cur += 1;
                    added += 1;
                }
            }
            op_pos += 1;
        }
    }
    (out, added, removed, changed, hunks.len())
}

fn list_files(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let files = project.source_files()?;
    Ok(json!({
        "files": files.iter().map(|p| {
            let rel = p.strip_prefix(&project.root).unwrap_or(p);
            rel.display().to_string()
        }).collect::<Vec<_>>(),
        "count": files.len(),
    }))
}

fn list_open_files(root: &Path) -> Result<serde_json::Value> {
    let session = Session::load(root);
    let open = session
        .open_files
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>();
    Ok(json!({
        "files": open,
        "active": session.active,
        "count": session.open_files.len(),
    }))
}

/// Read the IDE editor session (open tabs, active index, per-file view state).
fn get_session(root: &Path) -> Result<serde_json::Value> {
    let session = Session::load(root);
    let views = session
        .views
        .iter()
        .map(|view| {
            let cursor = match view.cursor {
                Some((line, column)) => json!([line, column]),
                None => json!(null),
            };
            json!({
                "path": view.path.display().to_string(),
                "cursor": cursor,
                "scroll_ratio": view.scroll_ratio,
                "zoom": view.zoom,
                "indent_width": view.indent_width,
                "word_wrap": view.word_wrap,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "open_files": session.open_files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "active": session.active,
        "count": session.open_files.len(),
        "views": views,
        "split": session.split.as_ref().map(|p| p.display().to_string()),
        "saved_at": session.saved_at,
    }))
}

/// Set and persist the IDE editor session. `open_files`/`active`/`views`/
/// `split` are optional; omitted fields keep their current (loaded) value.
fn save_session(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let mut session = Session::load(root);
    if let Some(files) = args.get("open_files").and_then(|v| v.as_array()) {
        session.open_files = files
            .iter()
            .filter_map(|v| v.as_str())
            .map(PathBuf::from)
            .collect();
    }
    if let Some(active) = args.get("active") {
        session.active = active.as_u64().map(|n| n as usize);
    }
    if let Some(views) = args.get("views").and_then(|v| v.as_array()) {
        session.views = views
            .iter()
            .filter_map(|view| {
                let path = view.get("path").and_then(|p| p.as_str())?;
                let cursor = view.get("cursor").and_then(|c| c.as_array()).map(|c| {
                    (
                        c.get(0).and_then(|x| x.as_u64()).unwrap_or(1) as usize,
                        c.get(1).and_then(|x| x.as_u64()).unwrap_or(1) as usize,
                    )
                });
                Some(ns_project::FileView {
                    path: PathBuf::from(path),
                    cursor,
                    scroll_ratio: view
                        .get("scroll_ratio")
                        .and_then(|x| x.as_f64())
                        .unwrap_or(0.0) as f32,
                    zoom: view.get("zoom").and_then(|x| x.as_f64()).unwrap_or(1.0) as f32,
                    indent_width: view
                        .get("indent_width")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(4) as usize,
                    word_wrap: view
                        .get("word_wrap")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false),
                })
            })
            .collect();
    }
    if let Some(split) = args.get("split").and_then(|v| v.as_str()) {
        if split.is_empty() {
            session.split = None;
        } else {
            session.split = Some(PathBuf::from(split));
        }
    }
    session.save(root)?;
    Ok(json!({
        "count": session.open_files.len(),
        "active": session.active,
        "saved": true,
    }))
}

fn list_build_outputs(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let out = project.build_dir();
    if !out.exists() {
        return Ok(json!({"files": [], "count": 0, "dir": out.display().to_string()}));
    }
    let mut files = Vec::new();
    for entry in WalkDir::new(&out).follow_links(false).into_iter().flatten() {
        if entry.file_type().is_file() {
            let rel = entry.path().strip_prefix(&out).unwrap_or(entry.path());
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            files.push(json!({
                "path": rel.display().to_string(),
                "size": size,
            }));
        }
    }
    files.sort_by_key(|f| f["path"].as_str().unwrap_or("").to_string());
    Ok(json!({
        "dir": out.display().to_string(),
        "files": files,
        "count": files.len(),
    }))
}

fn read_binary(root: &Path, path: &str, offset: usize, count: usize) -> Result<serde_json::Value> {
    let resolved = resolve_path(root, path);
    let bytes = fs::read(&resolved).with_context(|| format!("读取 {} 失败", resolved.display()))?;
    let end = (offset.saturating_add(count)).min(bytes.len());
    let slice = if offset < bytes.len() {
        &bytes[offset..end]
    } else {
        &[][..]
    };
    let hex = slice
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    let ascii: String = slice
        .iter()
        .map(|b| {
            if b.is_ascii_graphic() || *b == b' ' {
                *b as char
            } else {
                '.'
            }
        })
        .collect();
    Ok(json!({
        "path": resolved.display().to_string(),
        "total": bytes.len(),
        "offset": offset,
        "count": slice.len(),
        "hex": hex,
        "ascii": ascii,
    }))
}

// ── Build tools ──────────────────────────────────────────────────────────────

fn build(root: &Path, args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    let mut project = open_project_in(root)?;
    if let Some(config) = args.get("config").and_then(|v| v.as_str()) {
        if config == "debug" || config == "release" {
            project.config.build.config = config.to_string();
        }
    }
    let plan = ns_build::plan_build(&project)?;
    let outcome = ns_build::execute(&plan);
    let diagnostics = outcome.diagnostics(&project.root);
    Ok(json!({
        "success": outcome.success,
        "code": outcome.code,
        "command": plan.command_line(),
        "output": outcome.combined_output(),
        "duration_ms": outcome.duration.as_millis(),
        "diagnostics": diagnostics.iter().map(diagnostic_json).collect::<Vec<_>>(),
        "artifact": project.artifact_path().display().to_string(),
    }))
}

fn clean(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let removed = ns_build::clean_build(&project)?;
    Ok(json!({
        "cleaned": removed.is_some(),
        "dir": removed.map(|p| p.display().to_string()),
    }))
}

fn run_tests(root: &Path, _args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let Some(plan) = ns_build::plan_tests(&project) else {
        bail!("当前项目不支持单测（无对应测试计划）");
    };
    let outcome = ns_build::execute(&plan);
    Ok(json!({
        "success": outcome.success,
        "code": outcome.code,
        "command": plan.command_line(),
        "output": outcome.combined_output(),
        "duration_ms": outcome.duration.as_millis(),
    }))
}

fn run_static_analysis(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let Some(plan) = ns_build::plan_static_analysis(&project) else {
        bail!("当前项目不支持静态分析（无对应分析计划）");
    };
    let outcome = ns_build::execute(&plan);
    let diagnostics = outcome.diagnostics(&project.root);
    Ok(json!({
        "success": outcome.success,
        "code": outcome.code,
        "command": plan.command_line(),
        "output": outcome.combined_output(),
        "diagnostics": diagnostics.iter().map(diagnostic_json).collect::<Vec<_>>(),
    }))
}

fn preprocess_source(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let Some(plan) = ns_build::plan_preprocess(&project) else {
        bail!("当前项目不支持预处理（C/C++ 需要编译器支持 -E -P）");
    };
    let outcome = ns_build::execute(&plan);
    Ok(json!({
        "success": outcome.success,
        "code": outcome.code,
        "command": plan.command_line(),
        "output": outcome.combined_output(),
    }))
}

fn generate_compile_commands(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let count = ns_build::generate_compile_commands(&project)?;
    Ok(json!({
        "generated": true,
        "entries": count,
        "path": project.root.join("compile_commands.json").display().to_string(),
    }))
}

fn read_stack_usage(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let out = project.build_dir();
    let entries = ns_build::collect_stack_usage(&out);
    Ok(json!({
        "entries": entries.iter().map(stack_entry_json).collect::<Vec<_>>(),
        "count": entries.len(),
    }))
}

fn run_stack_usage(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let Some(plan) = ns_build::plan_stack_usage(&project) else {
        bail!("当前项目不支持栈使用分析（仅 C/C++ 工程，且需要可用编译器）");
    };
    let outcome = ns_build::execute(&plan);
    let entries = ns_build::collect_stack_usage(&plan.artifact);
    Ok(json!({
        "success": outcome.success,
        "code": outcome.code,
        "command": plan.command_line(),
        "output": outcome.combined_output(),
        "count": entries.len(),
        "entries": entries.iter().map(stack_entry_json).collect::<Vec<_>>(),
    }))
}

fn run_coverage(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let Some(plan) = ns_build::plan_coverage(&project) else {
        bail!("当前项目不支持覆盖率分析（仅 C/C++ 工程，且需要可用编译器）");
    };
    let build = ns_build::execute(&plan);
    let out_dir = plan
        .artifact
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| project.build_dir());
    let files =
        ns_build::run_coverage(&plan.artifact, &project.root, &out_dir)?.unwrap_or_default();
    Ok(json!({
        "success": build.success,
        "code": build.code,
        "command": plan.command_line(),
        "build_output": build.combined_output(),
        "count": files.len(),
        "files": files.iter().map(coverage_file_json).collect::<Vec<_>>(),
    }))
}

fn scan_tasks(root: &Path, keyword: Option<&str>) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let files = ns_project::tasks::collect_source_files(&project.root);
    let markers = ns_project::tasks::scan_tasks(&files);
    let keyword = keyword.unwrap_or("").trim().to_ascii_uppercase();
    let tasks = markers
        .iter()
        .filter(|marker| keyword.is_empty() || marker.keyword == keyword)
        .map(|marker| {
            json!({
                "path": marker.path.display().to_string(),
                "line": marker.line,
                "column": marker.column,
                "keyword": marker.keyword,
                "text": marker.text,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({"count": tasks.len(), "tasks": tasks}))
}

fn memory_layout(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let linker = project.root.join("link.ld");
    if !linker.is_file() {
        bail!("项目没有 link.ld（内存布局需要链接脚本的 MEMORY 区域）");
    }
    let text = fs::read_to_string(&linker).with_context(|| "读取 link.ld 失败")?;
    let mut regions = ns_build::layout::parse_linker_memory(&text);
    if regions.is_empty() {
        bail!("link.ld 中没有可解析的 MEMORY 区域");
    }
    let artifact = project.artifact_path();
    let symbols = if artifact.is_file() {
        ns_flash::firmware::analyze_symbols(&artifact)
    } else {
        Vec::new()
    };
    let sections = symbols
        .iter()
        .map(|symbol| ns_build::layout::SectionUsage {
            name: symbol.name.clone(),
            address: symbol.address,
            size: symbol.size,
        })
        .collect::<Vec<_>>();
    ns_build::layout::assign_sections(&mut regions, &sections);
    let used_total = regions.iter().map(|region| region.used).sum::<u64>();
    Ok(json!({
        "used_total": used_total,
        "regions": regions.iter().map(|region| json!({
            "name": region.name,
            "attributes": region.attributes,
            "origin": region.origin,
            "length": region.length,
            "used": region.used,
            "percent": if region.length == 0 { 0 } else { region.used * 100 / region.length },
        })).collect::<Vec<_>>(),
        "sections": sections.iter().map(|section| json!({
            "name": section.name,
            "address": section.address,
            "size": section.size,
        })).collect::<Vec<_>>(),
    }))
}

fn include_dependencies(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let sources = project.source_files()?;
    let graph = ns_project::depgraph::build_include_graph(
        &project.root,
        &sources,
        &project.config.compiler.includes,
    );
    let (dag_nodes, edges, columns) = ns_project::depgraph::dag_layout(&graph);
    let node_list = graph
        .nodes
        .values()
        .map(|node| {
            json!({
                "path": node.path,
                "deps": node.deps,
                "dependents": node.dependents,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "count": node_list.len(),
        "nodes": node_list,
        "dag_nodes": dag_nodes.iter().map(|node| json!({
            "path": node.path,
            "column": node.column,
            "row": node.row,
        })).collect::<Vec<_>>(),
        "dag_edges": edges.iter().map(|edge| json!({
            "from": edge.from,
            "to": edge.to,
        })).collect::<Vec<_>>(),
        "columns": columns,
    }))
}

fn coverage_file_json(file: &ns_build::CoverageFile) -> serde_json::Value {
    json!({
        "file": file.file,
        "executed": file.executed,
        "not_executed": file.not_executed,
        "percent": file.percent(),
        "line_hits": file.line_hits,
    })
}

fn diagnostic_json(d: &Diagnostic) -> serde_json::Value {
    json!({
        "severity": d.severity.label(),
        "path": d.path.as_ref().map(|p| p.display().to_string()),
        "line": d.line,
        "column": d.column,
        "message": d.message,
        "raw": d.raw,
        "location": d.location_label(),
    })
}

fn stack_entry_json(e: &StackUsageEntry) -> serde_json::Value {
    json!({
        "file": e.file,
        "function": e.function,
        "bytes": e.bytes,
        "dynamic": e.dynamic,
    })
}

fn get_diagnostics(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let plan = ns_build::plan_build(&project)?;
    let outcome = ns_build::execute(&plan);
    let diagnostics = outcome.diagnostics(&project.root);
    Ok(json!({
        "success": outcome.success,
        "diagnostics": diagnostics.iter().map(diagnostic_json).collect::<Vec<_>>(),
        "count": diagnostics.len(),
        "output": outcome.combined_output(),
    }))
}

fn suggest_quick_fix_tool(message: &str) -> Result<serde_json::Value> {
    let diagnostic = Diagnostic {
        path: None,
        line: None,
        column: None,
        severity: ns_build::Severity::Error,
        message: message.to_string(),
        raw: message.to_string(),
    };
    let fix = suggest_quick_fix(&diagnostic, None);
    Ok(json!({
        "message": message,
        "quick_fix": fix.as_ref().map(quick_fix_json),
    }))
}

fn quick_fix_json(fix: &QuickFix) -> serde_json::Value {
    match fix {
        QuickFix::AddInclude { header } => json!({"kind": "add_include", "header": header}),
        QuickFix::AddLinkFlag { flag } => json!({"kind": "add_link_flag", "flag": flag}),
        QuickFix::Hint(text) => json!({"kind": "hint", "text": text}),
    }
}

// ── Workspace / symbol tools ─────────────────────────────────────────────────

fn get_workspace_symbols(root: &Path) -> Result<serde_json::Value> {
    let project = open_project_in(root)?;
    let files = project.source_files()?;
    let mut symbols = Vec::new();
    for file in files {
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        let lang = Language::from_path(&file).unwrap_or(Language::C);
        for (idx, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') {
                continue;
            }
            if let Some(name) = extract_symbol(trimmed, lang) {
                symbols.push(json!({
                    "name": name,
                    "file": file.strip_prefix(&project.root).unwrap_or(&file).display().to_string(),
                    "line": idx + 1,
                    "language": lang.as_str(),
                }));
            }
        }
    }
    Ok(json!({
        "symbols": symbols,
        "count": symbols.len(),
    }))
}

fn extract_symbol(line: &str, lang: Language) -> Option<String> {
    let line = line.trim();
    match lang {
        Language::Rust => {
            if let Some(rest) = line.strip_prefix("pub ") {
                return extract_symbol(rest, lang);
            }
            for kw in [
                "fn", "struct", "enum", "trait", "const", "static", "type", "mod",
            ] {
                if let Some(rest) = line.strip_prefix(kw).map(str::trim_start) {
                    if let Some(name) = rest
                        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .next()
                    {
                        if !name.is_empty() {
                            return Some(name.to_string());
                        }
                    }
                }
            }
            None
        }
        Language::Zig | Language::ZR => {
            let stripped = line.strip_prefix("pub ").unwrap_or(line);
            for kw in ["fn", "const", "var", "struct", "enum", "union", "comptime"] {
                if let Some(rest) = stripped.strip_prefix(kw).map(str::trim_start) {
                    if let Some(name) = rest
                        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .next()
                    {
                        if !name.is_empty() {
                            return Some(name.to_string());
                        }
                    }
                }
            }
            None
        }
        Language::C | Language::Cpp | Language::Asm => {
            if let Some(open) = line.find('(') {
                if open == 0 {
                    return None;
                }
                let before = &line[..open];
                let tokens: Vec<&str> = before
                    .split_whitespace()
                    .filter(|t| !t.is_empty())
                    .collect();
                if let Some(name) = tokens.last().copied() {
                    let clean = name
                        .trim_end_matches(|c: char| c == '*' || c == '&')
                        .trim_end_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
                    if !clean.is_empty() && clean.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        return Some(clean.to_string());
                    }
                }
            }
            for kw in ["struct", "class", "enum", "union", "typedef"] {
                if let Some(rest) = line.strip_prefix(kw).map(str::trim_start) {
                    if let Some(name) = rest
                        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .find(|s| !s.is_empty())
                    {
                        return Some(name.to_string());
                    }
                }
            }
            None
        }
    }
}

// ── Device tools ─────────────────────────────────────────────────────────────

fn get_peripherals(target: &str) -> Result<serde_json::Value> {
    let device =
        device_for_target(target).ok_or_else(|| anyhow::anyhow!("未找到目标设备: {}", target))?;
    let peripherals = device
        .sorted_peripherals()
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "base_address": p.base_address,
                "description": p.description,
                "registers": p.registers.len(),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "target": target,
        "device": device.name,
        "peripherals": peripherals,
        "count": peripherals.len(),
    }))
}

fn get_device_info(target: &str) -> Result<serde_json::Value> {
    let device =
        device_for_target(target).ok_or_else(|| anyhow::anyhow!("未找到目标设备: {}", target))?;
    let memory = device
        .memory
        .iter()
        .map(|m| {
            json!({
                "name": m.name,
                "start": m.start,
                "size": m.size,
                "access": m.access,
            })
        })
        .collect::<Vec<_>>();
    let by_peripheral = registers_by_peripheral(&device);
    let peripherals = device
        .sorted_peripherals()
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "base_address": p.base_address,
                "registers": by_peripheral.get(&p.name).map(|r| r.len()).unwrap_or(0),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "target": target,
        "name": device.name,
        "vendor": device.vendor,
        "core": device.core,
        "memory": memory,
        "peripherals": peripherals,
    }))
}

fn get_register_details(target: &str, peripheral: &str) -> Result<serde_json::Value> {
    let device =
        device_for_target(target).ok_or_else(|| anyhow::anyhow!("未找到目标设备: {}", target))?;
    let p = device
        .peripheral(peripheral)
        .ok_or_else(|| anyhow::anyhow!("目标 {} 没有外设: {}", target, peripheral))?;
    let registers = p
        .registers
        .iter()
        .map(|r| {
            let fields = r
                .fields
                .iter()
                .map(|f| {
                    json!({
                        "name": f.name,
                        "bit_offset": f.bit_offset,
                        "bit_width": f.bit_width,
                        "description": f.description,
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "name": r.name,
                "address": r.address,
                "size": r.size,
                "reset_value": r.reset_value,
                "description": r.description,
                "fields": fields,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "target": target,
        "peripheral": p.name,
        "base_address": p.base_address,
        "description": p.description,
        "registers": registers,
    }))
}

fn get_memory_map(target: &str) -> Result<serde_json::Value> {
    let regions = memory_map(target);
    if regions.is_empty() {
        bail!("未找到目标设备的内存映射: {}", target);
    }
    Ok(json!({
        "target": target,
        "regions": regions.iter().map(|r| {
            json!({
                "name": r.name,
                "start": r.start,
                "size": r.size,
                "access": r.access,
            })
        }).collect::<Vec<_>>(),
    }))
}

fn generate_linker_script(target: &str) -> Result<serde_json::Value> {
    let script = linker_script(target)
        .ok_or_else(|| anyhow::anyhow!("未找到目标设备的链接脚本模板: {}", target))?;
    Ok(json!({
        "target": target,
        "content": script,
    }))
}

fn get_linker_script(target: &str) -> Result<serde_json::Value> {
    generate_linker_script(target)
}

fn get_startup_file(target: &str) -> Result<serde_json::Value> {
    let script = startup_file(target)
        .ok_or_else(|| anyhow::anyhow!("未找到目标设备的启动文件模板: {}", target))?;
    Ok(json!({
        "target": target,
        "content": script,
    }))
}

fn list_catalog_devices() -> Result<serde_json::Value> {
    Ok(json!({
        "devices": CATALOG.iter().map(|d| json!({
            "key": d.key,
            "label": d.label,
            "core": d.core,
            "flash_start": d.flash_start,
            "flash_size": d.flash_size,
            "ram_start": d.ram_start,
            "ram_size": d.ram_size,
        })).collect::<Vec<_>>(),
        "count": CATALOG.len(),
    }))
}

fn parse_svd_preview(path: &str) -> Result<serde_json::Value> {
    let text = fs::read_to_string(path).with_context(|| format!("读取 SVD {} 失败", path))?;
    let device = parse_svd(&text)?;
    Ok(json!({
        "path": path,
        "name": device.name,
        "vendor": device.vendor,
        "core": device.core,
        "peripherals": device.peripherals.len(),
        "registers": device
            .peripherals
            .iter()
            .map(|p| p.registers.len())
            .sum::<usize>(),
        "memory_regions": device.memory.len(),
    }))
}

fn generate_init_code(args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    let target = arg_str(args, "target")?;
    let peripherals = args
        .get("peripherals")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("缺少 peripherals 数组参数"))?;
    let mut configs = Vec::new();
    for p in peripherals {
        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let purpose = p.get("purpose").and_then(|v| v.as_str()).unwrap_or("");
        let mut register_values = BTreeMap::new();
        if let Some(values) = p.get("register_values").and_then(|v| v.as_object()) {
            for (reg, val) in values {
                if let Some(v) = val.as_u64() {
                    register_values.insert(reg.clone(), v);
                }
            }
        }
        configs.push(PeripheralConfig {
            name: name.to_string(),
            purpose: purpose.to_string(),
            options: Vec::new(),
            register_values,
        });
    }
    let code = generate_peripheral_init(target, &configs)
        .ok_or_else(|| anyhow::anyhow!("未找到目标设备或外设: {}", target))?;
    Ok(json!({
        "target": target,
        "code": code,
        "peripherals": configs.len(),
    }))
}

fn decode_number(value: &str) -> Result<serde_json::Value> {
    let trimmed = value.trim().replace('_', "");
    let parsed = if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(bin) = trimmed
        .strip_prefix("0b")
        .or_else(|| trimmed.strip_prefix("0B"))
    {
        u64::from_str_radix(bin, 2).ok()
    } else {
        trimmed
            .parse::<u64>()
            .ok()
            .or_else(|| trimmed.parse::<i64>().ok().map(|n| n as u64))
    };
    let n = parsed.ok_or_else(|| anyhow::anyhow!("无法解析数字: {}", value))?;
    Ok(json!({
        "decimal": n.to_string(),
        "hex": format!("0x{:x}", n),
        "hex_upper": format!("0x{:X}", n),
        "binary": format!("0b{:b}", n),
        "octal": format!("0o{:o}", n),
        "raw": value,
    }))
}

// ── Flash / simulation tools ─────────────────────────────────────────────────

fn flash_firmware(args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    let firmware_path = arg_str(args, "firmware_path")?;
    let project = open_project_in(&current_project_root())?;
    let firmware = load_firmware(Path::new(firmware_path))?;
    let adapter = ns_flash::adapter_for(&project)?;
    let request = FlashRequest {
        tool: adapter.tool(),
        action: FlashAction::Flash,
        firmware: PathBuf::from(firmware_path),
        interface: project.config.flasher.interface.clone(),
        read_to: None,
        device: Some(project.config.project.target.clone()),
        algorithm: None,
        cancel: None,
    };
    let mut output = String::new();
    let outcome = adapter.run(&request, &mut |line| output.push_str(line))?;
    Ok(json!({
        "success": outcome.success,
        "code": outcome.code,
        "tool": outcome.tool.label(),
        "action": outcome.action.label(),
        "output": outcome.combined_output(),
        "duration_ms": outcome.duration.as_millis(),
        "firmware": firmware.path.display().to_string(),
        "base": firmware.base,
        "bytes": firmware.bytes.len(),
    }))
}

/// Build a simulator from the project's artifact, mirroring the IDE load path.
fn simulator_from_project(project: &Project, firmware_override: Option<&str>) -> Result<Simulator> {
    let artifact = firmware_override
        .map(PathBuf::from)
        .unwrap_or_else(|| project.artifact_path());
    if !artifact.exists() {
        bail!("找不到产物: {}", artifact.display());
    }
    let bytes =
        fs::read(&artifact).with_context(|| format!("读取产物 {} 失败", artifact.display()))?;
    let target = project.config.project.target.trim().to_string();
    let arch = if !target.is_empty() {
        Arch::from_target(&target)
    } else if bytes.len() >= 20 && &bytes[0..4] == b"\x7fELF" {
        let little = bytes[5] == 1;
        let machine = read_u16_le(&bytes[18..20], little);
        Arch::from_elf_machine(machine).unwrap_or(Arch::RiscV)
    } else {
        Arch::RiscV
    };
    let mut core = Simulator::for_arch(arch)?;
    let is_hex_core = matches!(arch, Arch::Mcs51 | Arch::Mcs251 | Arch::C166 | Arch::Stm8);
    if is_hex_core {
        if bytes.starts_with(b":") {
            let text =
                std::str::from_utf8(&bytes).context("固件不是有效 UTF-8（Intel HEX 应为文本）")?;
            let decoded = decode_intel_hex(text)?;
            match arch {
                Arch::Mcs51 => core.load_mcs51(decoded.base as u16, &decoded.bytes)?,
                Arch::Mcs251 => core.load_x51(decoded.base as u32, &decoded.bytes)?,
                Arch::C166 => core.load_c166(decoded.base as u16, &decoded.bytes)?,
                Arch::Stm8 => core.load_stm8(decoded.base as u32, &decoded.bytes)?,
                _ => unreachable!(),
            }
        } else {
            match arch {
                Arch::Mcs51 => core.load_mcs51(0, &bytes)?,
                Arch::Mcs251 => core.load_x51(0, &bytes)?,
                Arch::C166 => core.load_c166(0, &bytes)?,
                Arch::Stm8 => core.load_stm8(0, &bytes)?,
                _ => unreachable!(),
            }
        }
    } else if bytes.len() >= 4 && &bytes[0..4] == b"\x7fELF" {
        core.load_elf(&bytes)?;
    } else {
        core.load_image(ns_sim::ENTRY_POINT, &bytes);
    }
    Ok(core)
}

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

// ── Stateful simulator tools ─────────────────────────────────────────────────

fn sim_session_start(args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    sim_session_start_in(&current_project_root(), args)
}

fn sim_session_start_in(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let firmware = arg_str_opt(args, "firmware");
    let project = open_project_in(root)?;
    let core = simulator_from_project(&project, firmware.as_deref())?;
    let arch = core.arch;
    let entry = core.program_counter();
    let pc = entry;
    let cycle = core.cycle_count;
    let regs = (0..ns_sim::reg_count_for(arch))
        .map(|i| {
            json!({
                "index": i,
                "name": ns_sim::reg_label_for(arch, i),
                "value": core.read_reg(i),
            })
        })
        .collect::<Vec<_>>();
    let id = format!(
        "sim-{}",
        SIM_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let project_name = project.config.project.name.clone();
    let mut sessions = sim_sessions().lock().unwrap_or_else(|e| e.into_inner());
    sessions.push(SimSession {
        simulator: core,
        name: id.clone(),
        entry,
    });
    Ok(json!({
        "session": id,
        "project": project_name,
        "arch": arch.label(),
        "pc": pc,
        "cycle": cycle,
        "registers": regs,
    }))
}

fn sim_session_end(session: &str) -> Result<serde_json::Value> {
    let mut sessions = sim_sessions().lock().unwrap_or_else(|e| e.into_inner());
    let idx = sessions
        .iter()
        .position(|s| s.name == session)
        .ok_or_else(|| anyhow::anyhow!("模拟器会话不存在: {session}"))?;
    sessions.remove(idx);
    Ok(json!({"session": session, "removed": true}))
}

fn sim_step(session: &str, args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    let n = args
        .get("steps")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        .max(1);
    let (status, pc, cycle) = with_sim_session(session, |core| {
        let mut last = ns_sim::StepResult::Advanced;
        for _ in 0..n {
            last = core.step();
            if last != ns_sim::StepResult::Advanced {
                break;
            }
        }
        Ok((step_status(last), core.program_counter(), core.cycle_count))
    })?;
    Ok(json!({"session": session, "steps": n, "status": status, "pc": pc, "cycle": cycle}))
}

fn sim_run(session: &str, args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    let n = args.get("steps").and_then(|v| v.as_u64()).unwrap_or(100);
    let (status, pc, cycle) = with_sim_session(session, |core| {
        let result = core.run(n);
        Ok((
            step_status(result),
            core.program_counter(),
            core.cycle_count,
        ))
    })?;
    Ok(json!({"session": session, "steps": n, "status": status, "pc": pc, "cycle": cycle}))
}

fn sim_reset(session: &str) -> Result<serde_json::Value> {
    let mut sessions = sim_sessions().lock().unwrap_or_else(|e| e.into_inner());
    let s = sessions
        .iter_mut()
        .find(|s| s.name == session)
        .ok_or_else(|| anyhow::anyhow!("模拟器会话不存在: {session}"))?;
    let entry = s.entry;
    s.simulator.reset(entry);
    Ok(json!({
        "session": session,
        "pc": s.simulator.program_counter(),
        "cycle": s.simulator.cycle_count,
    }))
}

fn sim_status(session: &str) -> Result<serde_json::Value> {
    let (pc, cycle, halted, arch, uart_output, regs) = with_sim_session(session, |core| {
        let arch = core.arch;
        let regs = (0..ns_sim::reg_count_for(arch))
            .map(|i| {
                json!({
                    "index": i,
                    "name": ns_sim::reg_label_for(arch, i),
                    "value": core.read_reg(i),
                })
            })
            .collect::<Vec<_>>();
        let state = core.snapshot();
        Ok((
            core.program_counter(),
            core.cycle_count,
            core.halted,
            arch,
            state.uart_output,
            regs,
        ))
    })?;
    Ok(json!({
        "session": session,
        "pc": pc,
        "cycle": cycle,
        "halted": halted,
        "arch": arch.label(),
        "uart_output": uart_output,
        "registers": regs,
    }))
}

fn sim_read_mem(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let address =
        args.get("address")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("missing required argument: address"))? as u32;
    let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(64) as usize;
    let (hex, ascii, bytes) = with_sim_session(session, |core| {
        let mut bytes = Vec::with_capacity(count);
        for i in 0..count {
            bytes.push(core.read_u8(address.wrapping_add(i as u32)));
        }
        let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let ascii = bytes
            .iter()
            .map(|b| {
                if b.is_ascii_graphic() || *b == b' ' {
                    *b as char
                } else {
                    '.'
                }
            })
            .collect::<String>();
        Ok((hex, ascii, bytes))
    })?;
    Ok(json!({
        "session": session,
        "address": address,
        "count": count,
        "hex": hex,
        "ascii": ascii,
        "bytes": bytes,
    }))
}

fn sim_write_mem(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let address =
        args.get("address")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("missing required argument: address"))? as u32;
    let data = arg_str(args, "data")?;
    let hex = args.get("hex").and_then(|v| v.as_bool()).unwrap_or(true);
    let payload = if hex {
        decode_hex_bytes(data)?
    } else {
        data.as_bytes().to_vec()
    };
    let (written, pc, cycle) = with_sim_session(session, |core| {
        core.write_bytes(address, &payload);
        Ok((payload.len(), core.program_counter(), core.cycle_count))
    })?;
    Ok(json!({
        "session": session,
        "address": address,
        "written": written,
        "bytes": payload,
        "pc": pc,
        "cycle": cycle,
    }))
}

fn sim_read_reg(session: &str, register: &str) -> Result<serde_json::Value> {
    let (index, name, value) = with_sim_session(session, |core| {
        let arch = core.arch;
        let idx = resolve_reg_index(arch, register)?;
        if idx == PC_REGISTER {
            Ok((idx, "pc".to_string(), core.program_counter()))
        } else {
            Ok((idx, ns_sim::reg_label_for(arch, idx), core.read_reg(idx)))
        }
    })?;
    Ok(json!({"session": session, "register": name, "index": index, "value": value}))
}

fn sim_write_reg(
    session: &str,
    register: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let value = args
        .get("value")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("missing required argument: value"))? as u32;
    let (index, name, new_value) = with_sim_session(session, |core| {
        let arch = core.arch;
        let idx = resolve_reg_index(arch, register)?;
        if idx == PC_REGISTER {
            core.set_program_counter(value);
            Ok((idx, "pc".to_string(), core.program_counter()))
        } else {
            core.write_reg(idx, value);
            Ok((idx, ns_sim::reg_label_for(arch, idx), core.read_reg(idx)))
        }
    })?;
    Ok(json!({"session": session, "register": name, "index": index, "value": new_value}))
}

fn sim_disassemble(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let start = args.get("address").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(16) as usize;
    let (arch, instructions) = with_sim_session(session, |core| {
        let arch = core.arch;
        let arm_state = core.arm_state();
        let mut addr = start;
        let mut list = Vec::with_capacity(count);
        for _ in 0..count {
            let read = |a: u32| core.read_u8(a);
            let text = ns_sim::disassemble_one_state(arch, addr, &read, arm_state);
            let size = instruction_len(arch, addr, &read, arm_state);
            list.push(json!({"address": addr, "text": text, "size": size}));
            addr = addr.wrapping_add(size);
        }
        Ok((arch, list))
    })?;
    Ok(json!({
        "session": session,
        "arch": arch.label(),
        "count": count,
        "instructions": instructions,
    }))
}

fn sim_snapshot_save(session: &str, path: &str) -> Result<serde_json::Value> {
    let saved = with_sim_session(session, |core| {
        core.save_snapshot(path)
            .map_err(|e| anyhow::anyhow!("保存快照失败: {e}"))
    })?;
    Ok(json!({"session": session, "path": saved}))
}

fn sim_snapshot_load(session: &str, path: &str) -> Result<serde_json::Value> {
    let (pc, cycle) = with_sim_session(session, |core| {
        core.load_snapshot(path)
            .map_err(|e| anyhow::anyhow!("加载快照失败: {e}"))?;
        Ok((core.program_counter(), core.cycle_count))
    })?;
    Ok(json!({"session": session, "path": path, "pc": pc, "cycle": cycle}))
}

fn sim_breakpoint_add(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let address = arg_u32(args, "address")?;
    let condition = arg_str_opt(args, "condition").unwrap_or_default();
    let hit_count = args.get("hit_count").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let (added, count) = with_sim_session(session, |core| {
        let added = core.set_breakpoint_ex(address, condition.clone(), hit_count);
        Ok((added, core.breakpoints.len()))
    })?;
    Ok(json!({
        "session": session,
        "address": address,
        "added": added,
        "condition": condition,
        "hit_count": hit_count,
        "count": count,
    }))
}

fn sim_breakpoint_remove(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let address = arg_u32(args, "address")?;
    let (removed, count) = with_sim_session(session, |core| {
        let removed = core.clear_breakpoint(address);
        Ok((removed, core.breakpoints.len()))
    })?;
    Ok(json!({
        "session": session,
        "address": address,
        "removed": removed,
        "count": count,
    }))
}

fn sim_breakpoint_list(session: &str) -> Result<serde_json::Value> {
    let list = with_sim_session(session, |core| {
        Ok(core
            .breakpoints
            .iter()
            .map(|bp| {
                json!({
                    "address": bp.address,
                    "condition": bp.condition,
                    "hit_count": bp.hit_count,
                    "hits": bp.hits(),
                })
            })
            .collect::<Vec<_>>())
    })?;
    Ok(json!({"session": session, "count": list.len(), "breakpoints": list}))
}

fn sim_clear_breakpoints(session: &str) -> Result<serde_json::Value> {
    let cleared = with_sim_session(session, |core| {
        let cleared = core.breakpoints.len();
        core.breakpoints.clear();
        Ok(cleared)
    })?;
    Ok(json!({"session": session, "cleared": cleared}))
}

fn sim_step_over(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let max_steps = args
        .get("steps")
        .and_then(|v| v.as_u64())
        .unwrap_or(10_000)
        .max(1);
    let (status, pc, cycle) = with_sim_session(session, |core| {
        let result = core.step_over(max_steps);
        Ok((
            step_status(result),
            core.program_counter(),
            core.cycle_count,
        ))
    })?;
    Ok(json!({"session": session, "status": status, "pc": pc, "cycle": cycle}))
}

fn sim_step_out(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let max_steps = args
        .get("steps")
        .and_then(|v| v.as_u64())
        .unwrap_or(10_000)
        .max(1);
    let (status, pc, cycle) = with_sim_session(session, |core| {
        let result = core.step_out(max_steps);
        Ok((
            step_status(result),
            core.program_counter(),
            core.cycle_count,
        ))
    })?;
    Ok(json!({"session": session, "status": status, "pc": pc, "cycle": cycle}))
}

fn sim_run_to(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let address = arg_u32(args, "address")?;
    let max_steps = args
        .get("steps")
        .and_then(|v| v.as_u64())
        .unwrap_or(10_000)
        .max(1);
    let (status, pc, cycle, steps, reached) = with_sim_session(session, |core| {
        let mut steps = 0u64;
        let mut last = ns_sim::StepResult::Advanced;
        while steps < max_steps {
            if core.program_counter() == address {
                return Ok((
                    ns_sim::StepResult::Advanced,
                    core.program_counter(),
                    core.cycle_count,
                    steps,
                    true,
                ));
            }
            last = core.step();
            steps += 1;
            if last != ns_sim::StepResult::Advanced {
                return Ok((
                    last,
                    core.program_counter(),
                    core.cycle_count,
                    steps,
                    core.program_counter() == address,
                ));
            }
        }
        Ok((
            last,
            core.program_counter(),
            core.cycle_count,
            steps,
            core.program_counter() == address,
        ))
    })?;
    Ok(json!({
        "session": session,
        "status": step_status(status),
        "pc": pc,
        "cycle": cycle,
        "steps": steps,
        "reached": reached,
    }))
}

fn sim_peripherals(session: &str) -> Result<serde_json::Value> {
    let (gpio_out, gpio_dir, gpio_in, uart_base, uart_output, timers) =
        with_sim_session(session, |core| {
            let p = &core.peripherals;
            let timers = p
                .timers
                .iter()
                .map(|t| {
                    json!({
                        "base": t.base,
                        "enabled": t.enabled,
                        "prescaler": t.prescaler,
                        "counter": t.counter,
                        "auto_reload": t.auto_reload,
                        "irq_line": t.irq_line,
                        "update_count": t.update_count,
                    })
                })
                .collect::<Vec<_>>();
            Ok((
                p.gpio_out,
                p.gpio_dir,
                p.gpio_in,
                p.uart_base,
                String::from_utf8_lossy(&p.outputs).into_owned(),
                timers,
            ))
        })?;
    Ok(json!({
        "session": session,
        "gpio_out": gpio_out,
        "gpio_dir": gpio_dir,
        "gpio_in": gpio_in,
        "uart_base": uart_base,
        "uart_output": uart_output,
        "timers": timers,
    }))
}

fn sim_uart_read(session: &str) -> Result<serde_json::Value> {
    let (bytes, text) = with_sim_session(session, |core| {
        let bytes = std::mem::take(&mut core.peripherals.outputs);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        Ok((bytes, text))
    })?;
    Ok(json!({"session": session, "bytes": bytes, "text": text}))
}

fn sim_load_image(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let address = arg_u32(args, "address")?;
    let bytes = if let Some(path) = arg_str_opt(args, "path") {
        fs::read(&path).with_context(|| format!("读取固件 {} 失败", path))?
    } else {
        let data = arg_str(args, "data")?;
        let hex = args.get("hex").and_then(|v| v.as_bool()).unwrap_or(true);
        if hex {
            decode_hex_bytes(data)?
        } else {
            data.as_bytes().to_vec()
        }
    };
    let (written, pc, cycle) = with_sim_session(session, |core| {
        core.write_bytes(address, &bytes);
        Ok((bytes.len(), core.program_counter(), core.cycle_count))
    })?;
    Ok(json!({
        "session": session,
        "address": address,
        "written": written,
        "bytes": bytes,
        "pc": pc,
        "cycle": cycle,
    }))
}

// ── Debug (DAP) tools ────────────────────────────────────────────────────────

fn parse_debug_breakpoints(args: &HashMap<String, serde_json::Value>) -> Vec<DebugBreakpoint> {
    let mut out = Vec::new();
    if let Some(items) = args.get("breakpoints").and_then(|v| v.as_array()) {
        for item in items {
            let file = item
                .get("file")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let line = item.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            if file.is_empty() || line == 0 {
                continue;
            }
            out.push(DebugBreakpoint {
                path: file,
                line,
                condition: item
                    .get("condition")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                hit_condition: item
                    .get("hit_condition")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                log_message: item
                    .get("log_message")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                hardware: item
                    .get("hardware")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            });
        }
    }
    out
}

fn is_control_message(message: &DebugMessage) -> bool {
    matches!(
        message,
        DebugMessage::Ready
            | DebugMessage::Stopped { .. }
            | DebugMessage::Continued
            | DebugMessage::Terminated
            | DebugMessage::Error(_)
            | DebugMessage::Output { .. }
            | DebugMessage::Caret { .. }
    )
}

fn debug_session_start(args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    debug_session_start_in(&current_project_root(), args)
}

fn debug_session_start_in(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let program = arg_str(args, "program")?.to_string();
    let cwd = arg_str_opt(args, "cwd")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.to_path_buf());
    let prog_args = args
        .get("args")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let env = args
        .get("env")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let breakpoints = parse_debug_breakpoints(args);
    let config = DebugConfig {
        program: PathBuf::from(&program),
        args: prog_args,
        env,
        cwd,
        breakpoints: breakpoints.clone(),
    };
    let handle = ns_debug::spawn(&config).context("启动调试会话失败（需要 lldb-dap 适配器）")?;
    let id = format!(
        "dbg-{}",
        DEBUG_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let mut sessions = debug_sessions().lock().unwrap_or_else(|e| e.into_inner());
    sessions.push(DebugSession {
        handle,
        name: id.clone(),
        program: PathBuf::from(&program),
        breakpoints,
        pending: VecDeque::new(),
        last_status: "starting".to_string(),
        last_thread: 1,
        output_log: String::new(),
    });
    Ok(json!({"session": id, "program": program, "ready": true}))
}

fn debug_session_stop(session: &str) -> Result<serde_json::Value> {
    let mut sessions = debug_sessions().lock().unwrap_or_else(|e| e.into_inner());
    let idx = sessions
        .iter()
        .position(|s| s.name == session)
        .ok_or_else(|| anyhow::anyhow!("调试会话不存在: {session}"))?;
    let s = &mut sessions[idx];
    let _ = s.handle.send(DebugCommand::Stop);
    let name = s.name.clone();
    sessions.remove(idx);
    Ok(json!({"session": name, "stopped": true}))
}

fn debug_status(session: &str) -> Result<serde_json::Value> {
    with_debug_session(session, |s| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match s.handle.try_recv() {
                Ok(message) => {
                    s.note_message(&message);
                    if !is_control_message(&message) {
                        s.pending.push_back(message);
                    }
                }
                Err(TryRecvError::Empty) => {
                    if s.last_status == "starting" && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    s.last_status = "disconnected".to_string();
                    break;
                }
            }
        }
        Ok(json!({
            "session": session,
            "status": s.last_status,
            "thread": s.last_thread,
            "output": s.output_log,
            "program": s.program.display().to_string(),
            "breakpoints": s.breakpoints.len(),
        }))
    })
}

fn debug_continue(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let thread = args.get("thread").and_then(|v| v.as_u64()).unwrap_or(1);
    with_debug_session(session, |s| {
        s.handle
            .send(DebugCommand::Continue { thread_id: thread })
            .context("发送 continue 失败")?;
        debug_recv_until(
            s,
            |m| matches!(m, DebugMessage::Continued),
            Duration::from_secs(5),
            "继续运行",
        )?;
        Ok(json!({"session": session, "status": "continued", "thread": thread}))
    })
}

fn debug_step(session: &str, command: DebugCommand, what: &str) -> Result<serde_json::Value> {
    with_debug_session(session, |s| {
        s.handle
            .send(command)
            .with_context(|| format!("发送 {what} 失败"))?;
        let message = debug_recv_until(
            s,
            |m| matches!(m, DebugMessage::Stopped { .. }),
            Duration::from_secs(5),
            what,
        )?;
        let DebugMessage::Stopped {
            reason,
            thread_id,
            description,
        } = message
        else {
            unreachable!()
        };
        Ok(json!({
            "session": session,
            "reason": reason,
            "thread": thread_id,
            "description": description,
        }))
    })
}

fn debug_step_over(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let thread = args.get("thread").and_then(|v| v.as_u64()).unwrap_or(1);
    debug_step(
        session,
        DebugCommand::StepOver { thread_id: thread },
        "步过",
    )
}

fn debug_step_into(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let thread = args.get("thread").and_then(|v| v.as_u64()).unwrap_or(1);
    debug_step(session, DebugCommand::StepIn { thread_id: thread }, "步入")
}

fn debug_step_out(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let thread = args.get("thread").and_then(|v| v.as_u64()).unwrap_or(1);
    debug_step(session, DebugCommand::StepOut { thread_id: thread }, "步出")
}

fn debug_step_instruction(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let thread = args.get("thread").and_then(|v| v.as_u64()).unwrap_or(1);
    debug_step(
        session,
        DebugCommand::StepInstruction { thread_id: thread },
        "单指令步进",
    )
}

fn debug_pause(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let thread = args.get("thread").and_then(|v| v.as_u64()).unwrap_or(1);
    debug_step(session, DebugCommand::Pause { thread_id: thread }, "暂停")
}

fn debug_resync_breakpoints(s: &mut DebugSession) -> Result<Vec<ns_debug::DebugBreakpointResult>> {
    let groups = ns_debug::group_breakpoints(&s.breakpoints);
    if groups.is_empty() {
        return Ok(Vec::new());
    }
    s.handle
        .send(DebugCommand::ResyncBreakpoints(groups))
        .context("发送断点同步失败")?;
    let message = debug_recv_until(
        s,
        |m| matches!(m, DebugMessage::BreakpointResults { .. }),
        Duration::from_secs(5),
        "断点绑定",
    )?;
    let DebugMessage::BreakpointResults { results, .. } = message else {
        unreachable!()
    };
    Ok(results)
}

fn debug_breakpoint_set(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let file = arg_str(args, "file")?.to_string();
    let line = arg_usize(args, "line")?;
    let condition = arg_str_opt(args, "condition");
    let hit_condition = arg_str_opt(args, "hit_condition");
    let log_message = arg_str_opt(args, "log_message");
    let hardware = args
        .get("hardware")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    with_debug_session(session, |s| {
        s.breakpoints
            .retain(|bp| !(bp.path == file && bp.line == line));
        s.breakpoints.push(DebugBreakpoint {
            path: file.clone(),
            line,
            condition,
            hit_condition,
            log_message,
            hardware,
        });
        let results = debug_resync_breakpoints(s)?;
        Ok(json!({
            "session": session,
            "file": file,
            "line": line,
            "count": s.breakpoints.len(),
            "results": results
                .iter()
                .map(|r| json!({"line": r.line, "verified": r.verified, "reason": r.reason}))
                .collect::<Vec<_>>(),
        }))
    })
}

fn debug_breakpoint_clear(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let file = arg_str(args, "file")?.to_string();
    let line = arg_usize(args, "line")?;
    with_debug_session(session, |s| {
        let before = s.breakpoints.len();
        s.breakpoints
            .retain(|bp| !(bp.path == file && bp.line == line));
        let removed = before != s.breakpoints.len();
        let results = debug_resync_breakpoints(s)?;
        Ok(json!({
            "session": session,
            "file": file,
            "line": line,
            "removed": removed,
            "count": s.breakpoints.len(),
            "results": results
                .iter()
                .map(|r| json!({"line": r.line, "verified": r.verified, "reason": r.reason}))
                .collect::<Vec<_>>(),
        }))
    })
}

fn debug_breakpoint_list(session: &str) -> Result<serde_json::Value> {
    with_debug_session(session, |s| {
        let list = s
            .breakpoints
            .iter()
            .map(|bp| {
                json!({
                    "file": bp.path,
                    "line": bp.line,
                    "condition": bp.condition,
                    "hit_condition": bp.hit_condition,
                    "log_message": bp.log_message,
                    "hardware": bp.hardware,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"session": session, "count": list.len(), "breakpoints": list}))
    })
}

fn debug_read_memory(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let address = arg_u64(args, "address")?;
    let count = arg_usize(args, "count")? as u64;
    let reference = format!("{address:#x}");
    with_debug_session(session, |s| {
        s.handle
            .send(DebugCommand::ReadMemory { reference, count })
            .context("发送 readMemory 失败")?;
        let message = debug_recv_until(
            s,
            |m| matches!(m, DebugMessage::Memory { .. }),
            Duration::from_secs(5),
            "内存读取",
        )?;
        let DebugMessage::Memory { data, .. } = message else {
            unreachable!()
        };
        let hex = data.iter().map(|b| format!("{b:02x}")).collect::<String>();
        Ok(json!({
            "session": session,
            "address": address,
            "count": count,
            "bytes": data,
            "hex": hex,
        }))
    })
}

fn debug_write_memory(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let address = arg_u64(args, "address")?;
    let data = arg_str(args, "data")?;
    let hex = args.get("hex").and_then(|v| v.as_bool()).unwrap_or(true);
    let bytes = if hex {
        decode_hex_bytes(data)?
    } else {
        data.as_bytes().to_vec()
    };
    let reference = format!("{address:#x}");
    with_debug_session(session, |s| {
        s.handle
            .send(DebugCommand::WriteMemory {
                reference,
                data: bytes.clone(),
            })
            .context("发送 writeMemory 失败")?;
        let message = debug_recv_until(
            s,
            |m| matches!(m, DebugMessage::MemoryWritten { .. }),
            Duration::from_secs(5),
            "内存写入",
        )?;
        let DebugMessage::MemoryWritten {
            bytes: written_count,
            ..
        } = message
        else {
            unreachable!()
        };
        Ok(json!({
            "session": session,
            "address": address,
            "written": written_count,
            "bytes": bytes,
        }))
    })
}

fn debug_stacks(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let thread = args.get("thread").and_then(|v| v.as_u64()).unwrap_or(1);
    with_debug_session(session, |s| {
        s.handle
            .send(DebugCommand::GetStackTrace { thread_id: thread })
            .context("发送 GetStackTrace 失败")?;
        let message = debug_recv_until(
            s,
            |m| matches!(m, DebugMessage::StackTrace { .. }),
            Duration::from_secs(5),
            "调用栈",
        )?;
        let DebugMessage::StackTrace { frames } = message else {
            unreachable!()
        };
        let list = frames
            .iter()
            .map(|f| {
                json!({
                    "id": f.id,
                    "name": f.name,
                    "file": f.file,
                    "line": f.line,
                    "column": f.column,
                    "pc": f.pc,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "session": session,
            "thread": thread,
            "count": frames.len(),
            "frames": list,
        }))
    })
}

fn debug_variables(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let thread = args.get("thread").and_then(|v| v.as_u64()).unwrap_or(1);
    let frame = args.get("frame").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let scope = arg_str_opt(args, "scope").unwrap_or_else(|| "loc".to_string());
    with_debug_session(session, |s| {
        let f = frame_for_thread(s, thread, frame)?;
        let (reference, scope_name, _) = debug_scope_reference(s, f.id, &scope)?;
        s.handle
            .send(DebugCommand::GetVariables {
                variables_reference: reference,
            })
            .context("发送 GetVariables 失败")?;
        let message = debug_recv_until(
            s,
            |m| matches!(m, DebugMessage::Variables { .. }),
            Duration::from_secs(5),
            "变量",
        )?;
        let DebugMessage::Variables { variables, .. } = message else {
            unreachable!()
        };
        let list = variables
            .iter()
            .map(|v| {
                json!({
                    "name": v.name,
                    "value": v.value,
                    "variables_reference": v.variables_reference,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "session": session,
            "frame": frame,
            "scope": scope_name,
            "count": list.len(),
            "variables": list,
        }))
    })
}

fn debug_read_registers(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let thread = args.get("thread").and_then(|v| v.as_u64()).unwrap_or(1);
    let frame = args.get("frame").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    with_debug_session(session, |s| {
        let f = frame_for_thread(s, thread, frame)?;
        let (reference, scope_name, _) = debug_scope_reference(s, f.id, "Registers")?;
        s.handle
            .send(DebugCommand::GetVariables {
                variables_reference: reference,
            })
            .context("发送 GetVariables 失败")?;
        let message = debug_recv_until(
            s,
            |m| matches!(m, DebugMessage::Variables { .. }),
            Duration::from_secs(5),
            "寄存器",
        )?;
        let DebugMessage::Variables { variables, .. } = message else {
            unreachable!()
        };
        let list = variables
            .iter()
            .map(|v| {
                json!({
                    "name": v.name,
                    "value": v.value,
                    "variables_reference": v.variables_reference,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "session": session,
            "frame": frame,
            "scope": scope_name,
            "count": list.len(),
            "registers": list,
        }))
    })
}

fn debug_disassemble(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let address = arg_u64(args, "address")?;
    let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(24) as u64;
    let reference = format!("{address:#x}");
    with_debug_session(session, |s| {
        s.handle
            .send(DebugCommand::Disassemble {
                reference,
                instruction_count: count,
            })
            .context("发送 disassemble 失败")?;
        let message = debug_recv_until(
            s,
            |m| matches!(m, DebugMessage::Disassembly { .. }),
            Duration::from_secs(5),
            "反汇编",
        )?;
        let DebugMessage::Disassembly { instructions } = message else {
            unreachable!()
        };
        let list = instructions
            .iter()
            .map(|i| {
                json!({
                    "address": i.address,
                    "instruction": i.instruction,
                    "bytes": i.bytes,
                    "line": i.line,
                    "file": i.file,
                    "symbol": i.symbol,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "session": session,
            "address": address,
            "count": list.len(),
            "instructions": list,
        }))
    })
}

fn debug_evaluate(
    session: &str,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let expression = arg_str(args, "expression")?.to_string();
    with_debug_session(session, |s| {
        let thread = args
            .get("thread")
            .and_then(|v| v.as_u64())
            .unwrap_or(s.last_thread);
        let frame = args.get("frame").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let f = frame_for_thread(s, thread, frame)?;
        s.handle
            .send(DebugCommand::EvaluateFrame {
                expression: expression.clone(),
                frame_id: f.id,
            })
            .context("发送 evaluate 失败")?;
        let message = debug_recv_until(
            s,
            |m| matches!(m, DebugMessage::Evaluated { .. }),
            Duration::from_secs(5),
            "求值",
        )?;
        let DebugMessage::Evaluated {
            result, type_name, ..
        } = message
        else {
            unreachable!()
        };
        Ok(json!({
            "session": session,
            "expression": expression,
            "result": result,
            "type": type_name,
        }))
    })
}

fn run_simulation(args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    let steps = args.get("steps").and_then(|v| v.as_u64()).unwrap_or(100);
    let firmware = args.get("firmware").and_then(|v| v.as_str());
    let project = open_project_in(&current_project_root())?;
    let mut core = simulator_from_project(&project, firmware)?;
    let result = core.run(steps);
    let pc = core.program_counter();
    let arch = Arch::from_target(&project.config.project.target);
    let regs = (0..ns_sim::reg_count_for(arch))
        .map(|i| {
            json!({
                "index": i,
                "name": ns_sim::reg_label_for(arch, i),
                "value": core.read_reg(i),
            })
        })
        .collect::<Vec<_>>();
    let status = match result {
        ns_sim::StepResult::Halted => "halted",
        ns_sim::StepResult::Breakpoint => "breakpoint",
        ns_sim::StepResult::Advanced => "advanced",
    };
    Ok(json!({
        "steps": steps,
        "pc": pc,
        "status": status,
        "cycle": core.cycle_count,
        "arch": arch.label(),
        "registers": regs,
    }))
}

fn mem_search(args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    let start = args
        .get("start")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("missing required argument: start"))?;
    let length = args
        .get("length")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("missing required argument: length"))?;
    let pattern_values = args
        .get("pattern")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing required argument: pattern"))?;
    let pattern: Vec<u8> = pattern_values
        .iter()
        .map(|v| v.as_u64().unwrap_or(0) as u8)
        .collect();
    let firmware = args.get("firmware").and_then(|v| v.as_str());
    let project = open_project_in(&current_project_root())?;
    let core = simulator_from_project(&project, firmware)?;
    let matches = core.memsearch(start, length, &pattern);
    Ok(json!({
        "start": start,
        "length": length,
        "pattern": pattern,
        "matches": matches.iter().map(|m| json!({"address": m})).collect::<Vec<_>>(),
        "count": matches.len(),
    }))
}

// ── Firmware analysis tools ──────────────────────────────────────────────────

fn analyze_firmware(path: &str, target: &str) -> Result<serde_json::Value> {
    let target = if target.is_empty() {
        "stm32f103c8"
    } else {
        target
    };
    let regions = memory_map(target);
    let flash_windows: Vec<(u64, u64)> = regions
        .iter()
        .filter(|r| r.name.to_uppercase().contains("FLASH"))
        .map(|r| (r.start, r.size))
        .collect();
    let ram_windows: Vec<(u64, u64)> = regions
        .iter()
        .filter(|r| r.name.to_uppercase().contains("RAM"))
        .map(|r| (r.start, r.size))
        .collect();
    let size = analyze_firmware_size(Path::new(path), &flash_windows, &ram_windows)?
        .ok_or_else(|| anyhow::anyhow!("无法分析固件（不是有效的 ELF/HEX/BIN）: {}", path))?;
    Ok(json!({
        "path": path,
        "target": target,
        "flash": size.flash,
        "ram": size.ram,
        "text": size.text,
        "data": size.data,
        "bss": size.bss,
        "total": size.total,
        "base": size.base,
        "symbols": size.symbols.iter().map(|s| json!({
            "name": s.name,
            "size": s.size,
            "address": s.address,
        })).collect::<Vec<_>>(),
    }))
}

fn decode_hex(path: &str) -> Result<serde_json::Value> {
    let text = fs::read_to_string(path).with_context(|| format!("读取 Intel HEX {} 失败", path))?;
    let decoded = decode_intel_hex(&text)?;
    Ok(json!({
        "path": path,
        "base": decoded.base,
        "bytes": decoded.bytes.len(),
        "hex": decoded.bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "),
    }))
}

fn convert_firmware(path: &str, format: &str) -> Result<serde_json::Value> {
    let bytes = fs::read(path).with_context(|| format!("读取固件 {} 失败", path))?;
    let target_format = match format.to_lowercase().as_str() {
        "elf" => FirmwareFormat::Elf,
        "hex" | "ihex" | "ihx" => FirmwareFormat::IntelHex,
        "bin" => FirmwareFormat::Bin,
        other => bail!("不支持的输出格式: {}（可选: elf, hex, bin）", other),
    };
    // Detect the source: Intel HEX text, ELF magic, or raw BIN. The upstream
    // converter only accepts ELF input, so decode HEX/BIN sources to a raw
    // payload before emitting the requested format.
    let source_format = detect_format(Path::new(path));
    let (payload, base, source_label) = match source_format {
        FirmwareFormat::IntelHex => {
            let text = String::from_utf8(bytes)
                .with_context(|| format!("Intel HEX 文件不是有效 UTF-8: {}", path))?;
            let decoded = decode_intel_hex(&text)?;
            (decoded.bytes, decoded.base, "Intel HEX")
        }
        FirmwareFormat::Elf => {
            let image = inner_convert_firmware(&bytes, FirmwareFormat::Bin)?;
            (image.bytes, image.base, "ELF")
        }
        FirmwareFormat::Bin => (bytes, 0, "BIN"),
    };
    let (out_bytes, out_base, out_label) = match target_format {
        FirmwareFormat::Elf => {
            // ELF output is only meaningful from an ELF source; otherwise the
            // upstream encoder is unavailable for raw payloads.
            if source_format == FirmwareFormat::Elf {
                let image = inner_convert_firmware(&payload, FirmwareFormat::Elf)?;
                (image.bytes, image.base, "ELF")
            } else {
                bail!(
                    "无法从 {} 源生成 ELF（仅支持 ELF -> ELF 原样转换）",
                    source_label
                );
            }
        }
        FirmwareFormat::IntelHex => {
            let image = inner_convert_firmware(&payload, FirmwareFormat::IntelHex)?;
            (image.bytes, image.base, "Intel HEX")
        }
        FirmwareFormat::Bin => (payload, base, "BIN"),
    };
    Ok(json!({
        "path": path,
        "source": source_label,
        "format": out_label,
        "base": out_base,
        "bytes": out_bytes.len(),
        "hex": if target_format == FirmwareFormat::IntelHex {
            String::from_utf8_lossy(&out_bytes).to_string()
        } else {
            out_bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
        },
    }))
}

/// Decode an ARM Cortex-M fault snapshot into a human-readable report.
///
/// Callers may pass a complete `snapshot` object (matching
/// `ns_debug::fault::FaultSnapshot`) or individual `cfsr`/`hfsr`/`mmfar`/
/// `bfar`/`afsr`/`stacked`/`lr_exc_return` fields. The decoder is pure and
/// side-effect free, so the same JSON can round-trip through the IDE and the
/// MCP boundary without any hardware or debug session.
fn decode_hardfault(args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    use ns_debug::fault::FaultSnapshot;
    let snapshot = if let Some(value) = args.get("snapshot") {
        serde_json::from_value::<FaultSnapshot>(value.clone())
            .with_context(|| "snapshot 必须是有效的 FaultSnapshot JSON")?
    } else {
        let mut snap = FaultSnapshot::default();
        if let Some(v) = args.get("cfsr").and_then(|v| v.as_u64()) {
            snap.cfsr = v as u32;
        }
        if let Some(v) = args.get("hfsr").and_then(|v| v.as_u64()) {
            snap.hfsr = v as u32;
        }
        if let Some(v) = args.get("mmfar").and_then(|v| v.as_u64()) {
            snap.mmfar = Some(v as u32);
        }
        if let Some(v) = args.get("bfar").and_then(|v| v.as_u64()) {
            snap.bfar = Some(v as u32);
        }
        if let Some(v) = args.get("afsr").and_then(|v| v.as_u64()) {
            snap.afsr = Some(v as u32);
        }
        if let Some(v) = args.get("lr_exc_return").and_then(|v| v.as_u64()) {
            snap.lr_exc_return = Some(v as u32);
        }
        if let Some(array) = args.get("stacked").and_then(|v| v.as_array()) {
            for (index, value) in array.iter().take(8).enumerate() {
                snap.stacked[index] = value.as_u64().unwrap_or(0) as u32;
            }
        }
        snap
    };
    let report = ns_debug::fault::decode_fault(&snapshot);
    Ok(serde_json::to_value(&report)?)
}

/// Build waveform channels from already-decoded SWV/ITM packets.
///
/// `packets` accepts the JSON form of `ns_debug::swvd::Packet`
/// (`Stimulus`/`Dwt`/`Exception`/`Timestamp`). When `channels` is omitted or
/// empty, the channels present in the stream are auto-detected.
fn trace_waveform(args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    use ns_debug::swvd::Packet;
    use ns_debug::waveform::{ChannelKind, available_channels, build_channels};
    let packets: Vec<Packet> = match args.get("packets") {
        Some(value) => serde_json::from_value(value.clone())
            .with_context(|| "packets 必须是已解码的 SWV/ITM 包数组")?,
        None => bail!("missing required argument: packets"),
    };
    let kinds: Vec<ChannelKind> = match args.get("channels").and_then(|v| v.as_array()) {
        Some(array) => array
            .iter()
            .filter_map(|v| v.as_str())
            .filter_map(ChannelKind::from_id)
            .collect(),
        None => available_channels(&packets),
    };
    let data = build_channels(&packets, &kinds);
    Ok(json!({
        "packet_count": data.packet_count,
        "has_timestamps": data.has_timestamps,
        "channels": data.channels.iter().map(|channel| json!({
            "id": channel.kind.id(),
            "label": channel.label,
            "has_timestamp": channel.has_timestamp,
            "samples": channel.samples.iter().map(|sample| json!({
                "t": sample.t,
                "value": sample.value,
                "raw": sample.raw,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    }))
}

/// Aggregate DWT PC samples into per-function hotspots using the firmware's
/// `nm` symbol table. Pure and offline: the caller supplies the symbols and
/// the sampled PCs, so the same input can be re-profiled without a target.
fn profile_hotspots(args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    use ns_debug::profile::ProfileAggregator;
    let symbols = args
        .get("symbols")
        .and_then(|value| value.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing required argument: symbols"))?;
    let symbol_table: Vec<(u64, u64, String)> = symbols
        .iter()
        .map(|entry| {
            (
                entry.get("address").and_then(|v| v.as_u64()).unwrap_or(0),
                entry.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                entry
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .collect();
    let samples = args
        .get("samples")
        .and_then(|value| value.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing required argument: samples"))?;
    let mut aggregator = ProfileAggregator::new(&symbol_table);
    for pc in samples {
        aggregator.record(pc.as_u64().unwrap_or(0) as u32);
    }
    Ok(json!({
        "total": aggregator.total(),
        "hotspots": aggregator.hotspots().iter().map(|hotspot| json!({
            "name": hotspot.name,
            "hits": hotspot.hits,
        })).collect::<Vec<_>>(),
    }))
}

/// Enumerate FreeRTOS tasks from a raw RAM image by walking the scheduler's
/// intrusive lists. Pure and offline: the caller supplies the memory image and
/// the list head addresses (as a debugger or simulator would read them), so an
/// MCP client can decode a task snapshot without a live kernel.
fn rtos_tasks(args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    use ns_debug::rtos::{RtosLayout, walk_free_rtos};
    let memory = args
        .get("memory")
        .and_then(|value| value.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing required argument: memory"))?;
    let bytes: Vec<u8> = memory
        .iter()
        .map(|value| value.as_u64().unwrap_or(0) as u8)
        .collect();
    let base = args
        .get("base")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let lists = args
        .get("lists")
        .and_then(|value| value.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing required argument: lists"))?;
    let list_heads: Vec<(u64, u32)> = lists
        .iter()
        .map(|entry| {
            (
                entry.get("address").and_then(|v| v.as_u64()).unwrap_or(0),
                entry.get("state").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            )
        })
        .collect();
    let running_tcb = args.get("running_tcb").and_then(|value| value.as_u64());
    let layout = match args.get("layout") {
        Some(value) => RtosLayout {
            word_size: value.get("word_size").and_then(|v| v.as_u64()).unwrap_or(4) as usize,
            tcb_pc_task_name: value
                .get("tcb_pc_task_name")
                .and_then(|v| v.as_u64())
                .unwrap_or(44) as usize,
            tcb_ux_priority: value
                .get("tcb_ux_priority")
                .and_then(|v| v.as_u64())
                .unwrap_or(36) as usize,
            tcb_x_state_list_item: value
                .get("tcb_x_state_list_item")
                .and_then(|v| v.as_u64())
                .unwrap_or(4) as usize,
            list_item_pv_owner: value
                .get("list_item_pv_owner")
                .and_then(|v| v.as_u64())
                .unwrap_or(12) as usize,
            list_item_px_next: value
                .get("list_item_px_next")
                .and_then(|v| v.as_u64())
                .unwrap_or(4) as usize,
            list_x_list_end: value
                .get("list_x_list_end")
                .and_then(|v| v.as_u64())
                .unwrap_or(8) as usize,
            list_ux_number_of_items: value
                .get("list_ux_number_of_items")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize,
            task_name_len: value
                .get("task_name_len")
                .and_then(|v| v.as_u64())
                .unwrap_or(16) as usize,
        },
        None => RtosLayout::default(),
    };
    let reader = |address: u64, length: usize| -> Option<Vec<u8>> {
        let offset = address.checked_sub(base)? as usize;
        let end = offset.checked_add(length)?;
        if end > bytes.len() {
            return None;
        }
        Some(bytes[offset..end].to_vec())
    };
    let tasks = walk_free_rtos(&reader, &layout, &list_heads, running_tcb);
    Ok(json!({
        "count": tasks.len(),
        "tasks": tasks.iter().map(|task| json!({
            "name": task.name,
            "priority": task.priority,
            "running": task.running,
            "state": task.state,
            "tcb": task.tcb,
        })).collect::<Vec<_>>(),
    }))
}

/// Decode raw SWO/ITM bytes into SWV packets and aggregate trace statistics.
///
/// Accepts `bytes` (an integer array) or `hex` (a hex string). The returned
/// `packets` use the same JSON shape `trace_waveform` consumes, so an LLM can
/// pipe `decode_trace` output straight into channel building.
fn decode_trace(args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    use ns_debug::swvd::{decode, trace_stats};
    let bytes: Vec<u8> = if let Some(array) = args.get("bytes").and_then(|v| v.as_array()) {
        array
            .iter()
            .map(|value| value.as_u64().unwrap_or(0) as u8)
            .collect()
    } else if let Some(hex) = args.get("hex").and_then(|v| v.as_str()) {
        parse_hex_bytes(hex)?
    } else {
        bail!("缺少参数: bytes 或 hex");
    };
    let (packets, consumed) = decode(&bytes);
    let stats = trace_stats(&packets);
    Ok(json!({
        "consumed": consumed,
        "total": bytes.len(),
        "packets": serde_json::to_value(&packets)?,
        "stats": json!({
            "stimulus": stats.stimulus,
            "dwt": stats.dwt,
            "exceptions": stats.exceptions,
            "timestamps": stats.timestamps,
            "pc_samples": stats.pc_samples,
        }),
    }))
}

/// Run a shell command in the project directory and capture its output.
///
/// This is the low-level "operate the software" primitive: an LLM can drive
/// build/script/test steps that are not otherwise exposed as a named tool.
fn run_command(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    use std::time::Instant;
    let command = arg_str(args, "command")?;
    let timeout_ms = parse_timeout_ms(args);
    let cwd = args
        .get("cwd")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| resolve_path(root, s))
        .unwrap_or_else(|| root.to_path_buf());
    let started = Instant::now();
    let output = shell_run_opt(command, &cwd, timeout_ms)?;
    Ok(json!({
        "success": output.status.success(),
        "code": output.status.code(),
        "stdout": String::from_utf8_lossy(&output.stdout).to_string(),
        "stderr": String::from_utf8_lossy(&output.stderr).to_string(),
        "duration_ms": started.elapsed().as_millis() as u64,
    }))
}

/// Run a user-defined external tool (from `.ns-ide/tools.json`) by id, in its
/// configured working directory, with an optional timeout and appended args.
fn run_external_tool(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    use std::time::Instant;
    let id = arg_str(args, "id")?;
    let store = ns_project::ToolStore::load(root);
    let tool = store
        .find(id)
        .ok_or_else(|| anyhow::anyhow!("外部工具不存在: {id}"))?;
    let extra_args = args
        .get("args")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let command = if extra_args.is_empty() {
        tool.command.clone()
    } else {
        format!("{} {}", tool.command, extra_args)
    };
    let timeout_ms = parse_timeout_ms(args);
    let cwd = if tool.cwd.trim().is_empty() {
        root.to_path_buf()
    } else {
        resolve_path(root, &tool.cwd)
    };
    let started = Instant::now();
    let output = shell_run_opt(&command, &cwd, timeout_ms)?;
    Ok(json!({
        "id": tool.id,
        "title": tool.title,
        "command": command,
        "cwd": cwd.display().to_string(),
        "success": output.status.success(),
        "code": output.status.code(),
        "stdout": String::from_utf8_lossy(&output.stdout).to_string(),
        "stderr": String::from_utf8_lossy(&output.stderr).to_string(),
        "duration_ms": started.elapsed().as_millis() as u64,
    }))
}

/// Parse the optional `timeout_ms` argument (number or numeric string), where
/// 0 / missing means "no timeout".
fn parse_timeout_ms(args: &HashMap<String, serde_json::Value>) -> Option<u64> {
    args.get("timeout_ms")
        .and_then(|v| {
            if let Some(s) = v.as_str() {
                s.trim().parse::<u64>().ok()
            } else {
                v.as_u64()
            }
        })
        .filter(|ms| *ms > 0)
}

/// Run a shell command line (`sh -c` / `cmd /C`) in `cwd`.
fn shell_run(command: &str, cwd: &Path) -> Result<std::process::Output> {
    #[cfg(target_os = "windows")]
    let (shell, flag) = ("cmd", "/C");
    #[cfg(not(target_os = "windows"))]
    let (shell, flag) = ("sh", "-c");
    std::process::Command::new(shell)
        .arg(flag)
        .arg(command)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("运行命令失败: {command}"))
}

/// Run a shell command with an optional timeout. When a timeout elapses the
/// child is killed and an error is returned, so a hung build/script cannot
/// block the MCP server indefinitely.
fn shell_run_opt(
    command: &str,
    cwd: &Path,
    timeout_ms: Option<u64>,
) -> Result<std::process::Output> {
    let Some(timeout_ms) = timeout_ms else {
        return shell_run(command, cwd);
    };
    use std::io::Read;
    #[cfg(target_os = "windows")]
    let (shell, flag) = ("cmd", "/C");
    #[cfg(not(target_os = "windows"))]
    let (shell, flag) = ("sh", "-c");
    let mut child = std::process::Command::new(shell)
        .arg(flag)
        .arg(command)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("运行命令失败: {command}"))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut stream) = stdout.take() {
            let _ = stream.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut stream) = stderr.take() {
            let _ = stream.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_handle.join();
                    let _ = stderr_handle.join();
                    bail!("命令超时（{} ms）: {command}", timeout_ms);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();
                return Err(anyhow::anyhow!("等待子进程失败: {error}"));
            }
        }
    };
    let out = stdout_handle.join().unwrap_or_default();
    let err = stderr_handle.join().unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout: out,
        stderr: err,
    })
}

// ── Language server (LSP) tools ──────────────────────────────────────────────

/// Live LSP session state: the running server plus the documents we have
/// opened (so a re-request after a disk edit triggers `didChange`).
struct LspSession {
    server: LspServer,
    opened: HashMap<PathBuf, String>,
    versions: HashMap<PathBuf, i32>,
}

fn lsp_sessions() -> &'static Mutex<HashMap<String, LspSession>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, LspSession>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lsp_request_timeout(args: &HashMap<String, serde_json::Value>) -> Duration {
    let ms = args
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .filter(|ms| *ms > 0)
        .unwrap_or(10_000);
    Duration::from_millis(ms)
}

/// Determine the language for an LSP call: explicit `language` arg wins,
/// then the project's main language, then the file extension.
fn language_for(
    args: &HashMap<String, serde_json::Value>,
    root: &Path,
    path: &Path,
) -> Result<Language> {
    if let Some(raw) = arg_str_opt(args, "language") {
        return raw
            .parse::<Language>()
            .map_err(|_| anyhow::anyhow!("未知语言: {raw}"));
    }
    if let Ok(project) = Project::open(root) {
        let language = project.config.project.language;
        if language != Language::Asm {
            return Ok(language);
        }
    }
    Language::from_path(path)
        .filter(|language| *language != Language::Asm)
        .ok_or_else(|| anyhow::anyhow!("无法从文件扩展名推断语言: {}", path.display()))
}

/// Get (or lazily start) the LSP session for `language`, returning a mutable
/// guard so the caller can `didOpen`/request against it.
fn with_lsp_session<R>(
    root: &Path,
    language: &Language,
    f: impl FnOnce(&mut LspSession) -> Result<R>,
) -> Result<R> {
    let key = language.as_str().to_string();
    let mut sessions = lsp_sessions().lock().unwrap_or_else(|e| e.into_inner());
    if !sessions.contains_key(&key) {
        let server = LspServer::spawn(language, root)
            .with_context(|| format!("启动 {} 语言服务器失败", language.as_str()))?;
        sessions.insert(
            key.clone(),
            LspSession {
                server,
                opened: HashMap::new(),
                versions: HashMap::new(),
            },
        );
    }
    let session = sessions.get_mut(&key).expect("just inserted");
    f(session)
}

/// Ensure the LSP server knows about `path`: `didOpen` on first access,
/// `didChange` when the on-disk content moved past our last-known copy.
fn lsp_ensure_open(session: &mut LspSession, path: &Path) -> Result<()> {
    let text = fs::read_to_string(path).with_context(|| format!("读取 {} 失败", path.display()))?;
    if let Some(previous) = session.opened.get(path) {
        if previous != &text {
            let version = session.versions.get(path).copied().unwrap_or(1) + 1;
            session
                .server
                .change_file(path.to_path_buf(), text.clone(), version)?;
            session.versions.insert(path.to_path_buf(), version);
            session.opened.insert(path.to_path_buf(), text);
        }
    } else {
        session.server.open_file(path.to_path_buf(), text.clone())?;
        session.versions.insert(path.to_path_buf(), 1);
        session.opened.insert(path.to_path_buf(), text);
    }
    Ok(())
}

fn lsp_session_start(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let path = root.join("main.c");
    let language = language_for(args, root, &path)?;
    let available = ns_lsp::server_available(&language);
    if !available {
        return Ok(json!({
            "available": false,
            "started": false,
            "language": language.as_str(),
            "message": format!("未找到语言服务器（{}）", language.as_str()),
        }));
    }
    let started = with_lsp_session(root, &language, |_| Ok(()))
        .map(|_| true)
        .unwrap_or(false);
    if !started {
        return Ok(json!({
            "available": true,
            "started": false,
            "language": language.as_str(),
            "message": "语言服务器启动失败（可能不支持当前环境）",
        }));
    }
    Ok(json!({
        "available": true,
        "started": true,
        "language": language.as_str(),
        "server": ns_lsp::server_for_language(&language),
    }))
}

fn lsp_session_end(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let path = root.join("main.c");
    let language = language_for(args, root, &path)?;
    let key = language.as_str().to_string();
    let mut sessions = lsp_sessions().lock().unwrap_or_else(|e| e.into_inner());
    let stopped = if let Some(session) = sessions.remove(&key) {
        session.server.shutdown();
        true
    } else {
        false
    };
    Ok(json!({
        "stopped": stopped,
        "language": language.as_str(),
        "active": sessions.len(),
    }))
}

fn lsp_hover(root: &Path, args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    let path = resolve_path(root, arg_str(args, "path")?);
    let language = language_for(args, root, &path)?;
    let line = arg_usize(args, "line")? - 1;
    let column = arg_usize(args, "column")? - 1;
    let timeout = lsp_request_timeout(args);
    let value = with_lsp_session(root, &language, |session| {
        lsp_ensure_open(session, &path)?;
        let receiver = session.server.hover(path.clone(), line, column)?;
        receiver
            .recv_timeout(timeout)
            .with_context(|| format!("悬停请求超时（{} ms）", timeout.as_millis()))
    })?;
    Ok(json!({
        "path": path.display().to_string(),
        "line": line + 1,
        "column": column + 1,
        "language": language.as_str(),
        "hover": parse_hover_text(&value),
    }))
}

fn lsp_definition(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let path = resolve_path(root, arg_str(args, "path")?);
    let language = language_for(args, root, &path)?;
    let line = arg_usize(args, "line")? - 1;
    let column = arg_usize(args, "column")? - 1;
    let timeout = lsp_request_timeout(args);
    let value = with_lsp_session(root, &language, |session| {
        lsp_ensure_open(session, &path)?;
        let receiver = session.server.definition(path.clone(), line, column)?;
        receiver
            .recv_timeout(timeout)
            .with_context(|| format!("定义请求超时（{} ms）", timeout.as_millis()))
    })?;
    let locations = parse_location_list(&value);
    Ok(json!({
        "path": path.display().to_string(),
        "line": line + 1,
        "column": column + 1,
        "language": language.as_str(),
        "locations": locations,
        "count": locations.len(),
    }))
}

fn lsp_references(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let path = resolve_path(root, arg_str(args, "path")?);
    let language = language_for(args, root, &path)?;
    let line = arg_usize(args, "line")? - 1;
    let column = arg_usize(args, "column")? - 1;
    let timeout = lsp_request_timeout(args);
    let value = with_lsp_session(root, &language, |session| {
        lsp_ensure_open(session, &path)?;
        let receiver = session.server.references(path.clone(), line, column)?;
        receiver
            .recv_timeout(timeout)
            .with_context(|| format!("引用请求超时（{} ms）", timeout.as_millis()))
    })?;
    let locations = parse_location_list(&value);
    Ok(json!({
        "path": path.display().to_string(),
        "line": line + 1,
        "column": column + 1,
        "language": language.as_str(),
        "locations": locations,
        "count": locations.len(),
    }))
}

fn lsp_completion(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let path = resolve_path(root, arg_str(args, "path")?);
    let language = language_for(args, root, &path)?;
    let line = arg_usize(args, "line")? - 1;
    let column = arg_usize(args, "column")? - 1;
    let timeout = lsp_request_timeout(args);
    let value = with_lsp_session(root, &language, |session| {
        lsp_ensure_open(session, &path)?;
        let receiver = session.server.completion(path.clone(), line, column)?;
        receiver
            .recv_timeout(timeout)
            .with_context(|| format!("补全请求超时（{} ms）", timeout.as_millis()))
    })?;
    let items = parse_completion_items(&value);
    Ok(json!({
        "path": path.display().to_string(),
        "line": line + 1,
        "column": column + 1,
        "language": language.as_str(),
        "items": items,
        "count": items.len(),
    }))
}

/// Extract the markdown/plaintext hover body from an LSP hover response.
fn parse_hover_text(value: &serde_json::Value) -> Option<String> {
    let contents = value.get("result")?.get("contents")?;
    if let Some(text) = contents.as_str() {
        return Some(text.to_string());
    }
    contents
        .as_object()
        .and_then(|object| object.get("value"))
        .and_then(|value| value.as_str())
        .map(|text| text.to_string())
}

/// Parse an LSP location (single object or array) into MCP-friendly entries.
fn parse_location_list(value: &serde_json::Value) -> Vec<serde_json::Value> {
    let result = value.get("result");
    let items: Vec<&serde_json::Value> = match result {
        Some(serde_json::Value::Array(array)) => array.iter().collect(),
        Some(object) => vec![object],
        None => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|item| {
            let uri = item.get("uri").and_then(|uri| uri.as_str())?;
            let range = item.get("range")?;
            let start = range.get("start")?;
            Some(json!({
                "uri": uri,
                "path": uri_to_path(uri),
                "line": start.get("line").and_then(|line| line.as_u64()).map(|line| line as usize + 1),
                "column": start.get("character").and_then(|ch| ch.as_u64()).map(|ch| ch as usize + 1),
            }))
        })
        .collect()
}

/// Best-effort conversion from a `file://` URI to a plain path.
fn uri_to_path(uri: &str) -> String {
    let rest = uri.strip_prefix("file://").unwrap_or(uri);
    rest.replace("%20", " ").replace("file://", "")
}

/// Parse LSP completion items (array or `{ items: [...] }`) into compact
/// MCP entries: label, insert text, kind and detail.
fn parse_completion_items(value: &serde_json::Value) -> Vec<serde_json::Value> {
    let items: Vec<&serde_json::Value> = match value.get("result") {
        Some(serde_json::Value::Array(array)) => array.iter().collect(),
        Some(serde_json::Value::Object(object)) => object
            .get("items")
            .and_then(|items| items.as_array())
            .map(|items| items.iter().collect())
            .unwrap_or_default(),
        _ => return Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|item| {
            let label = item.get("label").and_then(|label| label.as_str())?;
            let insert = item
                .get("textEdit")
                .and_then(|edit| edit.get("newText"))
                .and_then(|text| text.as_str())
                .or_else(|| item.get("insertText").and_then(|text| text.as_str()))
                .unwrap_or(label);
            Some(json!({
                "label": label,
                "insert_text": insert,
                "kind": item.get("kind").and_then(|kind| kind.as_u64()).unwrap_or(0),
                "detail": item.get("detail").and_then(|detail| detail.as_str()).unwrap_or(""),
            }))
        })
        .collect()
}

/// Parse a hex string (with optional `0x` prefix and whitespace) into bytes.
fn parse_hex_bytes(text: &str) -> Result<Vec<u8>> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let cleaned = cleaned.strip_prefix("0x").unwrap_or(&cleaned);
    if cleaned.len() % 2 != 0 {
        bail!("hex 长度必须为偶数");
    }
    let mut out = Vec::with_capacity(cleaned.len() / 2);
    for index in (0..cleaned.len()).step_by(2) {
        let byte = u8::from_str_radix(&cleaned[index..index + 2], 16)
            .with_context(|| format!("无效的十六进制: {}", &cleaned[index..index + 2]))?;
        out.push(byte);
    }
    Ok(out)
}

// ── Breakpoint / bookmark tools ──────────────────────────────────────────────

fn list_breakpoints(root: &Path) -> Result<serde_json::Value> {
    let store = BreakpointStore::load(root);
    Ok(json!({
        "breakpoints": store.all().iter().map(|b| json!({
            "path": b.path,
            "line": b.line,
            "enabled": b.enabled,
            "kind": b.kind.label(),
            "condition": b.condition,
            "hit_count": b.hit_count,
            "log_message": b.log_message,
            "state": b.state_label(),
            "verified": b.verified,
            "reason": b.reason,
            "hits": b.hits,
        })).collect::<Vec<_>>(),
        "watchpoints": store.watchpoints().iter().map(|w| json!({
            "expression": w.expression,
            "access": w.access.label(),
            "enabled": w.enabled,
            "state": w.state_label(),
        })).collect::<Vec<_>>(),
        "enabled_count": store.enabled_count(),
        "count": store.len(),
    }))
}

fn add_breakpoint(root: &Path, path: &str, line: usize) -> Result<serde_json::Value> {
    if line == 0 {
        bail!("行号必须大于 0");
    }
    let mut store = BreakpointStore::load(root);
    store.set(path, line, true);
    store.save(root)?;
    Ok(json!({
        "added": true,
        "path": path,
        "line": line,
        "count": store.len(),
    }))
}

fn remove_breakpoint(root: &Path, path: &str, line: usize) -> Result<serde_json::Value> {
    let mut store = BreakpointStore::load(root);
    let removed = store.remove(path, line);
    store.save(root)?;
    Ok(json!({
        "removed": removed,
        "path": path,
        "line": line,
        "count": store.len(),
    }))
}

/// Update a breakpoint's editable properties. Each optional field is applied
/// only when present, so an LLM can set a condition, hit-count limit, log
/// message, hardware kind or enabled state in one call.
fn update_breakpoint(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    use ns_project::BreakpointKind;
    let path = arg_str(args, "path")?;
    let line = arg_usize(args, "line")?;
    if line == 0 {
        bail!("行号必须大于 0");
    }
    let mut store = BreakpointStore::load(root);
    let mut changed = false;
    if let Some(enabled) = args.get("enabled").and_then(|value| value.as_bool()) {
        changed |= store.set(path, line, enabled);
    }
    // Property edits need an existing breakpoint; create one if absent.
    let exists = store
        .all()
        .iter()
        .any(|breakpoint| breakpoint.path == path && breakpoint.line == line);
    if !exists
        && (args.get("condition").is_some()
            || args.get("hit_count").is_some()
            || args.get("log_message").is_some()
            || args.get("hardware").is_some())
    {
        store.set(path, line, true);
    }
    if let Some(condition) = args.get("condition").and_then(|value| value.as_str()) {
        changed |= store.set_condition(path, line, condition);
    }
    if let Some(hit_count) = args.get("hit_count").and_then(|value| value.as_str()) {
        changed |= store.set_hit_count(path, line, hit_count);
    }
    if let Some(message) = args.get("log_message").and_then(|value| value.as_str()) {
        changed |= store.set_log_message(path, line, message);
    }
    if let Some(hardware) = args.get("hardware").and_then(|value| value.as_bool()) {
        let current = store
            .all()
            .iter()
            .find(|breakpoint| breakpoint.path == path && breakpoint.line == line)
            .map(|breakpoint| breakpoint.kind == BreakpointKind::Hardware);
        if let Some(current) = current {
            if current != hardware {
                changed |= store.set_hardware(path, line).is_some();
            }
        }
    }
    if changed {
        store.save(root)?;
    }
    let breakpoint = store
        .all()
        .iter()
        .find(|breakpoint| breakpoint.path == path && breakpoint.line == line);
    Ok(json!({
        "updated": changed,
        "breakpoint": breakpoint.map(|breakpoint| json!({
            "path": breakpoint.path,
            "line": breakpoint.line,
            "enabled": breakpoint.enabled,
            "kind": breakpoint.kind.label(),
            "condition": breakpoint.condition,
            "hit_count": breakpoint.hit_count,
            "log_message": breakpoint.log_message,
            "hardware": breakpoint.kind == BreakpointKind::Hardware,
            "verified": breakpoint.verified,
            "reason": breakpoint.reason,
            "hits": breakpoint.hits,
        })).unwrap_or(serde_json::Value::Null),
        "count": store.len(),
    }))
}

fn add_watchpoint(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    use ns_project::WatchAccess;
    let expression = arg_str(args, "expression")?;
    let access = match args
        .get("access")
        .and_then(|value| value.as_str())
        .unwrap_or("write")
    {
        "read" => WatchAccess::Read,
        "write" => WatchAccess::Write,
        "readwrite" | "read_write" => WatchAccess::ReadWrite,
        other => bail!("未知观察点访问类型: {other}（可选: read, write, readwrite）"),
    };
    let mut store = BreakpointStore::load(root);
    let added = store.add_watchpoint(expression, access);
    store.save(root)?;
    Ok(json!({
        "added": added,
        "expression": expression,
        "access": access.label(),
        "count": store.watchpoints().len(),
    }))
}

fn remove_watchpoint(root: &Path, expression: &str) -> Result<serde_json::Value> {
    let mut store = BreakpointStore::load(root);
    let removed = store.remove_watchpoint(expression);
    store.save(root)?;
    Ok(json!({
        "removed": removed,
        "expression": expression,
        "count": store.watchpoints().len(),
    }))
}

fn set_watchpoint_enabled(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let expression = arg_str(args, "expression")?;
    let enabled = args
        .get("enabled")
        .and_then(|value| value.as_bool())
        .ok_or_else(|| anyhow::anyhow!("missing required argument: enabled"))?;
    let mut store = BreakpointStore::load(root);
    let updated = store.set_watchpoint_enabled(expression, enabled);
    store.save(root)?;
    Ok(json!({
        "updated": updated,
        "expression": expression,
        "enabled": enabled,
        "count": store.watchpoints().len(),
    }))
}

/// List the user-defined external tool commands persisted in `.ns-ide/tools.json`.
fn list_external_tools(root: &Path) -> Result<serde_json::Value> {
    let store = ns_project::ToolStore::load(root);
    Ok(json!({
        "count": store.len(),
        "tools": store.tools().iter().map(|tool| json!({
            "id": tool.id,
            "title": tool.title,
            "command": tool.command,
            "cwd": tool.cwd,
        })).collect::<Vec<_>>(),
    }))
}

/// Add or replace an external tool command by id (a missing id is derived from
/// the title and made unique). Returns `added = true` when a new tool was
/// created, `false` when an existing one was replaced.
fn add_external_tool(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let title = arg_str(args, "title")?;
    let command = arg_str(args, "command")?;
    let cwd = arg_str_opt(args, "cwd").unwrap_or_default();
    let mut store = ns_project::ToolStore::load(root);
    let mut id = arg_str_opt(args, "id").unwrap_or_default();
    if id.trim().is_empty() {
        let base = slugify(title);
        let base = if base.is_empty() {
            "tool".to_string()
        } else {
            base
        };
        id = base.clone();
        let mut suffix = 1;
        while store.find(&id).is_some() {
            id = format!("{base}-{suffix}");
            suffix += 1;
        }
    }
    let tool = ns_project::ToolCommand {
        id: id.clone(),
        title: title.to_string(),
        command: command.to_string(),
        cwd: cwd.clone(),
    };
    let added = store.upsert(tool);
    store.save(root)?;
    Ok(json!({
        "added": added,
        "id": id,
        "title": title,
        "command": command,
        "cwd": cwd,
        "count": store.len(),
    }))
}

fn remove_external_tool(root: &Path, id: &str) -> Result<serde_json::Value> {
    let mut store = ns_project::ToolStore::load(root);
    let removed = store.remove(id);
    store.save(root)?;
    Ok(json!({
        "removed": removed,
        "id": id,
        "count": store.len(),
    }))
}

/// Convert a title into a filesystem-friendly id: lowercase, collapse runs of
/// non-alphanumerics to a single `-`, and trim surrounding separators.
fn slugify(text: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() {
            pending_dash = true;
        }
    }
    out
}

fn export_breakpoints(
    root: &Path,
    args: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let store = BreakpointStore::load(root);
    let out = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(|p| resolve_path(root, p))
        .unwrap_or_else(|| root.join(".ns-ide").join("breakpoints-export.json"));
    let count = store.export_to(&out)?;
    Ok(json!({
        "exported": count,
        "path": out.display().to_string(),
    }))
}

fn list_bookmarks(root: &Path) -> Result<serde_json::Value> {
    let store = BookmarkStore::load(root);
    Ok(json!({
        "bookmarks": store.all().iter().map(|b| json!({
            "path": b.path,
            "line": b.line,
            "label": b.label,
            "seq": b.seq,
        })).collect::<Vec<_>>(),
        "count": store.len(),
    }))
}

fn toggle_bookmark(root: &Path, path: &str, line: usize) -> Result<serde_json::Value> {
    if line == 0 {
        bail!("行号必须大于 0");
    }
    let mut store = BookmarkStore::load(root);
    let added = store.toggle(path, line);
    store.save(root)?;
    Ok(json!({
        "added": added,
        "path": path,
        "line": line,
        "count": store.len(),
    }))
}

// ── Toolchain / debug tools ──────────────────────────────────────────────────

fn check_toolchain() -> Result<serde_json::Value> {
    let names = [
        "arm-none-eabi-gcc",
        "arm-none-eabi-g++",
        "arm-none-eabi-gdb",
        "gcc",
        "g++",
        "clang",
        "clang++",
        "cargo",
        "rustc",
        "zig",
        "zr",
        "gdb",
        "lldb",
        "lldb-dap",
        "openocd",
        "pyocd",
        "JLinkExe",
        "st-flash",
        "probe-rs",
        "nm",
        "size",
    ];
    let tools = ns_project::toolchain::tools(&names);
    Ok(json!({
        "tools": tools.iter().map(|t| json!({
            "name": t.name,
            "available": t.available(),
            "path": t.path.as_ref().map(|p| p.display().to_string()),
            "version": t.version(),
            "install_hint": (!t.available()).then(|| missing_guide(&t.name)),
        })).collect::<Vec<_>>(),
    }))
}

fn debug_adapter_status() -> Result<serde_json::Value> {
    let adapter = ns_debug::find_dap_adapter();
    Ok(json!({
        "installed": adapter.is_some(),
        "path": adapter.map(|p| p.display().to_string()),
    }))
}

// ── Recents / misc tools ─────────────────────────────────────────────────────

fn read_recent_projects() -> Result<serde_json::Value> {
    let recents = load_recents();
    Ok(json!({
        "projects": recents.iter().map(|r| json!({
            "path": r.path.display().to_string(),
            "name": r.name,
            "opened_at": r.opened_at,
        })).collect::<Vec<_>>(),
        "count": recents.len(),
    }))
}

// ── Git tools ────────────────────────────────────────────────────────────────

/// Run `git` with `args` in `root`, returning stdout with trailing newline
/// trimmed. Mirrors the IDE git helper so MCP matches its behavior.
fn git_run(root: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("无法运行 git {}（请确认 git 已安装）", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "git {} 失败: {}",
            args.join(" "),
            if stderr.is_empty() {
                "未知错误"
            } else {
                &stderr
            }
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(stdout.trim_end_matches('\n').to_string())
}

fn is_git_repo(root: &Path) -> bool {
    git_run(root, &["rev-parse", "--is-inside-work-tree"])
        .map(|v| v == "true")
        .unwrap_or(false)
}

fn git_status(root: &Path) -> Result<serde_json::Value> {
    if !is_git_repo(root) {
        return Ok(json!({"in_repo": false, "branch": "", "changes": 0, "files": []}));
    }
    let branch = git_run(root, &["branch", "--show-current"])
        .ok()
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "HEAD".to_string());
    let porcelain = git_run(root, &["status", "--porcelain"]).unwrap_or_default();
    let mut files = Vec::new();
    for line in porcelain.lines() {
        if line.len() < 3 {
            continue;
        }
        let staged = line.as_bytes()[0] as char;
        let worktree = line.as_bytes()[1] as char;
        let path = line[3..]
            .split(" -> ")
            .last()
            .unwrap_or(&line[3..])
            .to_string();
        files.push(json!({
            "path": path,
            "staged": staged.to_string(),
            "worktree": worktree.to_string(),
            "untracked": worktree == '?',
            "deleted": staged == 'D' || worktree == 'D',
            "label": if worktree == '?' { "??".to_string() } else if staged != ' ' { staged.to_string() } else { worktree.to_string() },
        }));
    }
    Ok(json!({
        "in_repo": true,
        "branch": branch,
        "changes": files.len(),
        "files": files,
    }))
}

fn git_diff(root: &Path, path: Option<&str>) -> Result<serde_json::Value> {
    if !is_git_repo(root) {
        bail!("目录不是 Git 仓库: {}", root.display());
    }
    let mut args: Vec<&str> = vec!["diff", "--no-color"];
    let mut staged = false;
    if let Some(p) = path {
        let p = p.trim();
        if p == "--cached" {
            staged = true;
            args = vec!["diff", "--no-color", "--cached"];
        } else if !p.is_empty() {
            args.push(p);
        }
    }
    let diff = git_run(root, &args).unwrap_or_default();
    let staged_diff = if staged {
        diff.clone()
    } else {
        git_run(root, &["diff", "--no-color", "--cached"]).unwrap_or_default()
    };
    Ok(json!({
        "path": path.unwrap_or(""),
        "diff": diff,
        "staged_diff": staged_diff,
        "bytes": diff.len(),
    }))
}

fn git_stage(root: &Path, path: Option<&str>) -> Result<serde_json::Value> {
    if !is_git_repo(root) {
        bail!("目录不是 Git 仓库: {}", root.display());
    }
    let p = path.map(str::trim).filter(|p| !p.is_empty());
    let args: Vec<&str> = if let Some(p) = p {
        vec!["add", "--", p]
    } else {
        vec!["add", "-A"]
    };
    git_run(root, &args)?;
    Ok(json!({"staged": true, "path": p.unwrap_or("")}))
}

fn git_unstage(root: &Path, path: Option<&str>) -> Result<serde_json::Value> {
    if !is_git_repo(root) {
        bail!("目录不是 Git 仓库: {}", root.display());
    }
    let p = path.map(str::trim).filter(|p| !p.is_empty());
    let args: Vec<&str> = if let Some(p) = p {
        vec!["reset", "HEAD", "--", p]
    } else {
        vec!["reset", "HEAD", "--", "."]
    };
    let result = git_run(root, &args);
    // On an unborn branch `reset HEAD` fails; treat it as a no-op unstaged state.
    match result {
        Ok(_) => {}
        Err(_) if git_run(root, &["rev-parse", "--verify", "HEAD"]).is_err() => {}
        Err(e) => return Err(e),
    }
    Ok(json!({"unstaged": true, "path": p.unwrap_or("")}))
}

fn git_commit(root: &Path, message: &str) -> Result<serde_json::Value> {
    if !is_git_repo(root) {
        bail!("目录不是 Git 仓库: {}", root.display());
    }
    if message.trim().is_empty() {
        bail!("提交信息不能为空");
    }
    let output = std::process::Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(root)
        .output()
        .with_context(|| "无法运行 git commit")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "git commit 失败: {}",
            if stderr.is_empty() {
                "无暂存变更"
            } else {
                &stderr
            }
        );
    }
    Ok(json!({"committed": true, "message": message}))
}

fn git_log(root: &Path, args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    if !is_git_repo(root) {
        bail!("目录不是 Git 仓库: {}", root.display());
    }
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 200);
    let format = "%H%x1f%h%x1f%s%x1f%an%x1f%aI";
    let limit_str = limit.to_string();
    let pretty = format!("--pretty=format:{format}");
    let raw = git_run(root, &["log", "-n", &limit_str, &pretty]).unwrap_or_default();
    let commits = raw
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let parts: Vec<&str> = l.split('\x1f').collect();
            json!({
                "hash": parts.first().copied().unwrap_or(""),
                "short": parts.get(1).copied().unwrap_or(""),
                "subject": parts.get(2).copied().unwrap_or(""),
                "author": parts.get(3).copied().unwrap_or(""),
                "date": parts.get(4).copied().unwrap_or(""),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "branch": git_run(root, &["branch", "--show-current"]).unwrap_or_default(),
        "commits": commits,
        "count": commits.len(),
    }))
}

fn git_branches(root: &Path) -> Result<serde_json::Value> {
    if !is_git_repo(root) {
        bail!("目录不是 Git 仓库: {}", root.display());
    }
    let current = git_run(root, &["branch", "--show-current"]).unwrap_or_default();
    let raw = git_run(root, &["branch", "--format=%(refname:short)"]).unwrap_or_default();
    let branches: Vec<serde_json::Value> = raw
        .lines()
        .filter(|b| !b.is_empty() && *b != "HEAD")
        .map(|b| json!({"name": b, "current": b == current}))
        .collect();
    Ok(json!({
        "current": current,
        "branches": branches,
        "count": branches.len(),
    }))
}

fn git_checkout(root: &Path, branch: &str) -> Result<serde_json::Value> {
    if !is_git_repo(root) {
        bail!("目录不是 Git 仓库: {}", root.display());
    }
    if branch.trim().is_empty() {
        bail!("分支名不能为空");
    }
    git_run(root, &["checkout", branch])?;
    Ok(json!({"switched": true, "branch": branch}))
}

fn git_create_branch(root: &Path, branch: &str) -> Result<serde_json::Value> {
    if !is_git_repo(root) {
        bail!("目录不是 Git 仓库: {}", root.display());
    }
    if branch.trim().is_empty() {
        bail!("分支名不能为空");
    }
    git_run(root, &["checkout", "-b", branch])?;
    Ok(json!({"created": true, "branch": branch}))
}

fn git_pull(root: &Path) -> Result<serde_json::Value> {
    if !is_git_repo(root) {
        bail!("目录不是 Git 仓库: {}", root.display());
    }
    let output = std::process::Command::new("git")
        .args(["pull"])
        .current_dir(root)
        .output()
        .with_context(|| "无法运行 git pull")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "git pull 失败: {}",
            if stderr.is_empty() {
                "无上游分支"
            } else {
                &stderr
            }
        );
    }
    Ok(
        json!({"pulled": true, "output": String::from_utf8_lossy(&output.stdout).trim().to_string()}),
    )
}

fn git_push(root: &Path) -> Result<serde_json::Value> {
    if !is_git_repo(root) {
        bail!("目录不是 Git 仓库: {}", root.display());
    }
    let output = std::process::Command::new("git")
        .args(["push"])
        .current_dir(root)
        .output()
        .with_context(|| "无法运行 git push")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "git push 失败: {}",
            if stderr.is_empty() {
                "无上游分支"
            } else {
                &stderr
            }
        );
    }
    Ok(
        json!({"pushed": true, "output": String::from_utf8_lossy(&output.stdout).trim().to_string()}),
    )
}

// ── Import / toolchain tools ─────────────────────────────────────────────────

fn import_project(args: &HashMap<String, serde_json::Value>) -> Result<serde_json::Value> {
    let path = Path::new(arg_str(args, "path")?);
    let imported = ns_project::parse_import(path)?;
    // Write project.toml into the target directory (the vendor file's parent
    // unless an explicit location is given).
    let target_dir = arg_str_opt(args, "location")
        .map(PathBuf::from)
        .unwrap_or_else(|| path.parent().map(Path::to_path_buf).unwrap_or_default());
    fs::create_dir_all(&target_dir)
        .with_context(|| format!("创建目录 {} 失败", target_dir.display()))?;
    let config = &imported.config;
    let toml_text = toml::to_string_pretty(config).context("序列化 project.toml 失败")?;
    fs::write(target_dir.join("project.toml"), toml_text)
        .with_context(|| format!("写入 project.toml 到 {} 失败", target_dir.display()))?;
    // Copy discovered sources into src/ preserving their file name.
    let src_dir = target_dir.join("src");
    fs::create_dir_all(&src_dir).with_context(|| format!("创建目录 {} 失败", src_dir.display()))?;
    let mut copied = 0;
    for source in &imported.sources {
        if let Some(name) = source.file_name() {
            if let Ok(bytes) = fs::read(source) {
                if fs::write(src_dir.join(name), bytes).is_ok() {
                    copied += 1;
                }
            }
        }
    }
    Ok(json!({
        "imported": true,
        "path": path.display().to_string(),
        "target_dir": target_dir.display().to_string(),
        "language": config.project.language.as_str(),
        "target": config.project.target,
        "compiler": config.project.compiler,
        "sources": imported.sources.iter().map(|s| s.display().to_string()).collect::<Vec<_>>(),
        "sources_copied": copied,
        "notes": imported.notes,
    }))
}

fn get_toolchain_config() -> Result<serde_json::Value> {
    let config = ns_project::toolchain_config::ToolchainConfig::load();
    Ok(json!({
        "overrides": config.overrides,
        "count": config.overrides.len(),
    }))
}

fn set_toolchain_override(tool: &str, path: &str) -> Result<serde_json::Value> {
    let mut config = ns_project::toolchain_config::ToolchainConfig::load();
    config.set_override(tool, path);
    config.save()?;
    Ok(json!({
        "tool": tool,
        "path": if path.trim().is_empty() { "" } else { path },
        "cleared": path.trim().is_empty(),
        "saved": true,
    }))
}

fn list_toolchain_registry(language: &str) -> Result<serde_json::Value> {
    let language: Language = language.parse().map_err(|_| {
        anyhow::anyhow!("不支持的语言: {}（可选: C, C++, Rust, Zig, ZR）", language)
    })?;
    let config = ns_project::toolchain_config::ToolchainConfig::load();
    let tools = ns_project::toolchain_config::registry(&config, language);
    Ok(json!({
        "language": language.as_str(),
        "tools": tools.iter().map(|t| json!({
            "name": t.name,
            "available": t.available(),
            "path": t.path.as_ref().map(|p| p.display().to_string()),
            "version": t.version(),
        })).collect::<Vec<_>>(),
    }))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("ns-mcp-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_project(root: &Path) -> Project {
        let spec = NewProjectSpec {
            name: "blinky".to_string(),
            location: root.to_path_buf(),
            language: Language::C,
            compiler: "gcc".to_string(),
            simulator: "ns-sim".to_string(),
            debugger: "gdb".to_string(),
            flasher: "openocd".to_string(),
            target: "stm32f103c8".to_string(),
            svd: "stm32f103c8".to_string(),
            c_template: Some(CTemplate::Empty),
        };
        Project::create(spec).unwrap()
    }

    #[test]
    fn tool_registry_has_all_tools() {
        // Every registered name must have a schema; the registry and the
        // TOOL_NAMES table must stay in lockstep.
        assert!(TOOL_NAMES.len() >= 52);
        let listed = list_all();
        assert_eq!(listed.len(), TOOL_NAMES.len());
        for tool in &listed {
            assert!(tool["name"].is_string());
            assert!(tool["description"].is_string());
            assert!(tool["inputSchema"].is_object());
        }
    }

    #[test]
    fn every_registered_tool_has_unique_name_and_schema() {
        let listed = list_all();
        let mut names = std::collections::HashSet::new();
        for tool in &listed {
            let name = tool["name"].as_str().unwrap();
            assert!(names.insert(name), "duplicate tool: {name}");
            assert!(TOOL_NAMES.contains(&name), "unregistered tool: {name}");
        }
    }

    #[test]
    fn project_info_reports_created_project() {
        let dir = temp_dir("info");
        let project = sample_project(&dir);
        let json = project_info(&project.root).unwrap();
        assert_eq!(json["name"], "blinky");
        assert_eq!(json["language"], "C");
        assert_eq!(json["target"], "stm32f103c8");
        assert!(json["root"].as_str().unwrap().contains("ns-mcp-info"));
    }

    #[test]
    fn list_devices_returns_catalog() {
        let json = list_devices();
        assert!(json["devices"].as_array().unwrap().len() >= 4);
        assert!(json["devices"][0]["key"].is_string());
    }

    #[test]
    fn create_project_writes_skeleton() {
        let dir = temp_dir("create");
        let args = HashMap::from([
            ("name".to_string(), json!("led")),
            ("location".to_string(), json!(dir.display().to_string())),
            ("language".to_string(), json!("C")),
            ("target".to_string(), json!("stm32f103c8")),
        ]);
        let json = create_project(&args).unwrap();
        assert_eq!(json["created"], true);
        let project_root = PathBuf::from(json["project"]["root"].as_str().unwrap());
        assert!(project_root.join("project.toml").exists());
        assert!(project_root.join("src/main.c").exists());
    }

    #[test]
    fn read_write_file_roundtrip() {
        let dir = temp_dir("file");
        let project = sample_project(&dir);
        write_file(&project.root, "notes/hello.txt", "hello world").unwrap();
        let json = read_file(&project.root, "notes/hello.txt").unwrap();
        assert_eq!(json["content"], "hello world");
        assert_eq!(json["lines"], 1);
    }

    #[test]
    fn read_binary_returns_hex_and_ascii() {
        let dir = temp_dir("binary");
        let project = sample_project(&dir);
        write_file(&project.root, "data.bin", "ABC").unwrap();
        let json = read_binary(&project.root, "data.bin", 0, 3).unwrap();
        assert_eq!(json["hex"], "41 42 43");
        assert_eq!(json["ascii"], "ABC");
        assert_eq!(json["total"], 3);
    }

    #[test]
    fn search_files_finds_by_name() {
        let dir = temp_dir("search");
        let project = sample_project(&dir);
        write_file(&project.root, "src/util.c", "int util(void){}").unwrap();
        write_file(&project.root, "include/util.h", "#pragma once").unwrap();
        let json = search_files(&project.root, "util").unwrap();
        assert!(json["count"].as_u64().unwrap() >= 2);
    }

    #[test]
    fn read_file_lines_returns_range_and_clamps() {
        let dir = temp_dir("readlines");
        let project = sample_project(&dir);
        write_file(&project.root, "src/a.txt", "l1\nl2\nl3\nl4\nl5\n").unwrap();
        let json = read_file_lines(&project.root, "src/a.txt", 2, 2).unwrap();
        assert_eq!(json["count"], 2);
        assert_eq!(json["start"], 2);
        assert_eq!(json["total_lines"], 5);
        assert_eq!(json["truncated"], true);
        assert_eq!(json["lines"][0], "l2");
        assert_eq!(json["lines"][1], "l3");

        let json = read_file_lines(&project.root, "src/a.txt", 99, 5).unwrap();
        assert_eq!(json["count"], 0);
        assert_eq!(json["total_lines"], 5);

        let json = read_file_lines(&project.root, "src/a.txt", 0, 3).unwrap();
        assert_eq!(json["start"], 1);
        assert_eq!(json["lines"][0], "l1");
        assert_eq!(json["truncated"], true);
    }

    #[test]
    fn replace_in_files_replaces_and_respects_options() {
        let dir = temp_dir("replace");
        let project = sample_project(&dir);
        write_file(
            &project.root,
            "src/main.c",
            "int foo(){return FOO;}\nint FOO;",
        )
        .unwrap();
        write_file(&project.root, "src/other.c", "int bar;").unwrap();

        // Case-insensitive substring replace touches foo and both FOO.
        let json = replace_in_files_tool(
            &project.root,
            &HashMap::from([
                ("from".to_string(), json!("foo")),
                ("to".to_string(), json!("BAR")),
            ]),
        )
        .unwrap();
        assert_eq!(json["files_changed"], 1);
        assert_eq!(json["occurrences"], 3);
        assert_eq!(json["files"][0]["replacements"], 3);
        let text = fs::read_to_string(project.root.join("src/main.c")).unwrap();
        assert!(text.contains("BAR"));
        assert!(!text.contains("foo"));
        assert!(!text.contains("FOO"));

        // Whole-word: `foo` matches only standalone `foo` (case-insensitive).
        write_file(&project.root, "src/main.c", "foo foobar x_foo foo").unwrap();
        let json = replace_in_files_tool(
            &project.root,
            &HashMap::from([
                ("from".to_string(), json!("foo")),
                ("to".to_string(), json!("BAR")),
                ("whole_word".to_string(), json!(true)),
            ]),
        )
        .unwrap();
        assert_eq!(json["occurrences"], 2);
        let text = fs::read_to_string(project.root.join("src/main.c")).unwrap();
        assert_eq!(text, "BAR foobar x_foo BAR");

        // Dry-run reports without writing.
        let json = replace_in_files_tool(
            &project.root,
            &HashMap::from([
                ("from".to_string(), json!("BAR")),
                ("to".to_string(), json!("QUX")),
                ("case_sensitive".to_string(), json!(true)),
                ("dry_run".to_string(), json!(true)),
            ]),
        )
        .unwrap();
        assert_eq!(json["occurrences"], 2);
        assert_eq!(json["files_changed"], 1);
        assert_eq!(json["dry_run"], true);
        let text = fs::read_to_string(project.root.join("src/main.c")).unwrap();
        assert_eq!(text, "BAR foobar x_foo BAR");

        // Path filter restricts to matching files.
        write_file(&project.root, "src/keep.c", "zzz").unwrap();
        write_file(&project.root, "src/drop.c", "zzz").unwrap();
        let json = replace_in_files_tool(
            &project.root,
            &HashMap::from([
                ("from".to_string(), json!("zzz")),
                ("to".to_string(), json!("ok")),
                ("path".to_string(), json!("keep.c")),
            ]),
        )
        .unwrap();
        assert_eq!(json["files_changed"], 1);
        assert_eq!(json["files"][0]["path"], "src/keep.c");

        // Empty `from` is rejected.
        let err = replace_in_files_tool(
            &project.root,
            &HashMap::from([
                ("from".to_string(), json!("  ")),
                ("to".to_string(), json!("x")),
            ]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("不能为空"));
    }

    #[test]
    fn diff_text_reports_added_removed_and_hunks() {
        // Identical.
        let json = diff_text_tool(&HashMap::from([
            ("left".to_string(), json!("a\nb\n")),
            ("right".to_string(), json!("a\nb\n")),
        ]))
        .unwrap();
        assert_eq!(json["identical"], true);
        assert_eq!(json["added"], 0);
        assert_eq!(json["removed"], 0);
        assert_eq!(json["hunks"], 0);

        // Add + remove with a context line.
        let json = diff_text_tool(&HashMap::from([
            ("left".to_string(), json!("a\nb\nc\n")),
            ("right".to_string(), json!("a\nB\nc\nd\n")),
        ]))
        .unwrap();
        assert_eq!(json["identical"], false);
        assert_eq!(json["added"], 2); // B replaced b, plus new d
        assert_eq!(json["removed"], 1);
        assert_eq!(json["hunks"], 1);
        let diff = json["diff"].as_array().unwrap();
        assert_eq!(diff[0], "--- left");
        assert_eq!(diff[1], "+++ right");
        assert!(diff.iter().any(|l| l == "-b"));
        assert!(diff.iter().any(|l| l == "+B"));
        assert!(diff.iter().any(|l| l == "+d"));

        // Separate hunks are split (no overlap) when far apart.
        let mut left = String::new();
        let mut right = String::new();
        for i in 0..20 {
            if i == 2 {
                left.push_str("OLD\n");
                right.push_str("NEW\n");
            } else {
                let line = format!("l{i}\n");
                left.push_str(&line);
                right.push_str(&line);
            }
        }
        let json = diff_text_tool(&HashMap::from([
            ("left".to_string(), json!(left)),
            ("right".to_string(), json!(right)),
        ]))
        .unwrap();
        assert_eq!(json["hunks"], 1);
        assert_eq!(json["added"], 1);
        assert_eq!(json["removed"], 1);
    }

    #[test]
    fn list_files_returns_sources() {
        let dir = temp_dir("listfiles");
        let project = sample_project(&dir);
        let json = list_files(&project.root).unwrap();
        let files = json["files"].as_array().unwrap();
        assert!(
            files
                .iter()
                .any(|f| f.as_str().unwrap().ends_with("main.c"))
        );
    }

    #[test]
    fn toggle_build_config_flips_debug_release() {
        let dir = temp_dir("toggle");
        let project = sample_project(&dir);
        let json = toggle_build_config(&project.root).unwrap();
        assert_eq!(json["previous"], "debug");
        assert_eq!(json["current"], "release");
        let reopened = Project::open(&project.root).unwrap();
        assert_eq!(reopened.build_config(), "release");
    }

    #[test]
    fn write_project_field_updates_toml() {
        let dir = temp_dir("field");
        let project = sample_project(&dir);
        write_project_field(&project.root, "project.version", "0.2.0").unwrap();
        let reopened = Project::open(&project.root).unwrap();
        assert_eq!(reopened.config.project.version, "0.2.0");
    }

    #[test]
    fn decode_number_handles_all_bases() {
        let json = decode_number("0x10").unwrap();
        assert_eq!(json["decimal"], "16");
        assert_eq!(json["binary"], "0b10000");
        let json = decode_number("0b101").unwrap();
        assert_eq!(json["decimal"], "5");
        assert_eq!(json["hex"], "0x5");
        let json = decode_number("42").unwrap();
        assert_eq!(json["decimal"], "42");
    }

    #[test]
    fn get_memory_map_returns_regions() {
        let json = get_memory_map("stm32f103c8").unwrap();
        assert!(json["regions"].as_array().unwrap().len() >= 2);
    }

    #[test]
    fn get_peripherals_lists_gpio() {
        let json = get_peripherals("stm32f103c8").unwrap();
        let names = json["peripherals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert!(names.contains(&"GPIOA".to_string()));
    }

    #[test]
    fn get_register_details_returns_registers() {
        let json = get_register_details("stm32f103c8", "GPIOA").unwrap();
        assert!(json["registers"].as_array().unwrap().len() >= 3);
    }

    #[test]
    fn generate_linker_script_and_startup_file() {
        assert!(generate_linker_script("stm32f103c8").unwrap()["content"].is_string());
        assert!(get_startup_file("stm32f103c8").unwrap()["content"].is_string());
    }

    #[test]
    fn list_catalog_devices_counts() {
        let json = list_catalog_devices().unwrap();
        assert!(json["count"].as_u64().unwrap() >= 4);
    }

    #[test]
    fn compute_artifact_path_returns_elf() {
        let dir = temp_dir("artifact");
        let project = sample_project(&dir);
        let json = compute_artifact_path(&project.root).unwrap();
        assert!(json["path"].as_str().unwrap().ends_with(".elf"));
    }

    #[test]
    fn get_project_issues_reports_validation() {
        let dir = temp_dir("issues");
        let project = sample_project(&dir);
        let json = get_project_issues(&project.root).unwrap();
        assert!(json["issues"].is_array());
    }

    #[test]
    fn get_workspace_symbols_extracts_functions() {
        let dir = temp_dir("symbols");
        let project = sample_project(&dir);
        write_file(
            &project.root,
            "src/main.c",
            "int main(void) { return 0; }\nvoid helper(void) {}\n",
        )
        .unwrap();
        let json = get_workspace_symbols(&project.root).unwrap();
        let names = json["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert!(names.contains(&"main".to_string()));
        assert!(names.contains(&"helper".to_string()));
    }

    #[test]
    fn breakpoint_add_list_remove_roundtrip() {
        let dir = temp_dir("bp");
        let project = sample_project(&dir);
        add_breakpoint(&project.root, "src/main.c", 3).unwrap();
        let json = list_breakpoints(&project.root).unwrap();
        assert_eq!(json["count"], 1);
        assert_eq!(json["breakpoints"][0]["line"], 3);
        let json = remove_breakpoint(&project.root, "src/main.c", 3).unwrap();
        assert_eq!(json["removed"], true);
        assert_eq!(list_breakpoints(&project.root).unwrap()["count"], 0);
    }

    #[test]
    fn bookmark_toggle_persists() {
        let dir = temp_dir("bm");
        let project = sample_project(&dir);
        let json = toggle_bookmark(&project.root, "src/main.c", 10).unwrap();
        assert_eq!(json["added"], true);
        assert_eq!(list_bookmarks(&project.root).unwrap()["count"], 1);
        let json = toggle_bookmark(&project.root, "src/main.c", 10).unwrap();
        assert_eq!(json["added"], false);
        assert_eq!(list_bookmarks(&project.root).unwrap()["count"], 0);
    }

    #[test]
    fn export_breakpoints_writes_file() {
        let dir = temp_dir("export");
        let project = sample_project(&dir);
        add_breakpoint(&project.root, "src/main.c", 5).unwrap();
        add_breakpoint(&project.root, "src/main.c", 9).unwrap();
        let out = project.root.join(".ns-ide").join("bp-export.json");
        let args = HashMap::from([("path".to_string(), json!(out.display().to_string()))]);
        let json = export_breakpoints(&project.root, &args).unwrap();
        assert_eq!(json["exported"], 2);
        assert!(out.exists());
    }

    #[test]
    fn update_breakpoint_sets_properties_and_persists() {
        let dir = temp_dir("bp-props");
        let project = sample_project(&dir);
        let args = |key: &str, value: serde_json::Value| {
            HashMap::from([
                ("path".to_string(), json!("src/main.c")),
                ("line".to_string(), json!(7u64)),
                (key.to_string(), value),
            ])
        };

        // A condition creates a conditional breakpoint.
        let json =
            update_breakpoint(&project.root, &args("condition", json!("count == 10"))).unwrap();
        assert_eq!(json["breakpoint"]["condition"], "count == 10");
        assert_eq!(json["breakpoint"]["kind"], "条件断点");
        assert_eq!(json["breakpoint"]["enabled"], true);

        // A hit-count limit is applied independently.
        let json = update_breakpoint(&project.root, &args("hit_count", json!("3"))).unwrap();
        assert_eq!(json["breakpoint"]["hit_count"], "3");

        // A hardware kind is forced (and stays a conditional breakpoint kind
        // after the switch, since hardware preserves condition/log).
        let json = update_breakpoint(&project.root, &args("hardware", json!(true))).unwrap();
        assert_eq!(json["breakpoint"]["hardware"], true);
        assert_eq!(json["breakpoint"]["kind"], "硬件断点");

        // A log message turns it into a logging breakpoint.
        let json = update_breakpoint(&project.root, &args("log_message", json!("x={x}"))).unwrap();
        assert_eq!(json["breakpoint"]["log_message"], "x={x}");
        assert_eq!(json["breakpoint"]["kind"], "日志断点");

        // Disable, then verify persistence by reloading the store.
        let json = update_breakpoint(&project.root, &args("enabled", json!(false))).unwrap();
        assert_eq!(json["breakpoint"]["enabled"], false);
        let store = BreakpointStore::load(&project.root);
        let saved = store.all().iter().find(|b| b.line == 7).unwrap();
        assert_eq!(saved.condition, "count == 10");
        assert_eq!(saved.hit_count, "3");
        assert_eq!(saved.log_message, "x={x}");
        assert!(!saved.enabled);
    }

    #[test]
    fn watchpoint_add_update_remove_and_enable() {
        let dir = temp_dir("watch");
        let project = sample_project(&dir);

        let json = add_watchpoint(
            &project.root,
            &HashMap::from([
                ("expression".to_string(), json!("marker")),
                ("access".to_string(), json!("read")),
            ]),
        )
        .unwrap();
        assert_eq!(json["added"], true);
        assert_eq!(json["access"], "读取");
        assert_eq!(json["count"], 1);

        // Re-adding the same expression updates its access type.
        let json = add_watchpoint(
            &project.root,
            &HashMap::from([
                ("expression".to_string(), json!("marker")),
                ("access".to_string(), json!("write")),
            ]),
        )
        .unwrap();
        assert_eq!(json["access"], "写入");
        assert_eq!(json["count"], 1);

        let json = set_watchpoint_enabled(
            &project.root,
            &HashMap::from([
                ("expression".to_string(), json!("marker")),
                ("enabled".to_string(), json!(false)),
            ]),
        )
        .unwrap();
        assert_eq!(json["updated"], true);

        let json = remove_watchpoint(&project.root, "marker").unwrap();
        assert_eq!(json["removed"], true);
        assert_eq!(json["count"], 0);
    }

    #[test]
    fn external_tools_add_list_remove_and_persist() {
        let dir = temp_dir("ext-tools");
        let project = sample_project(&dir);

        // A missing id is derived from the title and made unique.
        let json = add_external_tool(
            &project.root,
            &HashMap::from([
                ("title".to_string(), json!("Build Firmware")),
                ("command".to_string(), json!("make")),
            ]),
        )
        .unwrap();
        assert_eq!(json["added"], true);
        assert_eq!(json["id"], "build-firmware");
        assert_eq!(json["count"], 1);

        // An explicit id is honored; re-adding it replaces the tool.
        let json = add_external_tool(
            &project.root,
            &HashMap::from([
                ("id".to_string(), json!("build")),
                ("title".to_string(), json!("Build")),
                ("command".to_string(), json!("make -j")),
            ]),
        )
        .unwrap();
        assert_eq!(json["added"], true);
        assert_eq!(json["count"], 2);
        let json = add_external_tool(
            &project.root,
            &HashMap::from([
                ("id".to_string(), json!("build")),
                ("title".to_string(), json!("Build v2")),
                ("command".to_string(), json!("make clean")),
            ]),
        )
        .unwrap();
        assert_eq!(json["added"], false, "replacing an existing id");
        assert_eq!(json["count"], 2);

        let listed = list_external_tools(&project.root).unwrap();
        assert_eq!(listed["count"], 2);
        assert!(
            listed["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["id"] == "build-firmware")
        );
        assert!(
            listed["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["title"] == "Build v2")
        );

        // Removal, then persistence across a reload.
        let json = remove_external_tool(&project.root, "build").unwrap();
        assert_eq!(json["removed"], true);
        assert_eq!(json["count"], 1);
        let reloaded = ns_project::ToolStore::load(&project.root);
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded.find("build-firmware").unwrap().command, "make");
    }

    #[test]
    fn check_toolchain_reports_installed_tools() {
        let json = check_toolchain().unwrap();
        assert!(json["tools"].as_array().unwrap().len() >= 10);
        assert!(json["tools"][0]["name"].is_string());
        assert!(json["tools"][0]["available"].is_boolean());
    }

    #[test]
    fn debug_adapter_status_returns_bool() {
        let json = debug_adapter_status().unwrap();
        assert!(json["installed"].is_boolean());
    }

    #[test]
    fn list_open_files_reads_session() {
        let dir = temp_dir("session");
        let project = sample_project(&dir);
        let mut session = Session::load(&project.root);
        session.open_files.push(PathBuf::from("src/main.c"));
        session.save(&project.root).unwrap();
        let json = list_open_files(&project.root).unwrap();
        assert_eq!(json["count"], 1);
        assert_eq!(json["files"][0], "src/main.c");
    }

    #[test]
    fn list_build_outputs_empty_when_missing() {
        let dir = temp_dir("out");
        let project = sample_project(&dir);
        let json = list_build_outputs(&project.root).unwrap();
        assert_eq!(json["count"], 0);
    }

    #[test]
    fn read_recent_projects_returns_array() {
        let json = read_recent_projects().unwrap();
        assert!(json["projects"].is_array());
    }

    #[test]
    fn execute_dispatches_unknown_tool_error() {
        let args = HashMap::new();
        assert!(execute("nope", &args).is_err());
    }

    #[test]
    fn parse_svd_preview_reads_file() {
        let dir = temp_dir("svd");
        let project = sample_project(&dir);
        let svd_path = project.root.join("device.svd");
        fs::write(&svd_path, "<device><name>demo</name></device>").unwrap();
        let json = parse_svd_preview(&svd_path.display().to_string()).unwrap();
        assert_eq!(json["name"], "demo");
    }

    #[test]
    fn generate_init_code_emits_c() {
        let args = HashMap::from([
            ("target".to_string(), json!("stm32f103c8")),
            (
                "peripherals".to_string(),
                json!([{"name": "GPIOA", "purpose": "GPIO 输出"}]),
            ),
        ]);
        let json = generate_init_code(&args).unwrap();
        assert!(json["code"].as_str().unwrap().contains("GPIOA"));
    }

    #[test]
    fn convert_firmware_hex_to_bin() {
        let dir = temp_dir("convert");
        let project = sample_project(&dir);
        let hex = ":020000040800F2\n:0400000001020304F2\n:00000001FF\n";
        let hex_path = project.root.join("fw.hex");
        fs::write(&hex_path, hex).unwrap();
        let json = convert_firmware(&hex_path.display().to_string(), "bin").unwrap();
        assert_eq!(json["format"], "BIN");
        assert!(json["bytes"].as_u64().unwrap() > 0);
    }

    #[test]
    fn decode_hex_returns_bytes() {
        let dir = temp_dir("decodehex");
        let project = sample_project(&dir);
        let hex = ":0400000001020304F2\n:00000001FF\n";
        let hex_path = project.root.join("fw.hex");
        fs::write(&hex_path, hex).unwrap();
        let json = decode_hex(&hex_path.display().to_string()).unwrap();
        assert_eq!(json["bytes"], 4);
    }

    #[test]
    fn analyze_firmware_reports_sizes() {
        let dir = temp_dir("analyze");
        let project = sample_project(&dir);
        let bin = project.root.join("fw.bin");
        fs::write(&bin, vec![0u8; 128]).unwrap();
        let json = analyze_firmware(&bin.display().to_string(), "stm32f103c8").unwrap();
        assert_eq!(json["total"], 128);
        // A raw BIN at base 0 does not fall in the FLASH window; it is
        // attributed to RAM. Flash + RAM must equal the total payload.
        let flash = json["flash"].as_u64().unwrap();
        let ram = json["ram"].as_u64().unwrap();
        assert_eq!(flash + ram, 128);
    }

    #[test]
    fn decode_hardfault_reports_forced_bus_fault() {
        // CFSR.BFSR.PRECISERR (bit 9) + HFSR.FORCED (bit 30), with a stacked
        // frame whose PC/LR/xPSR survive in the faulting context.
        let args = HashMap::from([
            ("cfsr".to_string(), json!(0x0000_0200u64)),
            ("hfsr".to_string(), json!(0x4000_0000u64)),
            (
                "stacked".to_string(),
                json!([1u64, 2, 3, 4, 5, 0x0800_0000, 0x0800_1234, 0x2100_0000]),
            ),
        ]);
        let json = decode_hardfault(&args).unwrap();
        assert_eq!(json["fault_type"], "HardFault");
        assert_eq!(json["is_forced"], true);
        assert_eq!(json["fault_pc"], 0x0800_1234u64);
        assert_eq!(json["fault_lr"], 0x0800_0000u64);
        assert_eq!(json["xpsr"], 0x2100_0000u64);
        let fields = json["fields"].as_array().unwrap();
        assert!(fields.iter().any(|f| f["name"] == "PRECISERR"));
        assert!(fields.iter().any(|f| f["name"] == "FORCED"));
        assert!(json["summary"].as_array().unwrap().len() >= 2);
    }

    #[test]
    fn decode_hardfault_accepts_full_snapshot_object() {
        // Feeding a complete FaultSnapshot (the same shape the IDE exports)
        // must produce the identical report as the field-by-field form.
        let snapshot = serde_json::json!({
            "cfsr": 0x0000_0200u64,
            "hfsr": 0x4000_0000u64,
            "mmfar": null,
            "bfar": null,
            "afsr": null,
            "lr_exc_return": 0xFFFF_FFFDu64,
            "stacked": [1u64, 2, 3, 4, 5, 0x0800_0000, 0x0800_1234, 0x2100_0000],
        });
        let args = HashMap::from([("snapshot".to_string(), snapshot)]);
        let json = decode_hardfault(&args).unwrap();
        assert_eq!(json["fault_type"], "HardFault");
        assert_eq!(json["fault_pc"], 0x0800_1234u64);
        assert!(json["exc_return"].is_string());
    }

    #[test]
    fn trace_waveform_auto_detects_channels() {
        use ns_debug::swvd::Packet;
        let packets = vec![
            Packet::Stimulus {
                port: 0,
                payload: vec![0x10],
            },
            Packet::Dwt {
                index: 0,
                pc: Some(0x0800_1234),
            },
            Packet::Exception {
                number: 3,
                entered: true,
            },
            Packet::Timestamp(42),
        ];
        let args = HashMap::from([(
            "packets".to_string(),
            serde_json::to_value(&packets).unwrap(),
        )]);
        let json = trace_waveform(&args).unwrap();
        assert_eq!(json["packet_count"], 4);
        assert_eq!(json["has_timestamps"], true);
        let channels = json["channels"].as_array().unwrap();
        assert_eq!(channels.len(), 4, "auto-detect should find all four kinds");
        let ids = channels
            .iter()
            .map(|c| c["id"].as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"stim0".to_string()));
        assert!(ids.contains(&"dwt_pc".to_string()));
        assert!(ids.contains(&"exc3".to_string()));
        assert!(ids.contains(&"ts".to_string()));
    }

    #[test]
    fn trace_waveform_ignores_unknown_channel_ids() {
        use ns_debug::swvd::Packet;
        let packets = vec![Packet::Stimulus {
            port: 0,
            payload: vec![0x20, 0x30],
        }];
        let args = HashMap::from([
            (
                "packets".to_string(),
                serde_json::to_value(&packets).unwrap(),
            ),
            ("channels".to_string(), json!(["stim0", "bogus", "dwt_pc"])),
        ]);
        let json = trace_waveform(&args).unwrap();
        let channels = json["channels"].as_array().unwrap();
        // Unknown "bogus" is dropped; only the requested stim0 survives (the
        // DWT channel has no backing packet but is still reported as empty).
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0]["id"], "stim0");
        // A stimulus packet contributes one point (its first byte).
        assert_eq!(channels[0]["samples"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn profile_hotspots_attributes_samples_to_symbols() {
        let args = HashMap::from([
            (
                "symbols".to_string(),
                json!([
                    {"address": 0x1000u64, "size": 0x40u64, "name": "foo"},
                    {"address": 0x1100u64, "size": 0x20u64, "name": "bar"},
                ]),
            ),
            (
                "samples".to_string(),
                json!([0x1004u64, 0x1008u64, 0x1104u64, 0x2000u64]),
            ),
        ]);
        let json = profile_hotspots(&args).unwrap();
        assert_eq!(json["total"], 4);
        let hotspots = json["hotspots"].as_array().unwrap();
        // Sorted by hits descending: foo (2), then bar (1) and <unknown> (1).
        assert_eq!(hotspots[0]["name"], "foo");
        assert_eq!(hotspots[0]["hits"], 2);
        assert!(
            hotspots
                .iter()
                .any(|h| h["name"] == "bar" && h["hits"] == 1)
        );
        assert!(
            hotspots
                .iter()
                .any(|h| h["name"] == "<unknown>" && h["hits"] == 1)
        );
    }

    #[test]
    fn rtos_tasks_walks_ready_and_delayed_lists() {
        struct Mem {
            data: Vec<u8>,
            base: u64,
        }
        impl Mem {
            fn write(&mut self, addr: u64, bytes: &[u8]) {
                let offset = (addr - self.base) as usize;
                self.data[offset..offset + bytes.len()].copy_from_slice(bytes);
            }
            fn write_u32(&mut self, addr: u64, value: u32) {
                self.write(addr, &value.to_le_bytes());
            }
            fn write_str(&mut self, addr: u64, value: &str) {
                self.write(addr, value.as_bytes());
            }
        }

        // Build a tiny synthetic FreeRTOS RAM image: two TCBs, one ready list
        // and one delayed list, with the default Cortex-M layout.
        const BASE: u64 = 0;
        let mut mem = Mem {
            data: vec![0u8; 0x1000],
            base: BASE,
        };

        let tcb_a = 0x100u64;
        let tcb_b = 0x200u64;
        let ready_head = 0x300u64;
        let ready_item = ready_head + 0x10;
        let delayed_head = 0x400u64;
        let delayed_item = delayed_head + 0x10;

        // TCB fields (default layout: pcTaskName@44, uxPriority@36).
        mem.write_str(tcb_a + 44, "taskA");
        mem.write_u32(tcb_a + 36, 1);
        mem.write_str(tcb_b + 44, "taskB");
        mem.write_u32(tcb_b + 36, 2);

        // Ready list head: uxNumberOfItems@0, xListEnd@8, pxNext@4, pvOwner@12.
        mem.write_u32(ready_head, 1);
        mem.write_u32(ready_head + 8 + 4, ready_item as u32);
        mem.write_u32(ready_item + 4, (ready_head + 8) as u32);
        mem.write_u32(ready_item + 12, tcb_a as u32);

        // Delayed list head.
        mem.write_u32(delayed_head, 1);
        mem.write_u32(delayed_head + 8 + 4, delayed_item as u32);
        mem.write_u32(delayed_item + 4, (delayed_head + 8) as u32);
        mem.write_u32(delayed_item + 12, tcb_b as u32);

        let args = HashMap::from([
            (
                "memory".to_string(),
                json!(mem.data.iter().map(|byte| *byte as u64).collect::<Vec<_>>()),
            ),
            ("base".to_string(), json!(BASE)),
            (
                "lists".to_string(),
                json!([
                    {"address": ready_head, "state": 0u32},
                    {"address": delayed_head, "state": 1u32},
                ]),
            ),
            ("running_tcb".to_string(), json!(tcb_a)),
        ]);
        let json = rtos_tasks(&args).unwrap();
        assert_eq!(json["count"], 2);
        let tasks = json["tasks"].as_array().unwrap();
        let task_a = tasks.iter().find(|task| task["name"] == "taskA").unwrap();
        assert_eq!(task_a["running"], true);
        assert_eq!(task_a["state"], 0);
        assert_eq!(task_a["priority"], 1);
        let task_b = tasks.iter().find(|task| task["name"] == "taskB").unwrap();
        assert_eq!(task_b["running"], false);
        assert_eq!(task_b["state"], 1);
        assert_eq!(task_b["priority"], 2);
    }

    #[test]
    fn decode_trace_parses_stimulus_exception_timestamp() {
        // Stimulus "hi" on port 0, an exception entry (number 15), then a
        // timestamp packet (same fixture as the ns-debug decode test).
        let bytes = [
            0x00u8, 0x02, b'h', b'i', 0x02, 0x1E, 0x01, 0x04, 0x03, 0x02, 0x01,
        ];
        let args = HashMap::from([(
            "bytes".to_string(),
            json!(bytes.iter().map(|b| *b as u64).collect::<Vec<_>>()),
        )]);
        let json = decode_trace(&args).unwrap();
        assert_eq!(json["consumed"], bytes.len());
        let packets = json["packets"].as_array().unwrap();
        assert_eq!(packets.len(), 3);
        assert!(packets[0]["Stimulus"]["port"] == 0);
        assert_eq!(
            packets[0]["Stimulus"]["payload"],
            json!([b'h' as u64, b'i' as u64])
        );
        assert_eq!(packets[1]["Exception"]["number"], 15);
        assert_eq!(packets[1]["Exception"]["entered"], true);
        assert_eq!(packets[2]["Timestamp"], 0x0102_0304u64);
        assert_eq!(json["stats"]["stimulus"], 1);
        assert_eq!(json["stats"]["exceptions"], 1);
        assert_eq!(json["stats"]["timestamps"], 1);
        assert_eq!(json["stats"]["pc_samples"], 0);
    }

    #[test]
    fn decode_trace_accepts_hex_string() {
        let args = HashMap::from([("hex".to_string(), json!("00026869021e0104030201"))]);
        let json = decode_trace(&args).unwrap();
        assert_eq!(json["consumed"], 11);
        assert_eq!(json["packets"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn run_command_captures_output_and_exit_code() {
        let dir = temp_dir("run-cmd");
        let root = dir.clone();
        let ok = run_command(
            &root,
            &HashMap::from([("command".to_string(), json!("printf hello"))]),
        )
        .unwrap();
        assert_eq!(ok["success"], true);
        assert_eq!(ok["stdout"], "hello");

        let fail = run_command(
            &root,
            &HashMap::from([("command".to_string(), json!("exit 3"))]),
        )
        .unwrap();
        assert_eq!(fail["success"], false);
        assert_eq!(fail["code"], 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_command_timeout_kills_hung_command() {
        let dir = temp_dir("run-timeout");
        let err = run_command(
            &dir,
            &HashMap::from([
                ("command".to_string(), json!("while :; do :; done")),
                ("timeout_ms".to_string(), json!(150)),
            ]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("超时"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_external_tool_runs_by_id_and_times_out() {
        let dir = temp_dir("run-ext");
        let root = dir.clone();
        add_external_tool(
            &root,
            &HashMap::from([
                ("id".to_string(), json!("hi")),
                ("title".to_string(), json!("Say hi")),
                ("command".to_string(), json!("printf hi")),
            ]),
        )
        .unwrap();
        let json =
            run_external_tool(&root, &HashMap::from([("id".to_string(), json!("hi"))])).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["stdout"], "hi");
        assert_eq!(json["id"], "hi");

        let err = run_external_tool(&root, &HashMap::from([("id".to_string(), json!("nope"))]))
            .unwrap_err();
        assert!(err.to_string().contains("不存在"));

        add_external_tool(
            &root,
            &HashMap::from([
                ("id".to_string(), json!("hang")),
                ("title".to_string(), json!("Hang")),
                ("command".to_string(), json!("while :; do :; done")),
            ]),
        )
        .unwrap();
        let err = run_external_tool(
            &root,
            &HashMap::from([
                ("id".to_string(), json!("hang")),
                ("timeout_ms".to_string(), json!(150)),
            ]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("超时"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_get_save_round_trips() {
        let dir = temp_dir("session");
        let root = dir.clone();
        let json = get_session(&root).unwrap();
        assert_eq!(json["count"], 0);

        save_session(
            &root,
            &HashMap::from([
                ("open_files".to_string(), json!(["src/main.c", "README.md"])),
                ("active".to_string(), json!(1)),
                (
                    "views".to_string(),
                    json!([{
                        "path": "src/main.c",
                        "cursor": [3, 5],
                        "scroll_ratio": 0.5,
                        "zoom": 1.2,
                        "indent_width": 8,
                        "word_wrap": true,
                    }]),
                ),
                ("split".to_string(), json!("src/util.c")),
            ]),
        )
        .unwrap();

        let json = get_session(&root).unwrap();
        assert_eq!(json["count"], 2);
        assert_eq!(json["active"], 1);
        assert_eq!(json["open_files"][0], "src/main.c");
        assert_eq!(json["split"], "src/util.c");
        let views = json["views"].as_array().unwrap();
        assert_eq!(views[0]["path"], "src/main.c");
        assert_eq!(views[0]["cursor"][0], 3);
        assert_eq!(views[0]["cursor"][1], 5);
        assert!((views[0]["zoom"].as_f64().unwrap() - 1.2).abs() < 1e-6);
        assert_eq!(views[0]["indent_width"], 8);
        assert_eq!(views[0]["word_wrap"], true);

        // Persisted to disk and re-read from a fresh load.
        let json = get_session(&root).unwrap();
        assert_eq!(json["count"], 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lsp_parse_hover_and_locations() {
        let hover = serde_json::json!({"result": {"contents": {"kind":"markdown","value":"int add(int a, int b)"}}});
        assert_eq!(
            parse_hover_text(&hover).as_deref(),
            Some("int add(int a, int b)")
        );
        let hover2 = serde_json::json!({"result": {"contents": "plain text"}});
        assert_eq!(parse_hover_text(&hover2).as_deref(), Some("plain text"));
        assert_eq!(parse_hover_text(&serde_json::json!({"result": null})), None);

        let loc = serde_json::json!({"result": {"uri":"file:///a/b.c","range":{"start":{"line":2,"character":3},"end":{"line":2,"character":6}}}});
        let list = parse_location_list(&loc);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["line"], 3);
        assert_eq!(list[0]["column"], 4);
        assert_eq!(list[0]["path"], "/a/b.c");
        assert!(parse_location_list(&serde_json::json!({"result": null})).is_empty());
        assert!(parse_location_list(&serde_json::json!({"result": []})).is_empty());
    }

    #[test]
    fn lsp_parse_completion_items() {
        let resp = serde_json::json!({"result": [
            {"label":"foo","kind":3,"detail":"int"},
            {"label":"bar","insertText":"bar()","kind":2},
            {"label":"baz","textEdit":{"newText":"baz(1)"}},
        ]});
        let items = parse_completion_items(&resp);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["label"], "foo");
        assert_eq!(items[1]["insert_text"], "bar()");
        assert_eq!(items[2]["insert_text"], "baz(1)");
        let object_form =
            serde_json::json!({"result": {"isIncomplete": false, "items": [{"label":"x"}]}});
        assert_eq!(parse_completion_items(&object_form).len(), 1);
        assert!(parse_completion_items(&serde_json::json!({"result": null})).is_empty());
    }

    #[test]
    fn lsp_language_for_explicit_project_and_extension() {
        let dir = temp_dir("lang");
        let project = sample_project(&dir);
        let explicit = language_for(
            &HashMap::from([("language".to_string(), json!("Rust"))]),
            &project.root,
            Path::new("x.c"),
        )
        .unwrap();
        assert_eq!(explicit, Language::Rust);
        let no_args = HashMap::new();
        let from_project = language_for(&no_args, &project.root, Path::new("x.c")).unwrap();
        assert_eq!(from_project, Language::C);

        let empty = std::env::temp_dir().join(format!("ns-mcp-nolang-{}", std::process::id()));
        let _ = fs::remove_dir_all(&empty);
        fs::create_dir_all(&empty).unwrap();
        let from_ext = language_for(&no_args, &empty, Path::new("src/lib.rs")).unwrap();
        assert_eq!(from_ext, Language::Rust);
        let _ = fs::remove_dir_all(&empty);
    }

    #[test]
    fn lsp_session_start_reports_missing_server_gracefully() {
        if ns_lsp::server_available(&Language::Zig) {
            return;
        }
        let dir = temp_dir("lsp-missing");
        let project = sample_project(&dir);
        let json = lsp_session_start(
            &project.root,
            &HashMap::from([("language".to_string(), json!("Zig"))]),
        )
        .unwrap();
        assert_eq!(json["available"], false);
        assert_eq!(json["started"], false);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lsp_hover_definition_references_with_clangd() {
        if !ns_lsp::server_available(&Language::C) {
            return;
        }
        let dir = temp_dir("lsp");
        let project = sample_project(&dir);
        fs::write(
            project.root.join("src/main.c"),
            "int add(int a, int b) { return a + b; }\nint main(void) { return add(1, 2); }\n",
        )
        .unwrap();
        let start = lsp_session_start(
            &project.root,
            &HashMap::from([("language".to_string(), json!("C"))]),
        )
        .unwrap();
        assert_eq!(start["started"], true);

        let hover = lsp_hover(
            &project.root,
            &HashMap::from([
                ("path".to_string(), json!("src/main.c")),
                ("line".to_string(), json!(2)),
                ("column".to_string(), json!(25)),
            ]),
        )
        .unwrap();
        let hover_text = hover["hover"].as_str().unwrap_or("");
        assert!(hover_text.contains("add"), "hover text: {hover_text}");

        let definition = lsp_definition(
            &project.root,
            &HashMap::from([
                ("path".to_string(), json!("src/main.c")),
                ("line".to_string(), json!(2)),
                ("column".to_string(), json!(25)),
            ]),
        )
        .unwrap();
        assert_eq!(definition["count"], 1);
        assert_eq!(definition["locations"][0]["line"], 1);

        let references = lsp_references(
            &project.root,
            &HashMap::from([
                ("path".to_string(), json!("src/main.c")),
                ("line".to_string(), json!(1)),
                ("column".to_string(), json!(5)),
            ]),
        )
        .unwrap();
        assert!(references["count"].as_u64().unwrap() >= 1);

        let completion = lsp_completion(
            &project.root,
            &HashMap::from([
                ("path".to_string(), json!("src/main.c")),
                ("line".to_string(), json!(2)),
                ("column".to_string(), json!(20)),
            ]),
        )
        .unwrap();
        assert!(completion["count"].as_u64().unwrap() >= 1);

        let end = lsp_session_end(
            &project.root,
            &HashMap::from([("language".to_string(), json!("C"))]),
        )
        .unwrap();
        assert_eq!(end["stopped"], true);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn suggest_quick_fix_hints_missing_include() {
        let json = suggest_quick_fix_tool("'stdio.h' file not found").unwrap();
        assert!(json["quick_fix"].is_object() || json["quick_fix"].is_null());
    }

    #[test]
    fn git_status_reports_non_repo() {
        let dir = temp_dir("git-nonrepo");
        let json = git_status(&dir).unwrap();
        assert_eq!(json["in_repo"], false);
    }

    #[test]
    fn import_project_parses_ioc() {
        let dir = temp_dir("import");
        let ioc = dir.join("blinky.ioc");
        fs::write(
            &ioc,
            "[Mcu]\nMcu.Name=STM32F103C8Tx\nMcu.UserName=STM32F103C8\n",
        )
        .unwrap();
        let args = HashMap::from([("path".to_string(), json!(ioc.display().to_string()))]);
        let json = import_project(&args).unwrap();
        assert_eq!(json["imported"], true);
        assert!(dir.join("project.toml").exists());
    }

    #[test]
    fn get_toolchain_config_returns_overrides() {
        let json = get_toolchain_config().unwrap();
        assert!(json["overrides"].is_object());
        assert!(json["count"].is_u64());
    }

    #[test]
    fn list_toolchain_registry_for_c() {
        let json = list_toolchain_registry("C").unwrap();
        assert_eq!(json["language"], "C");
        assert!(json["tools"].as_array().unwrap().len() >= 3);
    }

    // ── Project analysis tools ──────────────────────────────────────────────

    #[test]
    fn scan_tasks_finds_markers_and_filters_keyword() {
        let dir = temp_dir("tasks-mcp");
        let project = sample_project(&dir);
        write_file(
            &project.root,
            "src/main.c",
            "// TODO: implement uart\nint main(void) { return 0; }\n// FIXME: off-by-one\n",
        )
        .unwrap();
        let all = scan_tasks(&project.root, None).unwrap();
        assert!(all["count"].as_u64().unwrap() >= 2);
        let filtered = scan_tasks(&project.root, Some("TODO")).unwrap();
        assert_eq!(filtered["count"], 1);
        assert_eq!(filtered["tasks"][0]["keyword"], "TODO");
        assert!(
            filtered["tasks"][0]["text"]
                .as_str()
                .unwrap()
                .contains("implement uart")
        );
    }

    #[test]
    fn include_dependencies_builds_graph_and_layout() {
        let dir = temp_dir("dep-mcp");
        let project = sample_project(&dir);
        fs::create_dir_all(project.root.join("include")).unwrap();
        fs::write(project.root.join("include/board.h"), "#pragma once\n").unwrap();
        write_file(
            &project.root,
            "src/main.c",
            "#include <board.h>\nint main(void) { return 0; }\n",
        )
        .unwrap();
        let json = include_dependencies(&project.root).unwrap();
        assert!(json["count"].as_u64().unwrap() >= 1);
        let main = json["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["path"] == "src/main.c")
            .expect("main.c should be a graph node");
        assert!(
            main["deps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|dep| dep.as_str().unwrap_or("").contains("board.h"))
        );
        assert!(json["dag_nodes"].as_array().unwrap().len() >= 1);
        assert!(json["dag_edges"].is_array());
        assert!(json["columns"].is_array());
    }

    #[test]
    fn memory_layout_parses_linker_regions() {
        let dir = temp_dir("layout-mcp");
        let project = sample_project(&dir);
        fs::write(
            project.root.join("link.ld"),
            "MEMORY {\n  FLASH (rx) : ORIGIN = 0x08000000, LENGTH = 64K\n  RAM (rwx) : ORIGIN = 0x20000000, LENGTH = 20K\n}\n",
        )
        .unwrap();
        let json = memory_layout(&project.root).unwrap();
        let regions = json["regions"].as_array().unwrap();
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0]["name"], "FLASH");
        assert_eq!(regions[0]["origin"], 0x0800_0000u64);
        assert_eq!(regions[1]["name"], "RAM");
        assert!(json["used_total"].is_u64());
        assert!(json["sections"].is_array());
    }

    #[test]
    fn run_stack_usage_compiles_and_reports_entries() {
        if ns_project::toolchain::find_command("gcc").is_none() {
            eprintln!("gcc 不可用，跳过 MCP 栈使用分析");
            return;
        }
        let dir = temp_dir("stack-mcp");
        let project = sample_project(&dir);
        write_file(
            &project.root,
            "src/main.c",
            "static int helper(int x) { return x + 1; }\nint main(void) { return helper(1); }\n",
        )
        .unwrap();
        let json = run_stack_usage(&project.root).unwrap();
        assert!(json["success"].as_bool().unwrap());
        assert!(json["count"].as_u64().unwrap() >= 1);
        assert!(json["entries"].as_array().unwrap()[0]["function"].is_string());
    }

    #[test]
    fn run_coverage_compiles_runs_and_summarizes() {
        if ns_project::toolchain::find_command("gcc").is_none()
            || ns_project::toolchain::find_command("gcov").is_none()
        {
            eprintln!("gcc/gcov 不可用，跳过 MCP 覆盖率分析");
            return;
        }
        let dir = temp_dir("coverage-mcp");
        let project = sample_project(&dir);
        write_file(
            &project.root,
            "src/main.c",
            "static int pick(int x) { if (x > 0) return 1; return 0; }\nint main(void) { return pick(1); }\n",
        )
        .unwrap();
        let json = run_coverage(&project.root).unwrap();
        assert!(json["success"].as_bool().unwrap());
        assert!(json["count"].as_u64().unwrap() >= 1);
        let file = &json["files"].as_array().unwrap()[0];
        assert!(file["file"].is_string());
        assert!(file["percent"].is_u64());
        assert!(file["line_hits"].is_array() || file["line_hits"].is_null());
    }

    // ── Stateful simulator tools ─────────────────────────────────────────────

    /// Create a RISC-V project and write a minimal program (nop; addi x5,x0,1;
    /// addi x5,x5,1) at its artifact path so a simulator session can be started.
    fn sim_project_with_firmware() -> Project {
        let dir = temp_dir("sim");
        let spec = NewProjectSpec {
            name: "blinky".to_string(),
            location: dir,
            language: Language::C,
            compiler: "gcc".to_string(),
            simulator: "ns-sim".to_string(),
            debugger: "gdb".to_string(),
            flasher: "openocd".to_string(),
            target: "riscv32".to_string(),
            svd: String::new(),
            c_template: Some(CTemplate::Empty),
        };
        let project = Project::create(spec).unwrap();
        let artifact = project.artifact_path();
        if let Some(parent) = artifact.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        // RV32I: addi x0,x0,0; addi x5,x0,1; addi x5,x5,1.
        fs::write(
            &artifact,
            vec![
                0x13, 0x00, 0x00, 0x00, // addi x0, x0, 0
                0x93, 0x02, 0x10, 0x00, // addi x5, x0, 1
                0x93, 0x82, 0x12, 0x00, // addi x5, x5, 1
            ],
        )
        .unwrap();
        project
    }

    fn start_session(project: &Project) -> String {
        let args = HashMap::new();
        let json = sim_session_start_in(&project.root, &args).unwrap();
        assert!(json["session"].is_string());
        assert_eq!(json["arch"], "RISC-V (RV32I)");
        json["session"].as_str().unwrap().to_string()
    }

    #[test]
    fn sim_session_start_creates_session() {
        let project = sim_project_with_firmware();
        let session = start_session(&project);
        let json = sim_status(&session).unwrap();
        assert_eq!(json["session"], session);
        assert_eq!(json["arch"], "RISC-V (RV32I)");
        assert!(json["pc"].is_u64());
        sim_session_end(&session).unwrap();
    }

    #[test]
    fn sim_step_and_status_advance_pc() {
        let project = sim_project_with_firmware();
        let session = start_session(&project);
        let before = sim_status(&session).unwrap()["pc"].as_u64().unwrap();
        let step = sim_step(&session, &HashMap::new()).unwrap();
        assert_eq!(step["status"], "advanced");
        let after_pc = step["pc"].as_u64().unwrap();
        assert!(after_pc > before, "PC should advance after one step");
        sim_session_end(&session).unwrap();
    }

    #[test]
    fn sim_read_write_mem_roundtrip() {
        let project = sim_project_with_firmware();
        let session = start_session(&project);
        let args = HashMap::from([
            ("address".to_string(), json!(0x2000_0000u64)),
            ("data".to_string(), json!("DEADBEEF")),
        ]);
        let written = sim_write_mem(&session, &args).unwrap();
        assert_eq!(written["written"], 4);
        let read = sim_read_mem(
            &session,
            &HashMap::from([
                ("address".to_string(), json!(0x2000_0000u64)),
                ("count".to_string(), json!(4u64)),
            ]),
        )
        .unwrap();
        assert_eq!(read["hex"], "deadbeef");
        assert_eq!(read["bytes"].as_array().unwrap().len(), 4);
        sim_session_end(&session).unwrap();
    }

    #[test]
    fn sim_read_write_reg_roundtrip() {
        let project = sim_project_with_firmware();
        let session = start_session(&project);
        sim_write_reg(
            &session,
            "x5",
            &HashMap::from([("value".to_string(), json!(0x12345678u64))]),
        )
        .unwrap();
        let json = sim_read_reg(&session, "x5").unwrap();
        assert_eq!(json["register"], "t0");
        assert_eq!(json["value"], 0x12345678u64);
        let pc_json = sim_read_reg(&session, "pc").unwrap();
        assert_eq!(pc_json["register"], "pc");
        sim_session_end(&session).unwrap();
    }

    #[test]
    fn sim_disassemble_returns_instructions() {
        let project = sim_project_with_firmware();
        let session = start_session(&project);
        let json = sim_disassemble(
            &session,
            &HashMap::from([
                ("address".to_string(), json!(0x8000_0000u64)),
                ("count".to_string(), json!(3u64)),
            ]),
        )
        .unwrap();
        let insns = json["instructions"].as_array().unwrap();
        assert_eq!(insns.len(), 3);
        assert_eq!(insns[0]["address"], 0x8000_0000u64);
        let texts: Vec<String> = insns
            .iter()
            .map(|i| format!("{}:{}", i["address"], i["text"]))
            .collect();
        assert!(
            insns[0]["text"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("addi"),
            "instructions: {texts:?}"
        );
        sim_session_end(&session).unwrap();
    }

    #[test]
    fn sim_snapshot_roundtrip() {
        let project = sim_project_with_firmware();
        let session = start_session(&project);
        sim_step(&session, &HashMap::new()).unwrap();
        let snapshot =
            std::env::temp_dir().join(format!("ns-mcp-sim-snapshot-{}.json", std::process::id()));
        let path = snapshot.display().to_string();
        sim_snapshot_save(&session, &path).unwrap();
        // Advance further, then restore the snapshot.
        sim_step(&session, &HashMap::new()).unwrap();
        let loaded = sim_snapshot_load(&session, &path).unwrap();
        assert_eq!(loaded["path"], path);
        let _ = fs::remove_file(&snapshot);
        sim_session_end(&session).unwrap();
    }

    #[test]
    fn sim_unknown_session_errors() {
        let err = sim_step("sim-unknown", &HashMap::new()).unwrap_err();
        assert!(err.to_string().contains("会话不存在"));
    }

    #[test]
    fn sim_breakpoint_add_list_remove_and_clear() {
        let project = sim_project_with_firmware();
        let session = start_session(&project);
        let added = sim_breakpoint_add(
            &session,
            &HashMap::from([
                ("address".to_string(), json!(0x8000_0004u64)),
                ("condition".to_string(), json!("x5 == 1")),
                ("hit_count".to_string(), json!(2u64)),
            ]),
        )
        .unwrap();
        assert_eq!(added["added"], true);
        assert_eq!(added["count"], 1);
        let list = sim_breakpoint_list(&session).unwrap();
        assert_eq!(list["count"], 1);
        assert_eq!(list["breakpoints"][0]["address"], 0x8000_0004u64);
        assert_eq!(list["breakpoints"][0]["condition"], "x5 == 1");
        assert_eq!(list["breakpoints"][0]["hit_count"], 2);
        let removed = sim_breakpoint_remove(
            &session,
            &HashMap::from([("address".to_string(), json!(0x8000_0004u64))]),
        )
        .unwrap();
        assert_eq!(removed["removed"], true);
        assert_eq!(removed["count"], 0);
        sim_breakpoint_add(
            &session,
            &HashMap::from([("address".to_string(), json!(0x8000_0000u64))]),
        )
        .unwrap();
        let cleared = sim_clear_breakpoints(&session).unwrap();
        assert_eq!(cleared["cleared"], 1);
        sim_session_end(&session).unwrap();
    }

    #[test]
    fn sim_breakpoint_stops_execution() {
        let project = sim_project_with_firmware();
        let session = start_session(&project);
        sim_breakpoint_add(
            &session,
            &HashMap::from([("address".to_string(), json!(0x8000_0004u64))]),
        )
        .unwrap();
        let run = sim_run(
            &session,
            &HashMap::from([("steps".to_string(), json!(10u64))]),
        )
        .unwrap();
        assert_eq!(run["status"], "breakpoint");
        assert_eq!(run["pc"], 0x8000_0004u64);
        sim_session_end(&session).unwrap();
    }

    #[test]
    fn sim_run_to_reaches_address() {
        let project = sim_project_with_firmware();
        let session = start_session(&project);
        let run = sim_run_to(
            &session,
            &HashMap::from([
                ("address".to_string(), json!(0x8000_0008u64)),
                ("steps".to_string(), json!(20u64)),
            ]),
        )
        .unwrap();
        assert_eq!(run["reached"], true);
        assert_eq!(run["pc"], 0x8000_0008u64);
        assert_eq!(run["steps"], 2);
        sim_session_end(&session).unwrap();
    }

    #[test]
    fn sim_step_over_and_out_run() {
        let project = sim_project_with_firmware();
        let session = start_session(&project);
        let over = sim_step_over(
            &session,
            &HashMap::from([("steps".to_string(), json!(100u64))]),
        )
        .unwrap();
        assert!(over["status"].is_string());
        assert!(over["pc"].as_u64().unwrap() >= 0x8000_0000u64);
        let out = sim_step_out(
            &session,
            &HashMap::from([("steps".to_string(), json!(100u64))]),
        )
        .unwrap();
        assert!(out["status"].is_string());
        sim_session_end(&session).unwrap();
    }

    #[test]
    fn sim_load_image_and_peripherals_and_uart() {
        let project = sim_project_with_firmware();
        let session = start_session(&project);
        let loaded = sim_load_image(
            &session,
            &HashMap::from([
                ("address".to_string(), json!(0x2000_0000u64)),
                ("data".to_string(), json!("01020304")),
            ]),
        )
        .unwrap();
        assert_eq!(loaded["written"], 4);
        let read = sim_read_mem(
            &session,
            &HashMap::from([
                ("address".to_string(), json!(0x2000_0000u64)),
                ("count".to_string(), json!(4u64)),
            ]),
        )
        .unwrap();
        assert_eq!(read["hex"], "01020304");

        let periph = sim_peripherals(&session).unwrap();
        assert!(periph["gpio_out"].is_u64());
        assert!(periph["uart_base"].is_u64());
        assert!(periph["timers"].is_array());
        let uart = sim_uart_read(&session).unwrap();
        assert_eq!(uart["bytes"].as_array().unwrap().len(), 0);
        assert_eq!(uart["text"], "");
        sim_session_end(&session).unwrap();
    }

    // ── Stateful debug (DAP) tools ──────────────────────────────────────────

    /// Whether a real `lldb-dap` adapter and `cc` are available for e2e tests.
    fn debug_adapter_available() -> bool {
        ns_debug::find_dap_adapter().is_some()
            && Command::new("cc").arg("--version").output().is_ok()
    }

    /// Build a tiny C fixture and return (root, source, program).
    fn compile_debug_fixture() -> (PathBuf, PathBuf, PathBuf) {
        let dir = temp_dir("dbg");
        let source = dir.join("dbg.c");
        let program = dir.join("dbg");
        fs::write(
            &source,
            "static volatile int marker = 0;\n\
             int main(void) {\n\
                 for (int i = 0; i < 2; i++) {\n\
                     marker++;\n\
                 }\n\
                 return marker;\n\
             }\n",
        )
        .unwrap();
        let status = Command::new("cc")
            .arg("-g")
            .arg("-O0")
            .arg(&source)
            .arg("-o")
            .arg(&program)
            .status()
            .unwrap();
        assert!(status.success(), "failed to compile debug fixture");
        (dir, source, program)
    }

    /// Start a DAP session with a breakpoint on line 4 (`marker++;`).
    fn start_debug_session(root: &Path, source: &Path, program: &Path) -> String {
        let args = HashMap::from([
            ("program".to_string(), json!(program.display().to_string())),
            (
                "breakpoints".to_string(),
                json!([{"file": source.display().to_string(), "line": 4}]),
            ),
        ]);
        let json = debug_session_start_in(root, &args).unwrap();
        json["session"].as_str().unwrap().to_string()
    }

    /// Poll `debug_status` until the session has actually stopped.
    fn wait_debug_stopped(session: &str) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let status = debug_status(session).unwrap();
            let state = status["status"].as_str().unwrap_or("");
            if state.starts_with("stopped:") || state == "terminated" || state.starts_with("error:")
            {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "debug session never stopped: {status}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn debug_unknown_session_errors() {
        let err = debug_status("dbg-unknown").unwrap_err();
        assert!(err.to_string().contains("调试会话不存在"));
        let err = debug_continue("dbg-unknown", &HashMap::new()).unwrap_err();
        assert!(err.to_string().contains("调试会话不存在"));
        let err = debug_breakpoint_list("dbg-unknown").unwrap_err();
        assert!(err.to_string().contains("调试会话不存在"));
        let err = debug_write_memory(
            "dbg-unknown",
            &HashMap::from([
                ("address".to_string(), json!(0u64)),
                ("data".to_string(), json!("00")),
            ]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("调试会话不存在"));
    }

    #[test]
    fn debug_session_full_lifecycle() {
        if !debug_adapter_available() {
            eprintln!("lldb-dap 或 cc 不可用，跳过 MCP 调试端到端测试");
            return;
        }
        let (dir, source, program) = compile_debug_fixture();
        let session = start_debug_session(&dir, &source, &program);

        // The config breakpoint stops the debuggee on line 4.
        let first_stop = wait_debug_stopped(&session);
        assert!(
            first_stop["status"]
                .as_str()
                .unwrap()
                .starts_with("stopped:")
        );
        let thread = first_stop["thread"].as_u64().unwrap();

        // Stack: the fixture frame is on top at line 4.
        let stacks = debug_stacks(
            &session,
            &HashMap::from([("thread".to_string(), json!(thread))]),
        )
        .unwrap();
        assert!(stacks["count"].as_u64().unwrap() >= 1);
        let frames = stacks["frames"].as_array().unwrap();
        let top = &frames[0];
        assert_eq!(top["line"], 4);
        let pc = top["pc"].as_u64().unwrap();

        // Variables: `marker` is file-scope, so it appears in Globals.
        let vars = debug_variables(
            &session,
            &HashMap::from([
                ("thread".to_string(), json!(thread)),
                ("scope".to_string(), json!("Global")),
            ]),
        )
        .unwrap();
        let names = vars["variables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>();
        assert!(
            names.contains(&"marker".to_string()),
            "expected `marker` in Globals, got {names:?}"
        );

        // Registers: at least one register is reported.
        let regs = debug_read_registers(
            &session,
            &HashMap::from([("thread".to_string(), json!(thread))]),
        )
        .unwrap();
        assert!(regs["count"].as_u64().unwrap() > 0);
        assert!(regs["registers"].as_array().unwrap()[0]["name"].is_string());

        // Read memory at the PC.
        let mem = debug_read_memory(
            &session,
            &HashMap::from([
                ("address".to_string(), json!(pc)),
                ("count".to_string(), json!(16u64)),
            ]),
        )
        .unwrap();
        assert_eq!(mem["bytes"].as_array().unwrap().len(), 16);

        // Disassembly at the PC returns real instructions.
        let disasm = debug_disassemble(
            &session,
            &HashMap::from([
                ("address".to_string(), json!(pc)),
                ("count".to_string(), json!(4u64)),
            ]),
        )
        .unwrap();
        assert!(disasm["instructions"].as_array().unwrap().len() >= 3);

        // Evaluate a global expression.
        let eval = debug_evaluate(
            &session,
            &HashMap::from([("expression".to_string(), json!("marker"))]),
        )
        .unwrap();
        assert!(!eval["result"].as_str().unwrap().is_empty());

        // Step over one statement from the first stop; the debuggee stays in
        // the loop, so continuing still reaches the second breakpoint hit.
        let step = debug_step_over(
            &session,
            &HashMap::from([("thread".to_string(), json!(thread))]),
        )
        .unwrap();
        assert!(step["reason"].is_string());

        // Set / list / clear a breakpoint round-trip.
        let set = debug_breakpoint_set(
            &session,
            &HashMap::from([
                ("file".to_string(), json!(source.display().to_string())),
                ("line".to_string(), json!(4u64)),
            ]),
        )
        .unwrap();
        assert_eq!(set["count"], 1);
        assert_eq!(set["results"].as_array().unwrap().len(), 1);
        let listed = debug_breakpoint_list(&session).unwrap();
        assert_eq!(listed["count"], 1);

        // Continue: with the breakpoint re-added above it hits the second
        // iteration, then continue again and let the process exit.
        let continued = debug_continue(
            &session,
            &HashMap::from([("thread".to_string(), json!(thread))]),
        )
        .unwrap();
        assert_eq!(continued["status"], "continued");
        let second_stop = wait_debug_stopped(&session);
        assert!(
            second_stop["status"]
                .as_str()
                .unwrap()
                .starts_with("stopped:")
        );

        // Clear the breakpoint and verify the store is empty.
        let cleared = debug_breakpoint_clear(
            &session,
            &HashMap::from([
                ("file".to_string(), json!(source.display().to_string())),
                ("line".to_string(), json!(4u64)),
            ]),
        )
        .unwrap();
        assert_eq!(cleared["removed"], true);
        assert_eq!(debug_breakpoint_list(&session).unwrap()["count"], 0);

        // Stop removes the session; a later status reports it gone.
        let stopped = debug_session_stop(&session).unwrap();
        assert_eq!(stopped["stopped"], true);
        assert!(debug_status(&session).is_err());

        let _ = fs::remove_dir_all(dir);
    }
}
