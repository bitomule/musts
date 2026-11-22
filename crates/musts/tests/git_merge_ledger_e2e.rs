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

fn git_succeeds(repo: &Path, args: &[&str]) -> bool {
    StdCommand::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
        .status
        .success()
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
/// the squashed tree would carry *both* sets of edits — a combination
/// that was never validated on either side. Both sides revalidated the
/// same check, so they edit the same ledger file and git reports the
/// conflict instead of silently landing an unvalidated tree.
///
/// That is the correct answer, not a ledger bug: two individually-green
/// trees do not make their merge green.
#[test]
#[serial]
fn squash_merge_of_a_stale_branch_conflicts_on_the_revalidated_check() {
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
    assert!(
        !git_succeeds(dir, &["merge", "-q", "--squash", "feature"]),
        "both sides revalidated the same check: the merge must conflict"
    );
    let unmerged = StdCommand::new("git")
        .current_dir(dir)
        .args(["diff", "--name-only", "--diff-filter=U"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&unmerged.stdout).trim(),
        ".musts/ledger/root%2Fbuild"
    );
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

    // Bring `main` in: the ledger entry conflicts because both sides
    // revalidated the check. Take either side, then re-close the loop on
    // the integration tree.
    assert!(!git_succeeds(dir, &["merge", "-q", "--no-edit", "main"]));
    git(
        dir,
        &["checkout", "--ours", "--", ".musts/ledger/root%2Fbuild"],
    );
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--no-edit"]);
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

const SCOPES: usize = 16;

fn scope(i: usize) -> String {
    format!("s{i:02}")
}

fn record_evidence(dir: &Path, task: &str) {
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
        .arg(task)
        .arg("--text")
        .arg("Built it.")
        .assert()
        .success();
}

/// Seed commit with [`SCOPES`] independent scopes (same capability as the
/// root check, so each is carved out of the others), all validated.
///
/// Sixteen, not two: with a single-file ledger two branches only collide
/// when their new lines land next to each other in the sorted list, so a
/// handful of checks can slip through by luck. With sixteen, interleaved
/// edits collide almost surely, which keeps this suite honest against a
/// shared-file format.
fn init_scoped_repo(dir: &Path) {
    init_repo(dir);
    fs::write(dir.join("src/a.txt"), "a1\n").unwrap();
    for i in 0..SCOPES {
        let name = scope(i);
        fs::create_dir_all(dir.join(&name)).unwrap();
        fs::write(
            dir.join(&name).join("MUSTS.yml"),
            "version: 1\nchecks:\n  build:\n    uses: agent\n    with:\n      facts:\n        - \"The subproject builds.\"\n",
        )
        .unwrap();
        fs::write(dir.join(&name).join("f.txt"), "v1\n").unwrap();
    }
    record_evidence(dir, "agent-root");
    for i in 0..SCOPES {
        record_evidence(dir, &format!("agent-{}", scope(i)));
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "seed"]);
}

fn revalidate_scope(dir: &Path, i: usize, content: &str) {
    fs::write(dir.join(scope(i)).join("f.txt"), content).unwrap();
    record_evidence(dir, &format!("agent-{}", scope(i)));
}

fn assert_no_merge_rule(dir: &Path) {
    assert!(
        !dir.join(".musts/.gitattributes").exists(),
        "musts must not write a merge rule"
    );
    assert!(!dir.join(".gitattributes").exists());
}

/// What GitHub does: it ignores merge drivers, so only the content of
/// the files decides. Two branches that validate *different* checks
/// (interleaved, so a shared file would collide) touch different ledger
/// files and merge without a conflict, with no merge rule in the repo.
#[test]
#[serial]
fn branches_validating_different_checks_merge_without_a_rule_or_conflict() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    init_scoped_repo(dir);
    assert_no_merge_rule(dir);

    git(dir, &["checkout", "-q", "-b", "feature-a"]);
    for i in (0..SCOPES).step_by(2) {
        revalidate_scope(dir, i, "a-side\n");
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "even scopes"]);
    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["checkout", "-q", "-b", "feature-b"]);
    for i in (1..SCOPES).step_by(2) {
        revalidate_scope(dir, i, "b-side\n");
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "odd scopes"]);

    assert!(
        git_succeeds(
            dir,
            &["merge-tree", "--write-tree", "feature-a", "feature-b"]
        ),
        "different checks must merge cleanly"
    );

    git(dir, &["checkout", "-q", "feature-a"]);
    git(dir, &["merge", "-q", "--no-edit", "feature-b"]);
    wipe_local_state(dir);
    assert_clean(dir);
}

/// The other half of the contract: two branches that revalidate the
/// *same* check edit the same file, and the conflict is kept — the merged
/// tree was validated by neither.
#[test]
#[serial]
fn branches_revalidating_the_same_check_conflict() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    init_scoped_repo(dir);
    assert_no_merge_rule(dir);

    git(dir, &["checkout", "-q", "-b", "feature-a"]);
    revalidate_scope(dir, 0, "a-side\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "first"]);
    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["checkout", "-q", "-b", "feature-b"]);
    revalidate_scope(dir, 0, "b-side\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "second"]);

    assert!(
        !git_succeeds(
            dir,
            &["merge-tree", "--write-tree", "feature-a", "feature-b"]
        ),
        "the same check revalidated on both sides must conflict"
    );
}
