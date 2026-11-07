//! End-to-end test for the static-analysis path: run `cargo clippy` on a
//! warn-prone Rust project and confirm the GCC-style parser surfaces the
//! diagnostic. Skipped when `cargo` or `rustc` is not on PATH.
use std::process::Command;

use ns_build;
use ns_project::{Language, NewProjectSpec, Project};

#[test]
fn clippy_diagnostics_are_parsed() {
    if Command::new("cargo").arg("--version").output().is_err() {
        eprintln!("cargo not found; skipping clippy test");
        return;
    }
    if Command::new("rustc").arg("--version").output().is_err() {
        eprintln!("rustc not found; skipping clippy test");
        return;
    }

    let tmp = std::env::temp_dir().join(format!("ns-static-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let project = Project::create(NewProjectSpec {
        name: "static_smoke".to_string(),
        location: tmp.clone(),
        language: Language::Rust,
        compiler: "cargo".to_string(),
        simulator: String::new(),
        debugger: String::new(),
        flasher: String::new(),
        target: String::new(),
        svd: String::new(),
        c_template: None,
    })
    .expect("create project");

    // A deliberately warn-prone main.rs (clippy::clone_on_copy).
    std::fs::write(
        project.root.join("src/main.rs"),
        "fn main() {\n    let x = 5;\n    let _ = x.clone();\n}\n",
    )
    .expect("write main.rs");

    let plan = ns_build::plan_static_analysis(&project).expect("plan static analysis");
    let outcome = ns_build::execute(&plan);
    let diagnostics = outcome.diagnostics(&project.root);
    assert!(
        diagnostics.iter().any(|d| d.message.contains("clone_on_copy")
            || d.message.contains("using `clone` on type `i32`")),
        "expected a clone_on_copy diagnostic, got:\n{}",
        diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
            .join("\n")
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
