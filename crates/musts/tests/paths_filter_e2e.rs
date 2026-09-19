//! End-to-end coverage for the per-check `paths:` glob filter
//! (declared in `MUSTS.yml`). The filter narrows a check's effective
//! scope so changes to files outside the glob set don't make it dirty,
//! and a check whose filter currently matches nothing is dropped from
//! the task list entirely.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("musts").expect("musts binary not built")
}

fn stub_binary() -> PathBuf {
    common::workspace_binary("stub-extension", "stub-extension")
}

fn install_stub_descriptor(workspace: &Path, capability_uses: &str) {
    let dir = workspace.join(".musts/extensions/stub");
    fs::create_dir_all(&dir).unwrap();
    let stub = stub_binary();
    fs::write(
        dir.join("extension.yml"),
        format!(
            r#"name: stub
version: 0.1.0
capabilities:
  cap:
    uses: {capability_uses}
    resolve:
      command: [{bin:?}, "resolve"]
    evidence:
      command: [{bin:?}, "evidence"]
"#,
            bin = stub.display().to_string(),
        ),
    )
    .unwrap();
}

fn write_file(path: &Path, body: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn run_validate(workspace: &Path) -> assert_cmd::assert::Assert {
    bin()
        .env_remove("MUSTS_STUB_RESOLVE_MODE")
        .env_remove("MUSTS_STUB_RESOLVE_SHAPE")
        .arg("--workspace")
        .arg(workspace)
        .arg("validate")
        .assert()
}

fn run_evidence(workspace: &Path, task_id: &str) -> assert_cmd::Command {
    let mut cmd = bin();
    cmd.env_remove("MUSTS_STUB_RESOLVE_MODE")
        .env_remove("MUSTS_STUB_RESOLVE_SHAPE")
        .arg("--workspace")
        .arg(workspace)
        .arg("evidence")
        .arg(task_id)
        .arg("--text")
        .arg("ok");
    cmd
}

// ---------------------------------------------------------------------------
// Scenario: a check with `paths` matching files emits a task and goes green
// only after evidence; later edits to NON-matching files leave it green.
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn paths_filter_isolates_check_from_unrelated_file_changes() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    write_file(
        &root.join("MUSTS.yml"),
        br#"version: 1
checks:
  tracking-tests:
    uses: bazel/build
    paths:
      - "**/Tracking*.swift"
    with:
      target: //x
"#,
    );
    install_stub_descriptor(root, "bazel/build");
    write_file(
        &root.join("App/TrackingEvents.swift"),
        b"// initial tracking\n",
    );
    write_file(&root.join("App/Unrelated.swift"), b"// unrelated\n");

    // Initial run: a matching file exists, so the check is dirty.
    run_validate(root)
        .failure()
        .code(1)
        .stdout(predicate::str::contains("1. stub-task"));

    // Submit evidence to drive it green.
    run_evidence(root, "stub-task").assert().success();
    run_validate(root).success().code(0);

    // Edit a NON-matching file — the check stays green because the
    // path filter excluded it from the scope hash.
    fs::write(root.join("App/Unrelated.swift"), b"// touched\n").unwrap();
    run_validate(root)
        .success()
        .code(0)
        .stdout(predicate::str::contains("Musts validation clean."));

    // Edit a matching file — the check reopens.
    fs::write(root.join("App/TrackingEvents.swift"), b"// touched\n").unwrap();
    run_validate(root)
        .failure()
        .code(1)
        .stdout(predicate::str::contains("1. stub-task"));
}

// ---------------------------------------------------------------------------
// Scenario: `exclude_paths` carves a file out of scope so editing it does
// not re-open the check (the /beta version-bump churn the maintainer hit).
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn exclude_paths_isolates_check_from_excluded_file_changes() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    write_file(
        &root.join("MUSTS.yml"),
        br#"version: 1
checks:
  build:
    uses: bazel/build
    exclude_paths:
      - "tools/config.bzl"
    with:
      target: //x
"#,
    );
    install_stub_descriptor(root, "bazel/build");
    write_file(&root.join("App/Main.swift"), b"// app\n");
    write_file(&root.join("tools/config.bzl"), b"build_number = 35\n");

    // Initial run: in-scope files exist, so the check is dirty.
    run_validate(root)
        .failure()
        .code(1)
        .stdout(predicate::str::contains("1. stub-task"));

    // Drive it green.
    run_evidence(root, "stub-task").assert().success();
    run_validate(root).success().code(0);

    // Bump the excluded file (simulates the /beta build_number bump) —
    // the check stays green because it's outside the scope hash.
    fs::write(root.join("tools/config.bzl"), b"build_number = 36\n").unwrap();
    run_validate(root)
        .success()
        .code(0)
        .stdout(predicate::str::contains("Musts validation clean."));

    // Editing an in-scope file still reopens the check.
    fs::write(root.join("App/Main.swift"), b"// app changed\n").unwrap();
    run_validate(root)
        .failure()
        .code(1)
        .stdout(predicate::str::contains("1. stub-task"));
}

// ---------------------------------------------------------------------------
// Scenario: a `!`-negation pattern is rejected loudly at parse time rather
// than silently matching nothing.
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn bang_negation_pattern_is_rejected() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    write_file(
        &root.join("MUSTS.yml"),
        br#"version: 1
checks:
  build:
    uses: bazel/build
    paths:
      - "**/*.swift"
      - "!**/*Snapshot*.swift"
    with:
      target: //x
"#,
    );
    install_stub_descriptor(root, "bazel/build");
    write_file(&root.join("App/Main.swift"), b"// app\n");

    run_validate(root)
        .failure()
        .code(2)
        .stderr(predicate::str::contains("exclude_paths"));
}

// ---------------------------------------------------------------------------
// Scenario: a check whose `paths` matches no files is skipped entirely.
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn paths_filter_skips_check_when_no_files_match() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    write_file(
        &root.join("MUSTS.yml"),
        br#"version: 1
checks:
  tracking-tests:
    uses: bazel/build
    paths:
      - "**/Tracking*.swift"
    with:
      target: //x
"#,
    );
    install_stub_descriptor(root, "bazel/build");
    // Only non-matching files exist.
    write_file(&root.join("App/Login.swift"), b"// login\n");

    // No task should be emitted — and the run is clean.
    run_validate(root)
        .success()
        .code(0)
        .stdout(predicate::str::contains("Musts validation clean."));

    // Add a matching file: the check now activates.
    write_file(&root.join("App/TrackingEvents.swift"), b"// tracking\n");
    run_validate(root)
        .failure()
        .code(1)
        .stdout(predicate::str::contains("1. stub-task"));
}
