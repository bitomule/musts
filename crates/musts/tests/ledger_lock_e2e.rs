//! End-to-end coverage for `.harness/ledger.lock.yaml`, the portable
//! validated-state ledger.
//!
//! The lock file lets a clone inherit "already validated" state from
//! the repo: their fresh workspace has no `state.sqlite`, but the
//! committed lock answers the satisfaction question alongside it.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("musts").expect("harness binary not built")
}

fn write_manifest(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

const LOCK_REL: &str = ".harness/ledger.lock.yaml";

#[test]
#[serial]
fn evidence_accept_writes_lock_file() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        r#"version: 1
checks:
  contract:
    uses: agent
    with:
      facts:
        - "X is true."
"#,
    );
    let lock_path = dir.path().join(LOCK_REL);
    assert!(
        !lock_path.exists(),
        "lock file should not exist before any submission"
    );

    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(1);

    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg("agent-root")
        .arg("--text")
        .arg("Verified X.")
        .assert()
        .success();

    assert!(
        lock_path.exists(),
        "lock file must be written after accepted submission"
    );
    let body = fs::read_to_string(&lock_path).unwrap();
    assert!(body.contains("version: 1"));
    assert!(body.contains("root/contract"));
    // sha-like scope hash should be a 64-hex-char string.
    let hex = body
        .lines()
        .find(|l| l.contains("scope_hash:"))
        .expect("scope_hash line")
        .split(':')
        .nth(1)
        .unwrap()
        .trim();
    assert_eq!(hex.len(), 64, "blake3 hex should be 64 chars (was {hex:?})");
}

#[test]
#[serial]
fn fresh_clone_inherits_validated_state_via_lock() {
    // First, build up a workspace and submit evidence so the lock is
    // populated. Then simulate a "clone" by wiping `state.sqlite` and
    // re-running validate: the lock alone must be enough to mark the
    // check as already satisfied.
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        r#"version: 1
checks:
  contract:
    uses: agent
    with:
      facts:
        - "Y holds."
"#,
    );

    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(1);
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg("agent-root")
        .arg("--text")
        .arg("Verified Y.")
        .assert()
        .success();

    // The local SQLite ledger has the green row. Wipe it — this is what
    // a fresh `git clone` looks like.
    let state_db = dir.path().join(".harness/state.sqlite");
    assert!(state_db.exists(), "state.sqlite must exist at this point");
    fs::remove_file(&state_db).unwrap();
    for ext in ["state.sqlite-shm", "state.sqlite-wal"] {
        let p = dir.path().join(".harness").join(ext);
        if p.exists() {
            fs::remove_file(p).unwrap();
        }
    }
    // Evidence files would also be missing on a clone (.gitignore). But
    // they aren't consulted at validate time, only at submit time, so
    // their absence doesn't matter here.

    // First validate after the "clone" must be clean — the lock has the
    // entry and the scope_hash is unchanged.
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .success()
        .stdout(predicate::str::contains("Harness validation clean."));
}

#[test]
#[serial]
fn editing_a_file_invalidates_only_its_scope() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        r#"version: 1
checks:
  root-contract:
    uses: agent
    with:
      facts:
        - "Root invariant holds."
"#,
    );
    write_manifest(
        &dir.path().join("sub/HARNESS.yml"),
        r#"version: 1
checks:
  sub-contract:
    uses: agent
    with:
      facts:
        - "Sub invariant holds."
"#,
    );
    fs::write(dir.path().join("sub/source.txt"), "v1\n").unwrap();

    // Establish the green state for both scopes.
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(1);
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg("agent-root")
        .arg("--text")
        .arg("ok")
        .assert()
        .success();
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg("agent-sub")
        .arg("--text")
        .arg("ok")
        .assert()
        .success();
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .success();

    // Simulate clone (state.sqlite gone) and edit only sub/source.txt.
    fs::remove_file(dir.path().join(".harness/state.sqlite")).unwrap();
    fs::write(dir.path().join("sub/source.txt"), "v2\n").unwrap();

    let output = bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(1)
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Only the sub-scope check must reappear; root-scope stays clean
    // because its scope_hash didn't change.
    assert!(
        stdout.contains("Task: agent-sub"),
        "expected agent-sub in report:\n{stdout}"
    );
    assert!(
        !stdout.contains("Task: agent-root"),
        "agent-root should still be satisfied by the lock; got:\n{stdout}"
    );
}

#[test]
#[serial]
fn malformed_lock_file_is_a_configuration_error() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        r#"version: 1
checks:
  contract:
    uses: agent
    with:
      facts: ["A."]
"#,
    );
    fs::create_dir_all(dir.path().join(".harness")).unwrap();
    fs::write(dir.path().join(LOCK_REL), "::: not yaml :::").unwrap();

    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("ledger lock"));
}
