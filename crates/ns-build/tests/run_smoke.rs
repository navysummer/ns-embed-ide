//! Throwaway smoke test for the F9 direct-run path: build a host C project,
//! then spawn the artifact and stream its output, mirroring `IdeApp::spawn_run`.
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use ns_build;
use ns_project::{Language, NewProjectSpec, Project};

#[test]
fn builds_and_runs_host_c_project() {
    let tmp = std::env::temp_dir().join(format!("ns-run-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let project = Project::create(NewProjectSpec {
        name: "run_smoke".to_string(),
        location: tmp.clone(),
        language: Language::C,
        compiler: "clang".to_string(),
        simulator: String::new(),
        debugger: String::new(),
        flasher: String::new(),
        target: String::new(),
        svd: String::new(),
        c_template: None,
    })
    .expect("create project");

    // The default starter template loops forever; install a terminating
    // `main.c` so the spawned artifact actually exits and `child.wait()` can
    // complete within the test timeout.
    std::fs::write(
        project.root.join("src/main.c"),
        r#"#include <stdio.h>
int main(void) {
    printf("hello-run\n");
    return 0;
}
"#,
    )
    .expect("write main.c");

    let plan = ns_build::plan_build(&project).expect("plan build");
    let outcome = ns_build::execute(&plan);
    assert!(outcome.success, "build failed:\n{}", outcome.combined_output());

    let artifact = project.artifact_path();
    assert!(artifact.exists(), "artifact missing: {}", artifact.display());

    let mut child = Command::new(&artifact)
        .current_dir(&project.root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn artifact");

    let stdout = child.stdout.take().expect("stdout");
    let mut output = String::new();
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        output.push_str(&line.unwrap());
        output.push('\n');
    }
    let status = child.wait().expect("wait");
    assert_eq!(status.code(), Some(0), "unexpected exit, output: {output}");
    assert!(
        output.contains("hello-run"),
        "expected hello-run in output, got: {output}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
