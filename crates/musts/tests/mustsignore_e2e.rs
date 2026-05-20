//! End-to-end coverage for the workspace-level `.mustsignore` file.
//!
//! `.mustsignore` is `.gitignore` for musts: matched files are excluded
//! from the walker that builds a check's scope hash, so edits to local
//! logs, scratch artefacts, or canonical fixtures don't re-invalidate
//! the validated state stored in the ledger lock.

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

fn stub_manifest() -> &'static [u8] {
    br#"version: 1
checks:
  build:
    uses: bazel/build
    with:
      target: //x
"#
}

// ---------------------------------------------------------------------------
// Scenario: edits to a `.mustsignore`-matched file leave the check green.
// Removing the rule re-arms the check so editing the same file reopens it.
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn mustsignore_excludes_files_from_scope_hash() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    write_file(&root.join("MUSTS.yml"), stub_manifest());
    install_stub_descriptor(root, "bazel/build");
    write_file(&root.join("App/Login.swift"), b"// login\n");
    write_file(&root.join(".mustsignore"), b"*.log\n");
    // Pre-existing log file; matched by .mustsignore so it must not enter
    // the scope hash.
    write_file(&root.join("scratch.log"), b"first log\n");

    // Initial dirty → green.
    run_validate(root)
        .failure()
        .code(1)
        .stdout(predicate::str::contains("1. stub-task"));
    run_evidence(root, "stub-task").assert().success();
    run_validate(root)
        .success()
        .code(0)
        .stdout(predicate::str::contains("Musts validation clean."));

    // Mutating the ignored file MUST NOT re-invalidate the check.
    fs::write(root.join("scratch.log"), b"more log\n").unwrap();
    run_validate(root)
        .success()
        .code(0)
        .stdout(predicate::str::contains("Musts validation clean."));

    // Sanity: a non-ignored file still re-invalidates.
    fs::write(root.join("App/Login.swift"), b"// touched\n").unwrap();
    run_validate(root)
        .failure()
        .code(1)
        .stdout(predicate::str::contains("1. stub-task"));
}

// ---------------------------------------------------------------------------
// Scenario: removing `.mustsignore` re-includes previously ignored files in
// the scope hash. Editing them after the removal flips the check dirty.
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn removing_mustsignore_re_includes_files() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    write_file(&root.join("MUSTS.yml"), stub_manifest());
    install_stub_descriptor(root, "bazel/build");
    write_file(&root.join("App/Login.swift"), b"// login\n");
    write_file(&root.join(".mustsignore"), b"*.log\n");
    write_file(&root.join("scratch.log"), b"hidden\n");

    run_validate(root).failure().code(1);
    run_evidence(root, "stub-task").assert().success();
    run_validate(root).success().code(0);

    // Drop the ignore rule. The previously-hidden file now contributes to
    // the scope hash, so the next validate sees a new scope_hash and the
    // check goes dirty even without any further file edits.
    fs::remove_file(root.join(".mustsignore")).unwrap();
    run_validate(root)
        .failure()
        .code(1)
        .stdout(predicate::str::contains("1. stub-task"));
}

// ---------------------------------------------------------------------------
// Scenario: a nested `.mustsignore` scopes only to its subtree, identical
// to nested `.gitignore` behaviour.
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn nested_mustsignore_scopes_to_subtree() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    write_file(&root.join("MUSTS.yml"), stub_manifest());
    install_stub_descriptor(root, "bazel/build");
    write_file(&root.join("App/Login.swift"), b"// login\n");
    // Ignore rule only inside `App/`.
    write_file(&root.join("App/.mustsignore"), b"*.log\n");
    // Inside the subtree: ignored.
    write_file(&root.join("App/local.log"), b"x\n");
    // Outside the subtree: NOT ignored — a sibling top-level .log must
    // still contribute to the scope hash.
    write_file(&root.join("top.log"), b"y\n");

    run_validate(root).failure().code(1);
    run_evidence(root, "stub-task").assert().success();
    run_validate(root).success().code(0);

    // App/local.log is ignored → no rehash.
    fs::write(root.join("App/local.log"), b"z\n").unwrap();
    run_validate(root).success().code(0);

    // top.log is NOT ignored (the nested rule only applies inside App/).
    fs::write(root.join("top.log"), b"q\n").unwrap();
    run_validate(root)
        .failure()
        .code(1)
        .stdout(predicate::str::contains("1. stub-task"));
}

// ---------------------------------------------------------------------------
// Scenario: negation (`!pattern`) brings back specific files. Same
// gotcha as `.gitignore`: negation can only re-include a file whose
// parent dir wasn't excluded, so the working idiom is to ignore a
// file-pattern and negate a specific filename.
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn mustsignore_negation_re_includes_specific_files() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    write_file(&root.join("MUSTS.yml"), stub_manifest());
    install_stub_descriptor(root, "bazel/build");
    write_file(&root.join("App/Login.swift"), b"// login\n");
    write_file(&root.join(".mustsignore"), b"*.junk\n!keep.junk\n");
    write_file(&root.join("scratch/noise.junk"), b"n\n");
    write_file(&root.join("scratch/keep.junk"), b"k\n");

    run_validate(root).failure().code(1);
    run_evidence(root, "stub-task").assert().success();
    run_validate(root).success().code(0);

    // Editing a globally-ignored file does not reopen the check.
    fs::write(root.join("scratch/noise.junk"), b"nn\n").unwrap();
    run_validate(root).success().code(0);

    // Editing a negated-back file DOES reopen the check.
    fs::write(root.join("scratch/keep.junk"), b"kk\n").unwrap();
    run_validate(root)
        .failure()
        .code(1)
        .stdout(predicate::str::contains("1. stub-task"));
}
