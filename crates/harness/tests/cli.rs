//! End-to-end tests for the `harness` CLI (Phase 1 surface).
//!
//! These exercise the binary as a black box via `assert_cmd`. Phase 1
//! only ships two outcomes: empty-workspace clean (exit 0) and
//! "manifests present but extensions unwired" placeholder (exit 2).

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("harness").expect("harness binary not built")
}

#[test]
fn empty_workspace_with_git_anchor_is_clean() {
    // Scenario 22 (empty_workspace_no_manifests) — sans `.git` anchor.
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Harness validation clean. No HARNESS.yml files found.",
        ));
}

#[test]
fn json_clean_shape_matches_contract() {
    let dir = TempDir::new().unwrap();
    let out = bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .arg("--json")
        .assert()
        .success();
    let stdout = std::str::from_utf8(&out.get_output().stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout).expect("valid JSON");
    assert_eq!(parsed["protocol_version"], 1);
    assert_eq!(parsed["status"], "clean");
    assert!(parsed["tasks"].as_array().unwrap().is_empty());
    assert!(parsed["ignored_checks"].as_array().unwrap().is_empty());
    // notes is always present as an array; empty when clean (no synthetic
    // entries — the shape mirrors a real clean resolve result).
    assert!(parsed["notes"].as_array().unwrap().is_empty());
}

#[test]
fn missing_extension_capability_reports_clearly() {
    // Scenario 18 (missing_extension_capability): a manifest declares a
    // capability with no installed extension. PLAN.md §9 Phase 1 says
    // "no extension implements capability X" with the manifest path
    // and offending check id.
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("HARNESS.yml"),
        "version: 1\nchecks:\n  app-build:\n    uses: bazel/build\n    with:\n      target: //App:App\n",
    )
    .unwrap();

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
        .stderr(predicate::str::contains("root/app-build"))
        .stderr(predicate::str::contains("HARNESS.yml"));
}

#[test]
fn missing_workspace_reports_workspace_not_found() {
    // Run from a tmpdir that contains neither `.git` nor any HARNESS.yml.
    // Pass through `--workspace` so we don't accidentally resolve to the
    // calling test's git root.
    let dir = TempDir::new().unwrap();
    let alt = TempDir::new().unwrap();
    // Use the CWD of a totally separate empty dir without manifests/.git
    // and DON'T pass --workspace so the resolution rules kick in.
    bin()
        .current_dir(alt.path())
        .env("HOME", dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "no .git directory or HARNESS.yml found",
        ));
}

#[test]
fn broken_workspace_path_canonicalisation() {
    bin()
        .arg("--workspace")
        .arg("/this/path/definitely/does/not/exist")
        .arg("validate")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "could not canonicalise workspace path",
        ));
}
