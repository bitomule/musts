//! E2E for the workspace-health warnings.
//!
//! Field report: a repo gitignored `.musts/ledger.lock.yaml`, so every
//! `git worktree` started with no ledger and every check was dirty there
//! while `main` was fully green. It read as an unconditional pre-commit
//! block, `.mustsignore` did not help (the check is dirty from missing
//! state, not from the diff), and nothing in any output said so.

use std::path::Path;
use std::process::Command;

mod common;

fn musts_bin() -> std::path::PathBuf {
    common::workspace_binary("musts", "musts")
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(out.status.success(), "git {args:?} failed");
}

/// A git repo that is also a musts workspace, with one agent check.
fn repo(gitignore: &str) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    std::fs::write(dir.path().join(".gitignore"), gitignore).unwrap();
    std::fs::write(
        dir.path().join("MUSTS.yml"),
        "version: 1\nchecks:\n  c:\n    uses: agent\n    with:\n      facts: [\"it holds\"]\n",
    )
    .unwrap();
    dir
}

fn validate(root: &Path) -> String {
    let out = Command::new(musts_bin())
        .arg("--workspace")
        .arg(root)
        .arg("validate")
        .output()
        .expect("run musts validate");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn record_evidence(root: &Path) {
    let out = Command::new(musts_bin())
        .arg("--workspace")
        .arg(root)
        .args(["evidence", "agent-root", "--text", "it holds"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The exact `.gitignore` shape found in the wild: the repo carefully
/// keeps `.musts/extensions/` and ignores the sqlite cache, then ignores
/// the ledger alongside them as if it were another cache.
#[test]
fn a_gitignored_ledger_is_called_out() {
    let dir = repo(".musts/.lock\n.musts/*.sqlite\n.musts/evidence/\n.musts/ledger.lock.yaml\n");
    let stdout = validate(dir.path());
    assert!(stdout.contains("gitignored"), "{stdout}");
    assert!(stdout.contains("worktrees"), "{stdout}");
    assert!(stdout.contains(".gitignore"), "must name the fix: {stdout}");
}

#[test]
fn a_tracked_ledger_produces_no_health_warning() {
    let dir = repo(".musts/state.sqlite*\n.musts/.lock\n.musts/evidence/\n");
    validate(dir.path());
    record_evidence(dir.path());
    let stdout = validate(dir.path());
    assert!(stdout.contains("Musts validation clean."), "{stdout}");
    assert!(!stdout.contains("gitignored"), "{stdout}");
    assert!(!stdout.contains("no validation state"), "{stdout}");
}

/// "Pending because the tree changed" and "pending because this
/// workspace has no state" need opposite responses: do the work, versus
/// go and get the ledger. Running the suite in the second case proves
/// nothing that was not already proven on main.
#[test]
fn a_workspace_with_no_ledger_says_so_instead_of_just_listing_tasks() {
    let dir = repo(".musts/state.sqlite*\n.musts/.lock\n");
    let stdout = validate(dir.path());
    assert!(stdout.contains("no validation state"), "{stdout}");
    assert!(
        stdout.contains("not because"),
        "must distinguish it from a changed tree: {stdout}"
    );
    assert!(
        stdout.contains("agent-root"),
        "still lists the task: {stdout}"
    );
}

/// Once state exists, the "no state" hint must stop — otherwise it fires
/// on every ordinary pending run and stops being read.
#[test]
fn the_no_state_hint_disappears_once_anything_is_recorded() {
    let dir = repo(".musts/state.sqlite*\n.musts/.lock\n");
    validate(dir.path());
    record_evidence(dir.path());

    std::fs::write(dir.path().join("MUSTS.yml"),
        "version: 1\nchecks:\n  c:\n    uses: agent\n    with:\n      facts: [\"it still holds\"]\n",
    ).unwrap();
    let stdout = validate(dir.path());
    assert!(stdout.contains("agent-root"), "task is pending: {stdout}");
    assert!(
        !stdout.contains("no validation state"),
        "the ledger is populated; this is a genuine tree change: {stdout}"
    );
}

/// A gitignored ledger is the *cause* of the empty one. Reporting both
/// would bury the actionable finding under its own symptom.
#[test]
fn gitignored_and_empty_reports_only_the_cause() {
    let dir = repo(".musts/ledger.lock.yaml\n.musts/state.sqlite*\n");
    let stdout = validate(dir.path());
    assert!(stdout.contains("gitignored"), "{stdout}");
    assert!(!stdout.contains("no validation state"), "{stdout}");
}

/// Health hints are diagnostics, never a reason to change the verdict.
#[test]
fn health_warnings_do_not_change_the_exit_code() {
    let dir = repo(".musts/ledger.lock.yaml\n");
    validate(dir.path());
    record_evidence(dir.path());
    let out = Command::new(musts_bin())
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "a warning must not make a clean workspace fail"
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("gitignored"));
}

/// Not every workspace is a git repo, and `git` is not guaranteed to be
/// on PATH. Neither may break validate.
#[test]
fn a_non_git_workspace_is_fine() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("MUSTS.yml"),
        "version: 1\nchecks:\n  c:\n    uses: agent\n    with:\n      facts: [\"f\"]\n",
    )
    .unwrap();
    let out = Command::new(musts_bin())
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("gitignored"));
}
