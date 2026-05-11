//! PLAN.md §7.3 scenarios 19, 20, 21 — extension-presence and state-dir
//! readiness paths. Phase 4 territory (handled by core + bootstrap).

mod common;

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("harness").expect("harness binary not built")
}

fn write_manifest(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

// ---------------------------------------------------------------------------
// Scenario 19: missing_extension_binary
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_19_missing_extension_binary() {
    // Descriptor points at a binary path that doesn't exist. Per
    // PLAN.md §5 / Error::ExtensionFailure the error should mention
    // the descriptor and the missing program.
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        "version: 1\nchecks:\n  c:\n    uses: bazel/build\n    with:\n      target: //x\n",
    );
    let ext_dir = dir.path().join(".harness/extensions/bazel");
    fs::create_dir_all(&ext_dir).unwrap();
    fs::write(
        ext_dir.join("extension.yml"),
        r#"name: bazel
version: 0.1.0
capabilities:
  build:
    uses: bazel/build
    resolve:
      command: ["does-not-exist-binary", "resolve"]
    evidence:
      command: ["does-not-exist-binary", "evidence"]
"#,
    )
    .unwrap();

    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(2)
        .stderr(
            predicate::str::contains("could not spawn")
                .and(predicate::str::contains("does-not-exist-binary")),
        );
}

// ---------------------------------------------------------------------------
// Scenario 20: empty_extensions_dir
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_20_empty_extensions_dir() {
    // `.harness/extensions/` exists but is empty; manifests with
    // checks must fail with the scenario-18-style "no extension
    // implements capability X" message.
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        "version: 1\nchecks:\n  c:\n    uses: bazel/build\n    with:\n      target: //x\n",
    );
    fs::create_dir_all(dir.path().join(".harness/extensions")).unwrap();

    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "no extension implements capability `bazel/build`",
        ))
        .stderr(predicate::str::contains("root/c"));
}

// ---------------------------------------------------------------------------
// Scenario 21: readonly_state_dir
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
#[serial]
fn scenario_21_readonly_state_dir() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        "version: 1\nchecks:\n  c:\n    uses: bazel/build\n    with:\n      target: //x\n",
    );
    // Pre-create .harness/ and chmod it read-only so the write probe
    // in bootstrap returns PermissionDenied.
    let harness_dir = dir.path().join(".harness");
    fs::create_dir_all(&harness_dir).unwrap();
    let mut perms = fs::metadata(&harness_dir).unwrap().permissions();
    perms.set_mode(0o555);
    fs::set_permissions(&harness_dir, perms).unwrap();

    let result = bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(2);
    let _ = result;

    // Restore writable so TempDir can clean up.
    let mut perms = fs::metadata(&harness_dir).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&harness_dir, perms).unwrap();
}
