//! E2E for `musts stats`.
//!
//! The command exists because a check that reopens forever and never goes
//! red is the single most expensive thing a manifest can contain, and
//! before this there was no way to see one short of grepping the ledger.
//! These tests pin the numbers that claim actually rests on.

use std::path::Path;
use std::process::Command;

mod common;

fn musts_bin() -> std::path::PathBuf {
    common::workspace_binary("musts", "musts")
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

fn stats(root: &Path, extra: &[&str]) -> (String, String, i32) {
    let out = Command::new(musts_bin())
        .arg("stats")
        .args(extra)
        .arg("--workspace")
        .arg(root)
        .current_dir(root)
        .output()
        .expect("run musts stats");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// A ledger with `count` distinct scope hashes for one check — i.e. a
/// check that has been reopened and re-proven `count - 1` times.
fn ledger_with_repeats(check: &str, count: usize) -> String {
    let mut body = String::from("version: 1\nsatisfied:\n");
    for i in 0..count {
        body.push_str(&format!(
            "- {{check: \"{check}\", scope_hash: \"{:064x}\"}}\n",
            i
        ));
    }
    body
}

#[test]
fn reports_reopen_counts_from_the_committed_ledger() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    write(
        &root.join("tools/MUSTS.yml"),
        "version: 1\nchecks:\n  version-policy:\n    uses: agent\n    with:\n      facts: [\"a fact\"]\n",
    );
    write(
        &root.join(".musts/ledger.lock.yaml"),
        &ledger_with_repeats("tools/version-policy", 70),
    );

    let (stdout, stderr, code) = stats(root, &[]);
    assert_eq!(
        code, 0,
        "stats must not fail on a healthy workspace: {stderr}"
    );
    assert!(stdout.contains("tools/version-policy"), "{stdout}");
    // 70 distinct satisfied scopes is 69 reopens: the first proof is not
    // a reopen. This is the exact shape found in the Nokoru audit.
    assert!(
        stdout.contains("Reopened repeatedly, never red"),
        "a 70-satisfaction check with no reds must be called out:\n{stdout}"
    );
    assert!(stdout.contains("reopened 69 times, 0 red"), "{stdout}");
}

#[test]
fn json_output_carries_the_per_check_numbers() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    write(
        &root.join("MUSTS.yml"),
        "version: 1\nchecks:\n  build:\n    uses: cargo/fmt\n",
    );
    write(
        &root.join(".musts/ledger.lock.yaml"),
        &ledger_with_repeats("root/build", 4),
    );

    let (stdout, stderr, code) = stats(root, &["--json"]);
    assert_eq!(code, 0, "{stderr}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let check = &v["checks"][0];
    assert_eq!(check["check_id"], "root/build");
    assert_eq!(check["satisfied_scopes"], 4);
    assert_eq!(check["reopened"], 3);
    assert_eq!(check["capability"], "cargo/fmt");
    assert_eq!(check["declared"], true);
    assert_eq!(v["total_satisfied_entries"], 4);
}

#[test]
fn checks_are_ordered_most_expensive_first() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let mut ledger = String::from("version: 1\nsatisfied:\n");
    for i in 0..5 {
        ledger.push_str(&format!(
            "- {{check: \"expensive\", scope_hash: \"{:064x}\"}}\n",
            i
        ));
    }
    ledger.push_str("- {check: \"cheap\", scope_hash: \"ff\"}\n");
    write(&root.join(".musts/ledger.lock.yaml"), &ledger);

    let (stdout, _, _) = stats(root, &["--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["checks"][0]["check_id"], "expensive");
    assert_eq!(v["checks"][1]["check_id"], "cheap");
}

#[test]
fn a_declared_check_with_no_history_still_appears() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    write(
        &root.join("MUSTS.yml"),
        "version: 1\nchecks:\n  never-run:\n    uses: agent\n    with:\n      facts: [\"x\"]\n",
    );

    let (stdout, _, code) = stats(root, &["--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["checks"][0]["check_id"], "root/never-run");
    assert_eq!(v["checks"][0]["satisfied_scopes"], 0);
}

#[test]
fn ledger_entries_for_deleted_checks_are_marked() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    write(&root.join("MUSTS.yml"), "version: 1\nchecks: {}\n");
    write(
        &root.join(".musts/ledger.lock.yaml"),
        &ledger_with_repeats("removed-long-ago", 2),
    );

    let (stdout, _, _) = stats(root, &[]);
    assert!(stdout.contains("no longer declared"), "{stdout}");
}

#[test]
fn empty_workspace_exits_zero() {
    let dir = tempfile::TempDir::new().unwrap();
    let (stdout, stderr, code) = stats(dir.path(), &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("No checks"), "{stdout}");
}

/// `stats` must never take the workspace lock: reading history should not
/// be blockable by a long-running `validate` in another terminal.
#[test]
fn runs_while_the_workspace_lock_is_held() {
    use fs2::FileExt;

    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    write(&root.join("MUSTS.yml"), "version: 1\nchecks: {}\n");
    std::fs::create_dir_all(root.join(".musts")).unwrap();

    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".musts/.lock"))
        .unwrap();
    lock.try_lock_exclusive().expect("hold the lock");

    let (_, stderr, code) = stats(root, &[]);
    assert_eq!(code, 0, "stats blocked on the lock: {stderr}");
}
