//! End-to-end coverage for `.musts/ledger/`, the portable validated-state
//! ledger (one file per check), and for migrating the older
//! `.musts/ledger.lock.yaml`.
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
    Command::cargo_bin("musts").expect("musts binary not built")
}

fn write_manifest(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

const LOCK_REL: &str = ".musts/ledger.lock.yaml";
const ENTRY_REL: &str = ".musts/ledger/root%2Fcontract";

#[test]
#[serial]
fn evidence_accept_writes_lock_file() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("MUSTS.yml"),
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

    let entry_path = dir.path().join(ENTRY_REL);
    assert!(
        entry_path.exists(),
        "a per-check ledger file must be written after an accepted submission"
    );
    // One file, one line: the hash the check is currently green for.
    let body = fs::read_to_string(&entry_path).unwrap();
    let hex = body.trim_end_matches('\n');
    assert_eq!(hex.len(), 64, "blake3 hex should be 64 chars (was {hex:?})");
    assert_eq!(body.lines().count(), 1, "{body:?}");

    // Nothing merge-related ships with the ledger, and the old
    // single-file lock is never written.
    assert!(!dir.path().join(".musts/.gitattributes").exists());
    assert!(!lock_path.exists());
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
        &dir.path().join("MUSTS.yml"),
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
    let state_db = dir.path().join(".musts/state.sqlite");
    assert!(state_db.exists(), "state.sqlite must exist at this point");
    fs::remove_file(&state_db).unwrap();
    for ext in ["state.sqlite-shm", "state.sqlite-wal"] {
        let p = dir.path().join(".musts").join(ext);
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
        .stdout(predicate::str::contains("Musts validation clean."));
}

#[test]
#[serial]
fn editing_a_file_invalidates_only_its_scope() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("MUSTS.yml"),
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
        &dir.path().join("sub/MUSTS.yml"),
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
    fs::remove_file(dir.path().join(".musts/state.sqlite")).unwrap();
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
        stdout.contains("1. agent-sub"),
        "expected agent-sub in report:\n{stdout}"
    );
    assert!(
        !stdout.contains("1. agent-root"),
        "agent-root should still be satisfied by the lock; got:\n{stdout}"
    );
}

#[test]
#[serial]
fn malformed_lock_file_is_a_configuration_error() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("MUSTS.yml"),
        r#"version: 1
checks:
  contract:
    uses: agent
    with:
      facts: ["A."]
"#,
    );
    fs::create_dir_all(dir.path().join(".musts")).unwrap();
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

/// A repo still carrying a pre-0.10.0 `ledger.lock.yaml` (history for
/// several trees, plus the `merge=union` rule musts wrote) is migrated by
/// the next `validate`: the hash that matches the tree today becomes the
/// check's file, the history and the old file are dropped, and the check
/// stays green with no evidence re-run.
#[test]
#[serial]
fn validate_migrates_an_old_lock_keeping_the_current_state() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("MUSTS.yml"),
        r#"version: 1
checks:
  contract:
    uses: agent
    with:
      facts:
        - "X is true."
"#,
    );
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure();
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg("agent-root")
        .arg("--text")
        .arg("Verified X.")
        .assert()
        .success();
    let current = fs::read_to_string(dir.path().join(ENTRY_REL))
        .unwrap()
        .trim()
        .to_string();

    let musts = dir.path().join(".musts");
    fs::remove_dir_all(musts.join("ledger")).unwrap();
    for name in ["state.sqlite", "state.sqlite-shm", "state.sqlite-wal"] {
        let _ = fs::remove_file(musts.join(name));
    }
    fs::write(
        dir.path().join(LOCK_REL),
        format!(
            "version: 1\nsatisfied:\n\
             - {{check: \"root/contract\", scope_hash: \"{old}\"}}\n\
             - {{check: \"root/contract\", scope_hash: \"{current}\"}}\n\
             - {{check: \"gone/check\", scope_hash: \"{old}\"}}\n",
            old = "0".repeat(64)
        ),
    )
    .unwrap();
    fs::write(
        musts.join(".gitattributes"),
        "# Managed by musts. The validated-state ledger is an append-only set, so\n\
         # taking both sides is always the right merge — a hand resolution would\n\
         # drop entries that were legitimately proven green on one branch.\n\
         ledger.lock.yaml merge=union\n",
    )
    .unwrap();

    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .success()
        .stdout(predicate::str::contains("Musts validation clean."));

    assert!(!dir.path().join(LOCK_REL).exists(), "old lock is retired");
    assert!(!musts.join(".gitattributes").exists());
    assert_eq!(
        fs::read_to_string(dir.path().join(ENTRY_REL)).unwrap(),
        format!("{current}\n")
    );
    let files: Vec<_> = fs::read_dir(musts.join("ledger")).unwrap().collect();
    assert_eq!(files.len(), 1, "history and undeclared checks are dropped");
}
