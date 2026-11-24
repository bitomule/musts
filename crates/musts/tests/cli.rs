//! End-to-end tests for the `musts` CLI (Phase 1 surface).
//!
//! These exercise the binary as a black box via `assert_cmd`. Phase 1
//! only ships two outcomes: empty-workspace clean (exit 0) and
//! "manifests present but extensions unwired" placeholder (exit 2).

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("musts").expect("musts binary not built")
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
            "Musts validation clean. No MUSTS.yml files found.",
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
    // capability with no installed extension *and* no matching built-in.
    // PLAN.md §9 Phase 1 says "no extension implements capability X"
    // with the manifest path and offending check id.
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("MUSTS.yml"),
        "version: 1\nchecks:\n  custom-check:\n    uses: third-party/lint\n    with: {}\n",
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
            "no extension implements capability `third-party/lint`",
        ))
        .stderr(predicate::str::contains("root/custom-check"))
        .stderr(predicate::str::contains("MUSTS.yml"));
}

#[test]
fn missing_workspace_reports_workspace_not_found() {
    // Run from a tmpdir that contains neither `.git` nor any MUSTS.yml.
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
            "no .git directory or MUSTS.yml found",
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

fn listed_commands(help: &str) -> Vec<String> {
    help.lines()
        .skip_while(|l| !l.starts_with("Commands:"))
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .filter(|name| name != "help")
        .collect()
}

fn help_of(args: &[&str]) -> String {
    let out = bin().args(args).arg("--help").assert().success();
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

#[test]
fn main_help_shows_only_validate_run_evidence_and_admin() {
    assert_eq!(
        listed_commands(&help_of(&[])),
        ["validate", "run", "evidence", "admin"]
    );
}

#[test]
fn admin_help_lists_the_maintenance_commands() {
    assert_eq!(
        listed_commands(&help_of(&["admin"])),
        ["lint", "stats", "calibrate", "skill"]
    );
}

#[test]
fn commands_moved_under_admin_are_gone_from_the_root() {
    for old in ["record", "lint", "stats", "calibrate", "skill"] {
        bin().arg(old).assert().failure().code(2);
    }
}

#[test]
fn hidden_stamp_keeps_old_bazelrc_files_building() {
    bin()
        .arg("stamp")
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[cfg(unix)]
#[test]
fn admin_skill_install_delegates_to_npx() {
    use std::os::unix::fs::PermissionsExt;

    let stub_dir = TempDir::new().unwrap();
    let stub = stub_dir.path().join("npx");
    std::fs::write(&stub, "#!/bin/sh\necho \"npx $@\"\n").unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        stub_dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    bin()
        .env("PATH", path)
        .args(["admin", "skill", "install", "--agent", "claude-code"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "npx --yes skills add bitomule/musts --skill musts --global --yes --agent claude-code",
        ));
}
