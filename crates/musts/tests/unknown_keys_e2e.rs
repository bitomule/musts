//! E2E for unknown-manifest-key warnings.
//!
//! The motivating case is real: a repo wrote `excludes:` where the key is
//! `exclude_paths:`, so its exclusion of UI views did nothing and every UI
//! change fired the unit-test check as well as the snapshot check —
//! silently, for as long as the file had existed.

use std::path::Path;
use std::process::Command;

mod common;

fn musts_bin() -> std::path::PathBuf {
    common::workspace_binary("musts", "musts")
}

fn workspace(manifest: &str) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("MUSTS.yml"), manifest).unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.swift"), "// x\n").unwrap();
    dir
}

fn validate(root: &Path, extra: &[&str]) -> (String, String) {
    let out = Command::new(musts_bin())
        .arg("--workspace")
        .arg(root)
        .arg("validate")
        .args(extra)
        .current_dir(root)
        .output()
        .expect("run musts validate");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The exact typo found in the wild, with the suggestion that matters.
/// Naive edit distance puts `excludes` closer to `paths` than to
/// `exclude_paths`, which would send the author to the one key that
/// changes the check's meaning — so this pins the right answer.
#[test]
fn excludes_typo_is_reported_and_points_at_exclude_paths() {
    let dir = workspace(
        "version: 1\nchecks:\n  unit-tests:\n    uses: agent\n    paths: [\"src/**/*.swift\"]\n    excludes: [\"src/UI/**\"]\n    with:\n      facts: [\"tests pass\"]\n",
    );
    let (stdout, _) = validate(dir.path(), &[]);
    assert!(stdout.contains("unknown key `excludes`"), "{stdout}");
    assert!(
        stdout.contains("did you mean `exclude_paths`"),
        "must suggest exclude_paths, not paths:\n{stdout}"
    );
    assert!(stdout.contains("being ignored"), "{stdout}");
}

/// A warning must never turn an otherwise-valid manifest red. Seven
/// working repos would go red overnight on upgrade.
#[test]
fn an_unknown_key_does_not_fail_the_run() {
    let dir = workspace(
        "version: 1\nchecks:\n  c:\n    uses: agent\n    excludes: [\"x\"]\n    with:\n      facts: [\"f\"]\n",
    );
    let out = Command::new(musts_bin())
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .output()
        .unwrap();
    // Exit 1 means "tasks pending", which is expected for a fresh agent
    // check. Exit 2 would mean the manifest was rejected.
    assert_eq!(
        out.status.code(),
        Some(1),
        "unknown keys must warn, not fail:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn unknown_top_level_key_is_reported() {
    let dir = workspace("version: 1\nsettings: {}\nchecks: {}\n");
    let (stdout, _) = validate(dir.path(), &[]);
    assert!(
        stdout.contains("unknown top-level key `settings`"),
        "{stdout}"
    );
}

#[test]
fn a_key_with_no_close_match_is_reported_without_a_guess() {
    let dir = workspace(
        "version: 1\nchecks:\n  c:\n    uses: agent\n    zzzzzzzz: 1\n    with:\n      facts: [\"f\"]\n",
    );
    let (stdout, _) = validate(dir.path(), &[]);
    assert!(stdout.contains("unknown key `zzzzzzzz`"), "{stdout}");
    assert!(
        !stdout.contains("did you mean"),
        "no suggestion is better than a misleading one:\n{stdout}"
    );
}

#[test]
fn a_clean_manifest_emits_no_warning_line() {
    let dir = workspace(
        "version: 1\nchecks:\n  c:\n    uses: agent\n    paths: [\"src/**\"]\n    exclude_paths: [\"src/gen/**\"]\n    with:\n      facts: [\"f\"]\n",
    );
    let (stdout, _) = validate(dir.path(), &[]);
    assert!(!stdout.contains("unknown key"), "{stdout}");
    // Deliberately not asserting "no `!` lines at all": a fresh temp
    // workspace legitimately carries the "no validation state here"
    // health warning, which is a different subsystem.
}

#[test]
fn warnings_appear_in_json_output() {
    let dir = workspace(
        "version: 1\nchecks:\n  c:\n    uses: agent\n    excludes: [\"x\"]\n    with:\n      facts: [\"f\"]\n",
    );
    let (stdout, _) = validate(dir.path(), &["--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    // Select by manifest rather than by index: workspace-health warnings
    // share this array and are emitted first.
    let w = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["manifest"] == "MUSTS.yml")
        .unwrap_or_else(|| panic!("no manifest warning in {stdout}"));
    assert!(
        w["message"].as_str().unwrap().contains("excludes"),
        "{stdout}"
    );
}

/// A `uses:` naming a capability nothing provides already fails the run.
/// What it did not do was say what to write instead.
#[test]
fn missing_capability_error_lists_what_is_available() {
    let dir = workspace("version: 1\nchecks:\n  c:\n    uses: bazel/tests\n    with: {}\n");
    let out = Command::new(musts_bin())
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("bazel/tests"), "{stderr}");
    assert!(stderr.contains("root/c"), "names the check: {stderr}");
    assert!(stderr.contains("MUSTS.yml"), "names the manifest: {stderr}");
    assert!(
        stderr.contains(".musts/extensions"),
        "says where it searched: {stderr}"
    );
    assert!(
        stderr.contains("bazel/build") && stderr.contains("cargo/test"),
        "lists what is available: {stderr}"
    );
}
