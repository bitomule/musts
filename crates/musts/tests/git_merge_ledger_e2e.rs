//! Does the committed ledger survive a squash-merge?
//!
//! Field report: a branch validated green, got squash-merged, and `main`
//! immediately asked for the same (expensive) checks again — blamed on
//! "squash-merge invalidates the ledger". These tests drive real `git`
//! against a real workspace to separate the two candidate causes:
//!
//! 1. squash-merge itself losing the lock / shifting the scope hash, or
//! 2. the branch having been *behind* the target, so the tree that lands
//!    is a combination neither side ever validated.
//!
//! Only (2) reproduces. (1) is clean, which is what `squash_merge_of_an
//! _up_to_date_branch_keeps_the_ledger_green` pins down.

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("musts").expect("musts binary not built")
}

fn git(repo: &Path, args: &[&str]) {
    let out = StdCommand::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A git repo whose root is a musts workspace. `state.sqlite` and the
/// evidence dir are gitignored exactly like a real project, so only
/// `ledger.lock.yaml` travels between branches.
fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    fs::write(
        dir.join(".gitignore"),
        ".musts/state.sqlite*\n.musts/evidence/\n.musts/.lock\n",
    )
    .unwrap();
    fs::write(
        dir.join("MUSTS.yml"),
        r#"version: 1
checks:
  build:
    uses: agent
    paths:
      - "src/**/*.txt"
    with:
      facts:
        - "The tree builds."
"#,
    )
    .unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
}

/// Close the loop for the single `agent` check and commit the refreshed
/// lock, i.e. what a contributor does before opening a PR.
fn validate_and_record(dir: &Path, message: &str) {
    // Issue the task first; `evidence` only accepts ids the most recent
    // validate emitted.
    bin()
        .arg("--workspace")
        .arg(dir)
        .arg("validate")
        .assert()
        .failure()
        .code(1);
    bin()
        .arg("--workspace")
        .arg(dir)
        .arg("evidence")
        .arg("agent-root")
        .arg("--text")
        .arg("Built it.")
        .assert()
        .success();
    bin()
        .arg("--workspace")
        .arg(dir)
        .arg("validate")
        .assert()
        .success();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

fn assert_clean(dir: &Path) {
    bin()
        .arg("--workspace")
        .arg(dir)
        .arg("validate")
        .assert()
        .success()
        .stdout(predicate::str::contains("Musts validation clean."));
}

/// Drop the machine-local cache so only the committed lock can answer
/// the satisfaction question — the state a CI runner or a teammate's
/// clone is in.
fn wipe_local_state(dir: &Path) {
    for name in ["state.sqlite", "state.sqlite-shm", "state.sqlite-wal"] {
        let p = dir.join(".musts").join(name);
        if p.exists() {
            fs::remove_file(p).unwrap();
        }
    }
}

/// Baseline: the branch is up to date with `main`, so the squashed tree
/// is byte-identical to the tree the branch validated. The lock must
/// carry over and `main` must be clean without re-running anything.
#[test]
#[serial]
fn squash_merge_of_an_up_to_date_branch_keeps_the_ledger_green() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    init_repo(dir);

    fs::write(dir.join("src/a.txt"), "a1\n").unwrap();
    validate_and_record(dir, "seed");

    git(dir, &["checkout", "-q", "-b", "feature"]);
    fs::write(dir.join("src/a.txt"), "a2\n").unwrap();
    validate_and_record(dir, "feature work");

    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["merge", "-q", "--squash", "feature"]);
    git(dir, &["commit", "-q", "-m", "feat: squashed feature"]);

    wipe_local_state(dir);
    assert_clean(dir);
}

/// The real reproduction. `main` moved on while the branch was open, so
/// the squashed tree carries *both* sets of edits — a combination that
/// was never validated on either side. musts asks for the check again.
///
/// That is the correct answer, not a ledger bug: two individually-green
/// trees do not make their merge green.
#[test]
#[serial]
fn squash_merge_of_a_stale_branch_legitimately_reopens_the_check() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    init_repo(dir);

    fs::write(dir.join("src/a.txt"), "a1\n").unwrap();
    validate_and_record(dir, "seed");

    // Branch off, then let `main` land another validated change.
    git(dir, &["checkout", "-q", "-b", "feature"]);
    git(dir, &["checkout", "-q", "main"]);
    fs::write(dir.join("src/b.txt"), "b1\n").unwrap();
    validate_and_record(dir, "other PR");

    // The branch validates its own tree, which still has no b.txt.
    git(dir, &["checkout", "-q", "feature"]);
    fs::write(dir.join("src/a.txt"), "a2\n").unwrap();
    validate_and_record(dir, "feature work");

    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["merge", "-q", "--squash", "feature"]);
    git(dir, &["commit", "-q", "-m", "feat: squashed stale feature"]);

    // Both a2 and b1 are present. Neither branch ever saw this tree.
    assert_eq!(fs::read_to_string(dir.join("src/a.txt")).unwrap(), "a2\n");
    assert_eq!(fs::read_to_string(dir.join("src/b.txt")).unwrap(), "b1\n");

    wipe_local_state(dir);
    bin()
        .arg("--workspace")
        .arg(dir)
        .arg("validate")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("agent-root"));
}

/// Two branches record evidence for two *different* scopes and both get
/// merged. Each scope ends up in exactly the state its branch validated,
/// so `main` must be clean — the whole point of committing the ledger.
///
/// The scope carve-out is what makes this work: `sub/` declares the same
/// capability, so it is subtracted from the root check's scope and the
/// two branches never touch each other's hash. Scoping checks narrowly
/// is the lever that decides how much of a merge stays green.
#[test]
#[serial]
fn merging_two_branches_that_each_recorded_evidence_keeps_both_green() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    init_repo(dir);
    // A second scope. Same capability as the root check, so the root
    // scope carves `sub/` out and the two are genuinely independent.
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(
        dir.join("sub/MUSTS.yml"),
        r#"version: 1
checks:
  build:
    uses: agent
    with:
      facts:
        - "The subproject builds."
"#,
    )
    .unwrap();
    fs::write(dir.join("src/a.txt"), "a1\n").unwrap();
    fs::write(dir.join("sub/b.txt"), "b1\n").unwrap();

    for task in ["agent-root", "agent-sub"] {
        bin()
            .arg("--workspace")
            .arg(dir)
            .arg("validate")
            .assert()
            .failure();
        bin()
            .arg("--workspace")
            .arg(dir)
            .arg("evidence")
            .arg(task)
            .arg("--text")
            .arg("Built it.")
            .assert()
            .success();
    }
    assert_clean(dir);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "seed"]);
    git(dir, &["branch", "-q", "feature-b"]);

    // Branch A moves the root scope only.
    git(dir, &["checkout", "-q", "-b", "feature-a"]);
    fs::write(dir.join("src/a.txt"), "a2\n").unwrap();
    validate_and_record(dir, "root work");

    // Branch B moves the sub scope only.
    git(dir, &["checkout", "-q", "feature-b"]);
    fs::write(dir.join("sub/b.txt"), "b2\n").unwrap();
    bin()
        .arg("--workspace")
        .arg(dir)
        .arg("validate")
        .assert()
        .failure()
        .code(1);
    bin()
        .arg("--workspace")
        .arg(dir)
        .arg("evidence")
        .arg("agent-sub")
        .arg("--text")
        .arg("Built it.")
        .assert()
        .success();
    assert_clean(dir);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "sub work"]);

    // Land both. Neither touched the other's scope, so every scope is in
    // a state some branch validated.
    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["merge", "-q", "--squash", "feature-a"]);
    git(dir, &["commit", "-q", "-m", "feat: root work"]);
    git(dir, &["merge", "-q", "--squash", "feature-b"]);
    git(dir, &["commit", "-q", "-m", "feat: sub work"]);

    wipe_local_state(dir);
    assert_clean(dir);
}

/// Same stale branch, but updated from `main` before merging. The branch
/// then validates the exact tree that lands, so the ledger survives.
/// This is the workflow fix for the case above.
#[test]
#[serial]
fn updating_the_branch_before_merging_restores_ledger_carry_over() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    init_repo(dir);

    fs::write(dir.join("src/a.txt"), "a1\n").unwrap();
    validate_and_record(dir, "seed");

    git(dir, &["checkout", "-q", "-b", "feature"]);
    git(dir, &["checkout", "-q", "main"]);
    fs::write(dir.join("src/b.txt"), "b1\n").unwrap();
    validate_and_record(dir, "other PR");

    git(dir, &["checkout", "-q", "feature"]);
    fs::write(dir.join("src/a.txt"), "a2\n").unwrap();
    validate_and_record(dir, "feature work");

    // Bring `main` in and re-close the loop on the integration tree.
    git(dir, &["merge", "-q", "--no-edit", "main"]);
    validate_and_record(dir, "revalidate after update");

    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["merge", "-q", "--squash", "feature"]);
    git(
        dir,
        &["commit", "-q", "-m", "feat: squashed updated feature"],
    );

    wipe_local_state(dir);
    assert_clean(dir);
}
