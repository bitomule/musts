//! End-to-end coverage for the built-in `agent` capability per
//! `docs/PLAN.md` §6.0.
//!
//! No `.musts/extensions/` setup is required for any of these
//! scenarios — the whole point of the built-in is that a fresh
//! workspace with one manifest can run the loop straight away.

mod common;

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("musts").expect("musts binary not built")
}

fn write_manifest(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

#[test]
#[serial]
fn agent_capability_no_extensions_needed() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("MUSTS.yml"),
        r#"version: 1
checks:
  login-form:
    uses: agent
    with:
      facts:
        - "Login form shows an error when the email is empty."
        - "Password field is masked."
"#,
    );

    // validate emits the task.
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("1. agent-root"))
        .stdout(predicate::str::contains("Login form shows an error"))
        .stdout(predicate::str::contains("Password field is masked"));

    // evidence with just text → accept.
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg("agent-root")
        .arg("--text")
        .arg("Manually verified both facts.")
        .assert()
        .success()
        .stdout(predicate::str::contains("Evidence accepted"));

    // next validate is clean.
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .success()
        .stdout(predicate::str::contains("Musts validation clean."));
}

#[test]
#[serial]
fn agent_text_required() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("MUSTS.yml"),
        r#"version: 1
checks:
  c:
    uses: agent
    with:
      facts:
        - "Whatever."
"#,
    );
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(1);

    // No --text → rejected with the "Provide a text summary" message.
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg("agent-root")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("Provide a text summary"));
}

#[test]
#[serial]
fn agent_accepts_arbitrary_assets() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("MUSTS.yml"),
        r#"version: 1
checks:
  c:
    uses: agent
    with:
      facts:
        - "Verified the UI."
"#,
    );
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(1);

    // Stage a screenshot and a log OUTSIDE the workspace so we don't
    // mutate scope hashes.
    let assets = TempDir::new().unwrap();
    let shot = assets.path().join("ui.png");
    fs::write(&shot, [0x89, 0x50, 0x4E, 0x47]).unwrap();
    let log = assets.path().join("notes.txt");
    fs::write(&log, b"first impression: looks fine").unwrap();

    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg("agent-root")
        .arg("--text")
        .arg("ok")
        .arg("--asset")
        .arg(&shot)
        .arg("--asset")
        .arg(&log)
        .assert()
        .success();

    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .success();
}

#[test]
#[serial]
fn agent_schema_rejects_empty_facts() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("MUSTS.yml"),
        r#"version: 1
checks:
  c:
    uses: agent
    with:
      facts: []
"#,
    );
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("schema"));
}

#[test]
#[serial]
fn external_descriptor_shadows_builtin_agent() {
    // A workspace can override the built-in `agent` by shipping an
    // extension that also declares `uses: agent`. The descriptor's
    // resolve must be invoked instead of the built-in's, producing a
    // distinctive task_id we can assert on.
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("MUSTS.yml"),
        r#"version: 1
checks:
  c:
    uses: agent
    with:
      facts: ["F"]
"#,
    );

    // Point the descriptor at the stub-extension binary which emits
    // a task id of "stub-task" (built-in would emit "agent-root").
    let stub = common::workspace_binary("stub-extension", "stub-extension");
    let ext = dir.path().join(".musts/extensions/agent");
    fs::create_dir_all(&ext).unwrap();
    fs::write(
        ext.join("extension.yml"),
        format!(
            r#"name: agent
version: 0.1.0
capabilities:
  agent:
    uses: agent
    resolve:
      command: [{bin:?}, "resolve"]
    evidence:
      command: [{bin:?}, "evidence"]
"#,
            bin = stub.display().to_string(),
        ),
    )
    .unwrap();

    let out = bin()
        .env_remove("MUSTS_STUB_RESOLVE_MODE")
        .env_remove("MUSTS_STUB_RESOLVE_SHAPE")
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .arg("--json")
        .assert()
        .failure()
        .code(1);
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let id = v["tasks"][0]["id"].as_str().unwrap();
    assert_eq!(
        id, "stub-task",
        "external descriptor must shadow the built-in (got: {id})"
    );
}

#[test]
#[serial]
fn agent_groups_two_checks_in_same_scope() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("MUSTS.yml"),
        r#"version: 1
checks:
  visual:
    uses: agent
    with:
      facts:
        - "Buttons are aligned."
  copy:
    uses: agent
    with:
      facts:
        - "Headline is in title case."
"#,
    );
    let out = bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .arg("--json")
        .assert()
        .failure()
        .code(1);
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["tasks"].as_array().unwrap().len(), 1);
    let satisfies: Vec<String> = v["tasks"][0]["satisfies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().into())
        .collect();
    assert!(satisfies.contains(&"root/visual".to_string()));
    assert!(satisfies.contains(&"root/copy".to_string()));
}
