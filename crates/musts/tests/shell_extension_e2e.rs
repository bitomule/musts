//! End-to-end smoke test for `docs/examples/eslint-check/eslint-check.sh` —
//! proves the "an extension is any executable" promise from PLAN.md §6
//! and `docs/extensions.md` by driving a full validate→evidence→clean
//! loop against the bash script (not a Rust binary).
//!
//! Requires `jq` in $PATH. Skipped on hosts that don't have it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("musts").expect("musts binary not built")
}

fn has_jq() -> bool {
    StdCommand::new("jq")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn project_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("workspace root")
}

fn install_eslint_extension(workspace: &Path) {
    let dst = workspace.join(".musts/extensions/eslint");
    fs::create_dir_all(&dst).unwrap();
    let script_src = project_root().join("docs/examples/eslint-check/eslint-check.sh");
    let script_dst = dst.join("eslint-check.sh");
    fs::copy(&script_src, &script_dst).unwrap();
    // Re-set the executable bit — `fs::copy` preserves permissions on
    // unix but be defensive.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = fs::metadata(&script_dst).unwrap().permissions();
        p.set_mode(0o755);
        fs::set_permissions(&script_dst, p).unwrap();
    }
    fs::copy(
        project_root().join("docs/examples/eslint-check/extension.yml"),
        dst.join("extension.yml"),
    )
    .unwrap();
}

fn write_manifest(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        "version: 1\nchecks:\n  lint:\n    uses: eslint/check\n    with: {}\n",
    )
    .unwrap();
}

#[test]
#[serial]
fn shell_extension_drives_full_validate_evidence_loop() {
    if !has_jq() {
        eprintln!("skipping shell_extension_e2e: jq not in $PATH");
        return;
    }
    let dir = TempDir::new().unwrap();
    write_manifest(&dir.path().join("MUSTS.yml"));
    install_eslint_extension(dir.path());

    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("1. eslint-root"))
        .stdout(predicate::str::contains("npx eslint"));

    // Stage a log outside the workspace so the scope hash stays stable.
    let assets = TempDir::new().unwrap();
    let log = assets.path().join("eslint.log");
    fs::write(&log, b"0 problems (0 errors, 0 warnings)\n").unwrap();

    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg("eslint-root")
        .arg("--text")
        .arg("eslint exited 0")
        .arg("--asset")
        .arg(&log)
        .assert()
        .success();

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
fn shell_extension_rejects_missing_log() {
    if !has_jq() {
        return;
    }
    let dir = TempDir::new().unwrap();
    write_manifest(&dir.path().join("MUSTS.yml"));
    install_eslint_extension(dir.path());
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
        .arg("eslint-root")
        .arg("--text")
        .arg("ok")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("log"));
}
