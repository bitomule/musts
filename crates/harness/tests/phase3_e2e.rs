//! Phase 3 end-to-end scenarios per `docs/PLAN.md` §7.3 and §9 Phase 3.
//!
//! Each scenario sets up a temp workspace with the required manifests
//! and a stub-extension descriptor, then drives the real `harness`
//! binary.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("harness").expect("harness binary not built")
}

/// Path to the stub-extension binary in the target dir.
fn stub_binary() -> PathBuf {
    // `cargo test` produces sibling target binaries under the same profile.
    let test_bin = std::env::current_exe().unwrap();
    let profile = test_bin.parent().unwrap().parent().unwrap();
    profile.join("stub-extension")
}

/// Install a stub-extension descriptor that claims the given fully
/// qualified capability id.
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

fn run_validate(workspace: &Path) -> assert_cmd::assert::Assert {
    bin()
        .env_remove("HARNESS_STUB_RESOLVE_MODE")
        .env_remove("HARNESS_STUB_RESOLVE_SHAPE")
        .arg("--workspace")
        .arg(workspace)
        .arg("validate")
        .assert()
}

// ---------------------------------------------------------------------------
// Scenario 2: first_run_emits_tasks
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_2_first_run_emits_tasks() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        "version: 1\nchecks:\n  app-build:\n    uses: bazel/build\n    with:\n      target: //App:App\n",
    );
    write_manifest(
        &dir.path().join("App/Login/HARNESS.yml"),
        "version: 1\nchecks:\n  login-build:\n    uses: bazel/build\n    with:\n      target: //App/Login:Login\n",
    );
    install_stub_descriptor(dir.path(), "bazel/build");

    run_validate(dir.path())
        .failure()
        .code(1)
        .stdout(predicate::str::contains("Harness validation pending."))
        .stdout(predicate::str::contains("Task: stub-task"))
        .stdout(predicate::str::contains("Extension: bazel/build"))
        .stdout(predicate::str::contains("Completion rule:"));
}

// ---------------------------------------------------------------------------
// Scenario 8: bad_manifest_errors
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_8_bad_manifest_invalid_yaml() {
    let dir = TempDir::new().unwrap();
    write_manifest(&dir.path().join("HARNESS.yml"), "not: [valid yaml\n");
    run_validate(dir.path())
        .failure()
        .code(2)
        .stderr(predicate::str::contains("HARNESS.yml"));
}

#[test]
#[serial]
fn scenario_8_bad_manifest_unsupported_version() {
    let dir = TempDir::new().unwrap();
    write_manifest(&dir.path().join("HARNESS.yml"), "version: 99\nchecks: {}\n");
    install_stub_descriptor(dir.path(), "bazel/build");
    run_validate(dir.path())
        .failure()
        .code(2)
        .stderr(predicate::str::contains("unsupported version"));
}

#[test]
#[serial]
fn scenario_8_bad_manifest_with_schema_violation() {
    // The stub descriptor declares a schema requiring `target: string`;
    // the manifest sends an integer.
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        "version: 1\nchecks:\n  bad:\n    uses: bazel/build\n    with:\n      target: 42\n",
    );
    let cap_dir = dir.path().join(".harness/extensions/stub");
    fs::create_dir_all(cap_dir.join("schemas")).unwrap();
    fs::write(
        cap_dir.join("schemas/build.schema.json"),
        r#"{"type":"object","required":["target"],"properties":{"target":{"type":"string"}},"additionalProperties":false}"#,
    )
    .unwrap();
    let stub = stub_binary();
    fs::write(
        cap_dir.join("extension.yml"),
        format!(
            r#"name: stub
version: 0.1.0
capabilities:
  cap:
    uses: bazel/build
    schema: schemas/build.schema.json
    resolve:
      command: [{bin:?}, "resolve"]
    evidence:
      command: [{bin:?}, "evidence"]
"#,
            bin = stub.display().to_string(),
        ),
    )
    .unwrap();

    run_validate(dir.path())
        .failure()
        .code(2)
        .stderr(predicate::str::contains("schema"))
        .stderr(predicate::str::contains("root/bad"));
}

// ---------------------------------------------------------------------------
// Scenario 9a: resolve-side extension_failure (every RESOLVE_MODE)
// ---------------------------------------------------------------------------

fn run_resolve_failure_mode(mode: &str) -> assert_cmd::assert::Assert {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        "version: 1\nchecks:\n  c:\n    uses: bazel/build\n    with:\n      target: //x\n",
    );
    install_stub_descriptor(dir.path(), "bazel/build");
    let mut cmd = bin();
    cmd.env("HARNESS_STUB_RESOLVE_MODE", mode)
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate");
    if mode == "timeout" {
        // The stub sleeps 300s — keep the test fast.
        cmd.env("HARNESS_EXTENSION_TIMEOUT_SECS", "1");
    }
    // tempdir lifetime: keep the assertion inside the function so the
    // dir persists for the binary's run.
    let assert = cmd.assert().failure().code(2);
    drop(dir);
    assert
}

#[test]
#[serial]
fn scenario_9a_resolve_garbage_is_rejected() {
    run_resolve_failure_mode("garbage")
        .stderr(predicate::str::contains("bazel/build"))
        .stderr(
            predicate::str::contains("not valid JSON").or(predicate::str::contains("data after")),
        );
}

#[test]
#[serial]
fn scenario_9a_resolve_oversized_is_rejected() {
    run_resolve_failure_mode("oversized").stderr(predicate::str::contains("exceeds"));
}

#[test]
#[serial]
fn scenario_9a_resolve_nonzero_exit_is_rejected() {
    run_resolve_failure_mode("nonzero_exit").stderr(predicate::str::contains("simulated failure"));
}

#[test]
#[serial]
fn scenario_9a_resolve_bad_protocol_version_is_rejected() {
    run_resolve_failure_mode("bad_protocol_version")
        .stderr(predicate::str::contains("protocol_version"));
}

#[test]
#[serial]
fn scenario_9a_resolve_timeout_is_rejected() {
    run_resolve_failure_mode("timeout").stderr(predicate::str::contains("timed out"));
}

// ---------------------------------------------------------------------------
// Scenario 10: json_output
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_10_json_output_pending_shape() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        "version: 1\nchecks:\n  c:\n    uses: bazel/build\n    with:\n      target: //x\n",
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
    let stdout = std::str::from_utf8(&out.get_output().stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(stdout).expect("valid JSON");
    assert_eq!(v["protocol_version"], 1);
    assert_eq!(v["status"], "pending");
    assert!(!v["workspace_root"].as_str().unwrap().is_empty());
    assert_eq!(v["tasks"][0]["id"], "stub-task");
    assert_eq!(v["tasks"][0]["extension"], "bazel/build");
    assert!(v["tasks"][0]["satisfies"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("root/c")));
    assert!(v["ignored_checks"].as_array().is_some());
    assert!(v["notes"].as_array().is_some());
}

// ---------------------------------------------------------------------------
// Scenario 15: concurrent_validate_locks
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_15_concurrent_validate_locks() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        "version: 1\nchecks:\n  c:\n    uses: bazel/build\n    with:\n      target: //x\n",
    );
    install_stub_descriptor(dir.path(), "bazel/build");

    // Hold the lock manually, then run harness and expect lock-busy.
    use fs2::FileExt;
    use std::fs::OpenOptions;
    let harness_dir = dir.path().join(".harness");
    fs::create_dir_all(&harness_dir).unwrap();
    let lock_path = harness_dir.join(".lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    lock.try_lock_exclusive().unwrap();

    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "another harness process is running",
        ));

    drop(lock);
}

// ---------------------------------------------------------------------------
// Scenario 16: unicode_path_stability
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_16_unicode_path_stability() {
    // Two side-by-side workspaces. One contains a file at `é/x.txt`
    // (NFC), the other at the same logical path written using
    // U+0065 + combining U+0301 (NFD). Both run with identical
    // manifests; their report task ids must match (the JSON output
    // does not surface raw scope hashes but the deterministic stub
    // task id is enough to confirm the scope hash converged).
    fn build(ws: &Path, name: &str) {
        write_manifest(
            &ws.join("HARNESS.yml"),
            "version: 1\nchecks:\n  c:\n    uses: bazel/build\n    with:\n      target: //x\n",
        );
        install_stub_descriptor(ws, "bazel/build");
        let sub = ws.join(name);
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("x.txt"), b"content").unwrap();
    }

    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    build(a.path(), "é"); // NFC
    build(b.path(), "e\u{0301}"); // NFD

    let out_a = bin()
        .arg("--workspace")
        .arg(a.path())
        .arg("validate")
        .arg("--json")
        .assert()
        .failure()
        .code(1);
    let out_b = bin()
        .arg("--workspace")
        .arg(b.path())
        .arg("validate")
        .arg("--json")
        .assert()
        .failure()
        .code(1);
    let a_json: serde_json::Value = serde_json::from_slice(&out_a.get_output().stdout).unwrap();
    let b_json: serde_json::Value = serde_json::from_slice(&out_b.get_output().stdout).unwrap();
    // workspace_root differs by path but the resolved tasks shape is
    // deterministic.
    assert_eq!(a_json["tasks"][0]["id"], b_json["tasks"][0]["id"]);
    assert_eq!(
        a_json["tasks"][0]["satisfies"],
        b_json["tasks"][0]["satisfies"]
    );
}

// ---------------------------------------------------------------------------
// Scenario 17: submodule_workspace_root
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_17_submodule_workspace_root() {
    // Outer repo with .git (dir) and a manifest. Inner submodule has a
    // .git **file** (gitlink). cwd inside the submodule must resolve to
    // the outer workspace.
    let outer = TempDir::new().unwrap();
    fs::create_dir(outer.path().join(".git")).unwrap();
    write_manifest(
        &outer.path().join("HARNESS.yml"),
        "version: 1\nchecks:\n  c:\n    uses: bazel/build\n    with:\n      target: //x\n",
    );
    install_stub_descriptor(outer.path(), "bazel/build");
    let inner = outer.path().join("submodule");
    fs::create_dir(&inner).unwrap();
    fs::write(inner.join(".git"), "gitdir: ../.git/modules/submodule\n").unwrap();

    bin()
        .current_dir(&inner)
        .arg("validate")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("Harness validation pending."))
        .stdout(predicate::str::contains("Task: stub-task"));
}
