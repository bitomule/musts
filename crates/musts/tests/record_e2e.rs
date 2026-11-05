//! End-to-end coverage for `musts stamp` + `musts record`: a `bazel test`
//! the agent already ran is recorded from its build event file, and only
//! when the tree it ran on is the tree the check covers now.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("musts").expect("musts binary not built")
}

fn musts(root: &Path) -> Command {
    let mut cmd = bin();
    cmd.arg("--workspace").arg(root);
    cmd
}

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn workspace() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(
        &root.join("MUSTS.yml"),
        "version: 1\nchecks:\n  tests:\n    uses: bazel/test\n    paths: [\"src/**\"]\n    with:\n      targets: [\"//src/...\", \"//tools:lint_test\"]\n  docs:\n    uses: agent\n    paths: [\"docs/**\"]\n    with:\n      facts: [\"The guide is accurate.\"]\n",
    );
    write(&root.join("src/lib.sh"), "echo hi\n");
    write(&root.join("docs/guide.md"), "# Guide\n");
    dir
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn stamp(root: &Path) -> String {
    let out = musts(root).current_dir(root).arg("stamp").output().unwrap();
    assert!(out.status.success());
    let line = String::from_utf8(out.stdout).unwrap();
    let value = line
        .trim()
        .strip_prefix("MUSTS_SCOPES ")
        .unwrap_or_else(|| panic!("unexpected stamp line: {line:?}"));
    value.to_string()
}

/// A build event file in the shapes Bazel 8.4.2 writes, for a run of
/// `bazel test //src/... //tools:lint_test` that passed.
fn bep(root: &Path, stamp: Option<&str>, start_ms: u128) -> String {
    let ws = fs::canonicalize(root).unwrap().display().to_string();
    let mut lines = vec![
        serde_json::json!({"id":{"started":{}},"started":{"uuid":"5ae1c0b7-4565-4a45-9497-8abc51bce47e","startTimeMillis":start_ms.to_string(),"command":"test","workspaceDirectory":ws}}),
        serde_json::json!({"id":{"optionsParsed":{}},"optionsParsed":{"cmdLine":["--keep_going"]}}),
        serde_json::json!({"id":{"pattern":{"pattern":["//src/...","//tools:lint_test"]}},"expanded":{}}),
    ];
    if let Some(stamp) = stamp {
        lines.push(serde_json::json!({"id":{"workspaceStatus":{}},"workspaceStatus":{"item":[{"key":"MUSTS_SCOPES","value":stamp}]}}));
    }
    for label in ["//src:lib_test", "//tools:lint_test"] {
        lines.push(serde_json::json!({"id":{"targetConfigured":{"label":label}},"configured":{"targetKind":"sh_test rule"}}));
        lines.push(serde_json::json!({"id":{"testSummary":{"label":label,"configuration":{"id":"c1"}}},"testSummary":{"overallStatus":"PASSED","totalRunCount":1}}));
    }
    lines.push(
        serde_json::json!({"id":{"buildFinished":{}},"finished":{"exitCode":{"name":"SUCCESS"}}}),
    );
    lines.push(serde_json::json!({"id":{"buildMetrics":{}},"lastMessage":true,"buildMetrics":{}}));
    let body: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let path = root.join(".musts/bep.json");
    write(&path, &(body.join("\n") + "\n"));
    path.display().to_string()
}

fn issue_tasks(root: &Path) {
    musts(root)
        .arg("validate")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("bazel-test-root"))
        .stdout(predicate::str::contains("musts record <task-id>"));
}

#[test]
#[serial]
fn records_a_stamped_run_and_the_check_turns_green() {
    let dir = workspace();
    let root = dir.path();
    issue_tasks(root);
    let stamp = stamp(root);
    bep(root, Some(&stamp), now_ms());

    musts(root)
        .args(["record", "bazel-test-root"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "✓ root/tests — 2 tests passed, tree unchanged since the invocation",
        ));

    musts(root)
        .arg("validate")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("bazel-test-root").not())
        .stdout(predicate::str::contains("agent-"));
    let lock = fs::read_to_string(root.join(".musts/ledger.lock.yaml")).unwrap();
    assert!(lock.contains("root/tests"), "{lock}");
}

#[test]
#[serial]
fn records_by_check_id_from_an_explicit_file() {
    let dir = workspace();
    let root = dir.path();
    issue_tasks(root);
    let stamp = stamp(root);
    let path = bep(root, Some(&stamp), now_ms());
    musts(root)
        .args(["record", "root/tests", "--from", &path])
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ root/tests"));
}

#[test]
#[serial]
fn rejects_a_run_on_a_tree_that_changed_since() {
    let dir = workspace();
    let root = dir.path();
    issue_tasks(root);
    let stamp = stamp(root);
    bep(root, Some(&stamp), now_ms());
    write(&root.join("src/lib.sh"), "echo changed\n");

    musts(root)
        .args(["record", "bazel-test-root"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "✗ root/tests — the tree changed since the invocation: root/tests was",
        ))
        .stdout(predicate::str::contains("The check stays pending"));
    musts(root)
        .arg("validate")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("bazel-test-root"));
}

#[test]
#[serial]
fn rejects_a_file_rewritten_after_the_run_started() {
    let dir = workspace();
    let root = dir.path();
    issue_tasks(root);
    let stamp = stamp(root);
    let started = now_ms() - 5_000;
    write(&root.join("src/lib.sh"), "echo hi\n");
    bep(root, Some(&stamp), started);

    musts(root)
        .args(["record", "bazel-test-root"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "✗ root/tests — src/lib.sh was modified at",
        ))
        .stdout(predicate::str::contains("after the invocation started at"));
}

#[test]
#[serial]
fn rejects_a_run_without_the_stamp() {
    let dir = workspace();
    let root = dir.path();
    issue_tasks(root);
    bep(root, None, now_ms());
    musts(root)
        .args(["record", "bazel-test-root"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "the invocation carries no musts stamp",
        ));
}

#[test]
#[serial]
fn rejects_a_missing_build_event_file() {
    let dir = workspace();
    let root = dir.path();
    issue_tasks(root);
    musts(root)
        .args(["record", "bazel-test-root"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "cannot read the build event file .musts/bep.json",
        ));
}

#[test]
#[serial]
fn refuses_a_task_that_is_not_bazel_test() {
    let dir = workspace();
    let root = dir.path();
    issue_tasks(root);
    musts(root)
        .args(["record", "root/docs"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("only reads bazel test runs"));
}

#[test]
#[serial]
fn stamp_never_fails_a_build() {
    let dir = TempDir::new().unwrap();
    bin()
        .current_dir(dir.path())
        .arg("stamp")
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("musts stamp: no stamp written"));
}
