//! `plugin/scripts/precommit-validate.sh` must guard the repository the
//! commit belongs to — not whatever repository the agent session happens
//! to be sitting in.
//!
//! Field report: a session with its cwd in a musts repo could not commit
//! in a *different*, non-musts repo. The hook walked up from the session
//! cwd, found the first repo's `MUSTS.yml`, validated that, and blocked
//! the unrelated commit on the first repo's pending tasks.
//!
//! Exit codes: 0 allows the tool call, 2 blocks it.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serial_test::serial;
use tempfile::TempDir;

fn hook_script() -> PathBuf {
    // CARGO_MANIFEST_DIR is `crates/musts`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugin/scripts/precommit-validate.sh")
        .canonicalize()
        .expect("hook script")
}

/// Run the hook with a PreToolUse event body, with the freshly built
/// `musts` first on PATH so the hook resolves the binary under test.
fn run_hook(cwd: &Path, command: &str) -> (i32, String) {
    let musts = common::workspace_binary("musts", "musts");
    let bin_dir = musts.parent().unwrap().to_path_buf();
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let payload = serde_json::json!({
        "cwd": cwd.display().to_string(),
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();

    let mut child = Command::new("bash")
        .arg(hook_script())
        .env("PATH", path)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hook");
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("hook output");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(out.status.success(), "git {args:?} failed");
}

/// A git repo with a musts manifest whose loop is permanently dirty: an
/// `agent` check nobody has recorded evidence for.
fn dirty_musts_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    fs::write(
        dir.join("MUSTS.yml"),
        r#"version: 1
checks:
  contract:
    uses: agent
    with:
      facts:
        - "Someone checked this."
"#,
    )
    .unwrap();
}

/// A plain git repo with no musts manifest anywhere inside it.
fn plain_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    fs::write(dir.join("main.go"), "package main\n").unwrap();
}

/// The reported bug. Session cwd is the musts repo; the commit targets a
/// different repo that has no `MUSTS.yml`. The hook must not reach back
/// into the session's repo — the unrelated commit has to go through.
#[test]
#[serial]
fn commit_in_another_repo_is_not_blocked_by_the_session_repo() {
    let musts_repo = TempDir::new().unwrap();
    dirty_musts_repo(musts_repo.path());
    let other = TempDir::new().unwrap();
    plain_repo(other.path());

    // Sanity: the session repo really is dirty, so a commit *there* blocks.
    let (code, _) = run_hook(musts_repo.path(), "git commit -m 'wip'");
    assert_eq!(code, 2, "control: commit in the musts repo should block");

    let (code, stderr) = run_hook(
        musts_repo.path(),
        &format!("git -C {} commit -m 'wip'", other.path().display()),
    );
    assert_eq!(
        code, 0,
        "commit in a non-musts repo must be allowed; hook said:\n{stderr}"
    );
}

/// Same, expressed the way an agent usually writes it: `cd` into the
/// other repo first.
#[test]
#[serial]
fn cd_into_another_repo_before_committing_is_not_blocked() {
    let musts_repo = TempDir::new().unwrap();
    dirty_musts_repo(musts_repo.path());
    let other = TempDir::new().unwrap();
    plain_repo(other.path());

    let (code, stderr) = run_hook(
        musts_repo.path(),
        &format!("cd {} && git commit -m 'wip'", other.path().display()),
    );
    assert_eq!(
        code, 0,
        "commit after cd into a non-musts repo must be allowed; hook said:\n{stderr}"
    );
}

/// The mirror image: the session sits in a repo with no manifest, and the
/// commit targets the musts repo. The hook must follow the commit and
/// block it.
#[test]
#[serial]
fn commit_targeting_the_musts_repo_is_blocked_from_elsewhere() {
    let musts_repo = TempDir::new().unwrap();
    dirty_musts_repo(musts_repo.path());
    let other = TempDir::new().unwrap();
    plain_repo(other.path());

    let (code, stderr) = run_hook(
        other.path(),
        &format!("git -C {} commit -m 'wip'", musts_repo.path().display()),
    );
    assert_eq!(
        code, 2,
        "commit into the musts repo must be validated wherever it is run from"
    );
    assert!(
        stderr.contains("musts validate reports pending work"),
        "expected the blocking message, got:\n{stderr}"
    );
}

/// A manifest in a subdirectory still guards commits made from that
/// subdirectory — the walk starts at the command's directory, it just
/// stops at the repository root.
#[test]
#[serial]
fn manifest_in_a_subdirectory_still_guards_commits_from_there() {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    let sub = repo.path().join("service");
    fs::create_dir_all(&sub).unwrap();
    dirty_musts_repo_manifest_only(&sub);

    let (code, _) = run_hook(
        repo.path(),
        &format!("git -C {} commit -m 'wip'", sub.display()),
    );
    assert_eq!(
        code, 2,
        "the subdirectory manifest should guard this commit"
    );
}

/// A musts repo nested *above* the commit's repository must not leak in.
/// Without a repository boundary the upward walk would find it.
#[test]
#[serial]
fn parent_directory_manifest_outside_the_repo_is_ignored() {
    let outer = TempDir::new().unwrap();
    dirty_musts_repo_manifest_only(outer.path());
    let inner = outer.path().join("vendor/tool");
    fs::create_dir_all(&inner).unwrap();
    plain_repo(&inner);

    let (code, stderr) = run_hook(
        outer.path(),
        &format!("git -C {} commit -m 'wip'", inner.display()),
    );
    assert_eq!(
        code, 0,
        "a manifest above the commit's repo root must not apply; hook said:\n{stderr}"
    );
}

fn dirty_musts_repo_manifest_only(dir: &Path) {
    fs::write(
        dir.join("MUSTS.yml"),
        r#"version: 1
checks:
  contract:
    uses: agent
    with:
      facts:
        - "Someone checked this."
"#,
    )
    .unwrap();
}

/// Regression guard for the existing command parser: a `git log` whose
/// pattern mentions "git commit" is not a commit.
#[test]
#[serial]
fn non_commit_git_commands_are_ignored() {
    let musts_repo = TempDir::new().unwrap();
    dirty_musts_repo(musts_repo.path());
    let (code, _) = run_hook(musts_repo.path(), "git log --grep=\"git commit\"");
    assert_eq!(code, 0, "`git log` must never be treated as a commit");
}
