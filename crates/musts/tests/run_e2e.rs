//! End-to-end coverage for `musts run <task-id>` — executing a
//! deterministic built-in check and recording evidence from the real
//! result, without the agent re-running the command.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("musts").expect("musts binary not built")
}

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// A minimal, already-formatted cargo project with a root `cargo/fmt`
/// check. `cargo fmt --check` is fast, offline, and available wherever
/// the test suite runs.
fn cargo_fmt_workspace() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(
        &root.join("MUSTS.yml"),
        "version: 1\nchecks:\n  fmt:\n    uses: cargo/fmt\n",
    );
    write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"fixture\"\npath = \"src/main.rs\"\n",
    );
    // Correctly formatted so `cargo fmt --check` exits 0.
    write(
        &root.join("src/main.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    );
    dir
}

#[test]
#[serial]
fn run_executes_cargo_fmt_and_records_evidence() {
    let dir = cargo_fmt_workspace();
    let root = dir.path();

    // Issue the task.
    bin()
        .arg("--workspace")
        .arg(root)
        .arg("validate")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("cargo-fmt-root"));

    // `musts run` executes `cargo fmt --check`, sees exit 0, and records
    // evidence — no `--asset`, no `--text` from the agent.
    bin()
        .arg("--workspace")
        .arg(root)
        .arg("run")
        .arg("cargo-fmt-root")
        .assert()
        .success()
        .stdout(predicate::str::contains("cargo fmt --check` exited 0"))
        .stdout(predicate::str::contains("Evidence accepted"));

    // The loop is now clean for that check.
    bin()
        .arg("--workspace")
        .arg(root)
        .arg("validate")
        .assert()
        .success()
        .code(0)
        .stdout(predicate::str::contains("Musts validation clean."));

    // Evidence was NOT archived: no `.musts/evidence/` tree is created.
    assert!(
        !root.join(".musts/evidence").exists(),
        "musts should no longer archive evidence submissions"
    );
}

#[test]
#[serial]
fn run_reports_a_failing_command_without_recording() {
    let dir = cargo_fmt_workspace();
    let root = dir.path();
    // Break the formatting so `cargo fmt --check` exits non-zero.
    write(&root.join("src/main.rs"), "fn main(){println!(\"hi\");}\n");

    bin()
        .arg("--workspace")
        .arg(root)
        .arg("validate")
        .assert()
        .failure()
        .code(1);

    // run surfaces the failure (exit 1) and does not record evidence.
    bin()
        .arg("--workspace")
        .arg(root)
        .arg("run")
        .arg("cargo-fmt-root")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("failed"));

    // Still pending — nothing was recorded.
    bin()
        .arg("--workspace")
        .arg(root)
        .arg("validate")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("cargo-fmt-root"));
}

#[test]
#[serial]
fn run_refuses_a_judgment_task() {
    // An `agent` check has no runnable command; `musts run` must refuse
    // and point the agent at `musts evidence`.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(
        &root.join("MUSTS.yml"),
        "version: 1\nchecks:\n  facts:\n    uses: agent\n    with:\n      facts:\n        - \"It works.\"\n",
    );
    write(&root.join("code.txt"), "x\n");

    bin()
        .arg("--workspace")
        .arg(root)
        .arg("validate")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("agent-root"));

    bin()
        .arg("--workspace")
        .arg(root)
        .arg("run")
        .arg("agent-root")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("no runnable command"))
        .stderr(predicate::str::contains("musts evidence"));
}

#[test]
#[serial]
fn run_rejects_unknown_task_id() {
    let dir = cargo_fmt_workspace();
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("run")
        .arg("does-not-exist")
        .assert()
        .failure()
        .stderr(predicate::str::contains("does-not-exist"));
}
