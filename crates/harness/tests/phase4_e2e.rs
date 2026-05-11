//! Phase 4 end-to-end scenarios per `docs/PLAN.md` §7.3 and §9 Phase 4.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("harness").expect("harness binary not built")
}

fn stub_binary() -> PathBuf {
    common::workspace_binary("stub-extension", "stub-extension")
}

fn install_stub_descriptor(workspace: &Path, capability_uses: &str) {
    let dir = workspace.join(".harness/extensions/stub");
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

fn write_manifest(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn build_basic_workspace() -> TempDir {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        "version: 1\nchecks:\n  c:\n    uses: bazel/build\n    with:\n      target: //x\n",
    );
    install_stub_descriptor(dir.path(), "bazel/build");
    dir
}

fn run_validate(workspace: &Path) -> assert_cmd::assert::Assert {
    bin()
        .env_remove("HARNESS_STUB_RESOLVE_MODE")
        .env_remove("HARNESS_STUB_RESOLVE_SHAPE")
        .env_remove("HARNESS_STUB_EVIDENCE_MODE")
        .env_remove("HARNESS_STUB_EVIDENCE_SHAPE")
        .arg("--workspace")
        .arg(workspace)
        .arg("validate")
        .assert()
}

fn run_evidence(workspace: &Path, task_id: &str) -> assert_cmd::Command {
    let mut cmd = bin();
    cmd.env_remove("HARNESS_STUB_RESOLVE_MODE")
        .env_remove("HARNESS_STUB_RESOLVE_SHAPE")
        .arg("--workspace")
        .arg(workspace)
        .arg("evidence")
        .arg(task_id)
        .arg("--text")
        .arg("ok");
    cmd
}

// ---------------------------------------------------------------------------
// Scenario 1: clean_repo_clean_report
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_1_clean_after_evidence() {
    let dir = build_basic_workspace();
    run_validate(dir.path()).failure().code(1);
    run_evidence(dir.path(), "stub-task").assert().success();
    run_validate(dir.path())
        .success()
        .code(0)
        .stdout(predicate::str::contains("Harness validation clean."));
}

// ---------------------------------------------------------------------------
// Scenario 3: evidence_loop
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_3_evidence_loop_returns_clean() {
    let dir = build_basic_workspace();
    run_validate(dir.path()).failure().code(1);
    run_evidence(dir.path(), "stub-task")
        .assert()
        .success()
        .stdout(predicate::str::contains("Evidence accepted"));
    run_validate(dir.path()).success();
}

// ---------------------------------------------------------------------------
// Scenario 4: modify_file_reopens_task
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_4_modify_file_reopens_task() {
    let dir = build_basic_workspace();
    let touched = dir.path().join("src.txt");
    fs::write(&touched, b"first").unwrap();
    run_validate(dir.path()).failure().code(1);
    run_evidence(dir.path(), "stub-task").assert().success();
    run_validate(dir.path()).success();

    // Mutate the file; the next validate must reopen the task.
    fs::write(&touched, b"second").unwrap();
    run_validate(dir.path())
        .failure()
        .code(1)
        .stdout(predicate::str::contains("Task: stub-task"));
}

// ---------------------------------------------------------------------------
// Scenario 5: stale_evidence_rejected
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_5_stale_evidence_rejected() {
    let dir = build_basic_workspace();
    let watched = dir.path().join("watched.txt");
    fs::write(&watched, b"v1").unwrap();
    run_validate(dir.path()).failure().code(1);

    // Modify the watched file before recording evidence.
    fs::write(&watched, b"v2").unwrap();

    run_evidence(dir.path(), "stub-task")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("stale"));
}

// ---------------------------------------------------------------------------
// Scenario 9b: evidence-side extension_failure (every EVIDENCE_MODE)
// ---------------------------------------------------------------------------

fn run_evidence_failure_mode(mode: &str) -> assert_cmd::assert::Assert {
    let dir = build_basic_workspace();
    run_validate(dir.path()).failure().code(1);
    let mut cmd = bin();
    cmd.env("HARNESS_STUB_EVIDENCE_MODE", mode)
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg("stub-task")
        .arg("--text")
        .arg("x");
    if mode == "timeout" {
        cmd.env("HARNESS_EXTENSION_TIMEOUT_SECS", "1");
    }
    let assert = cmd.assert();
    drop(dir);
    assert
}

#[test]
#[serial]
fn scenario_9b_evidence_garbage_is_rejected() {
    run_evidence_failure_mode("garbage")
        .failure()
        .code(2)
        .stderr(
            predicate::str::contains("not valid JSON").or(predicate::str::contains("data after")),
        );
}

#[test]
#[serial]
fn scenario_9b_evidence_oversized_is_rejected() {
    run_evidence_failure_mode("oversized")
        .failure()
        .code(2)
        .stderr(predicate::str::contains("exceeds"));
}

#[test]
#[serial]
fn scenario_9b_evidence_nonzero_exit_is_rejected() {
    run_evidence_failure_mode("nonzero_exit")
        .failure()
        .code(2)
        .stderr(predicate::str::contains("simulated"));
}

#[test]
#[serial]
fn scenario_9b_evidence_bad_protocol_version_is_rejected() {
    run_evidence_failure_mode("bad_protocol_version")
        .failure()
        .code(2)
        .stderr(predicate::str::contains("protocol_version"));
}

#[test]
#[serial]
fn scenario_9b_evidence_timeout_is_rejected() {
    run_evidence_failure_mode("timeout")
        .failure()
        .code(2)
        .stderr(predicate::str::contains("timed out"));
}

// ---------------------------------------------------------------------------
// Scenario 11: partial_accept
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_11_partial_accept_leaves_unlisted_pending() {
    // Two checks under the same scope so the stub's default shape
    // emits one task that satisfies both.
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        "version: 1\nchecks:\n  a:\n    uses: bazel/build\n    with:\n      target: //a\n  b:\n    uses: bazel/build\n    with:\n      target: //b\n",
    );
    install_stub_descriptor(dir.path(), "bazel/build");

    run_validate(dir.path()).failure().code(1);

    // accept_subset: stub returns satisfies with only the first id.
    bin()
        .env("HARNESS_STUB_EVIDENCE_SHAPE", "accept_subset")
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg("stub-task")
        .arg("--text")
        .arg("partial")
        .assert()
        .success();

    // Next validate must still emit a task for the unlisted check.
    let out = run_validate(dir.path())
        .failure()
        .code(1)
        .stdout(predicate::str::contains("Harness validation pending."));
    let stdout = std::str::from_utf8(&out.get_output().stdout).unwrap();
    assert!(stdout.contains("root/a") || stdout.contains("root/b"));
}

// ---------------------------------------------------------------------------
// Scenario 12: same_local_id_two_manifests
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_12_same_local_id_two_manifests() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        "version: 1\nchecks:\n  login-build:\n    uses: bazel/build\n    with:\n      target: //x\n",
    );
    write_manifest(
        &dir.path().join("App/Login/HARNESS.yml"),
        "version: 1\nchecks:\n  login-build:\n    uses: bazel/build\n    with:\n      target: //App/Login:Login\n",
    );
    install_stub_descriptor(dir.path(), "bazel/build");

    let out = bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .arg("--json")
        .assert()
        .failure()
        .code(1);
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let satisfies = v["tasks"][0]["satisfies"].as_array().unwrap();
    let ids: Vec<String> = satisfies
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&"root/login-build".to_string()), "{ids:?}");
    assert!(
        ids.contains(&"App/Login/login-build".to_string()),
        "{ids:?}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 13: unrelated_edit_does_not_stale
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_13_unrelated_edit_does_not_stale() {
    let dir = TempDir::new().unwrap();
    // Two manifests with the SAME capability; sibling scopes.
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        "version: 1\nchecks:\n  app:\n    uses: bazel/build\n    with:\n      target: //x\n",
    );
    write_manifest(
        &dir.path().join("Other/HARNESS.yml"),
        "version: 1\nchecks:\n  other:\n    uses: bazel/build\n    with:\n      target: //o\n",
    );
    install_stub_descriptor(dir.path(), "bazel/build");

    // multi_task: emit one task per check so we can pick the deeper one.
    bin()
        .env("HARNESS_STUB_RESOLVE_SHAPE", "multi_task")
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(1);

    // Modify a file in the *Other* scope after the task list was issued.
    fs::write(dir.path().join("Other/unrelated.txt"), b"changed").unwrap();

    // Submit evidence for the root task (task id `stub-task-0` from
    // multi_task — order is BTreeMap, capability iterates sorted).
    // Find the root task by id.
    let out = bin()
        .env("HARNESS_STUB_RESOLVE_SHAPE", "multi_task")
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .arg("--json")
        .assert()
        .failure()
        .code(1);
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let tasks = v["tasks"].as_array().unwrap();
    let mut root_task_id = None;
    for t in tasks {
        let satisfies = t["satisfies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        if satisfies.contains(&"root/app".to_string()) {
            root_task_id = Some(t["id"].as_str().unwrap().to_string());
        }
    }
    let root_task_id = root_task_id.expect("root task present");

    // Modify a different unrelated scope ("Other") *after* validate
    // issued the root task. Root task's effective scope carves out
    // Other/ because Other has a same-capability manifest. So evidence
    // for the root task should still be accepted.
    fs::write(dir.path().join("Other/unrelated.txt"), b"changed-again").unwrap();

    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg(&root_task_id)
        .arg("--text")
        .arg("ok")
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// Scenario 14: stale_task_id_rejected
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_14_stale_task_id_rejected() {
    let dir = build_basic_workspace();
    run_validate(dir.path()).failure().code(1);
    // Re-run validate with `ignore_all` so the second validate emits
    // zero tasks and the `tasks` table is truncated to empty.
    bin()
        .env("HARNESS_STUB_RESOLVE_SHAPE", "ignore_all")
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .success()
        .code(0);

    // The original task id no longer exists in the tasks table.
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg("stub-task")
        .arg("--text")
        .arg("x")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("no longer applies"));
}
