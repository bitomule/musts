//! Phase 5 end-to-end scenarios per `docs/PLAN.md` §7.3 and §9 Phase 5:
//! scenario 6 (`bazel_picks_deepest_target`) and the build half of §15.
//!
//! `bazel/build` is a built-in capability — no extension descriptor or
//! sidecar binary required.

mod common;

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("musts").expect("musts binary not built")
}

fn write_manifest(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

#[test]
#[serial]
fn scenario_6_bazel_picks_deepest_target() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("MUSTS.yml"),
        r#"version: 1
checks:
  app-build:
    uses: bazel/build
    with:
      target: //App:App
"#,
    );
    write_manifest(
        &dir.path().join("App/Login/MUSTS.yml"),
        r#"version: 1
checks:
  login-build:
    uses: bazel/build
    with:
      target: //App/Login:Login
"#,
    );

    let out = bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .arg("--json")
        .assert()
        .failure()
        .code(1);
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    // Exactly one task: the deepest target. The task subsumes both
    // check_ids so a single evidence submission converges the loop.
    assert_eq!(v["tasks"].as_array().unwrap().len(), 1);
    let task = &v["tasks"][0];
    assert!(task["title"]
        .as_str()
        .unwrap()
        .contains("//App/Login:Login"));
    let satisfies: Vec<String> = task["satisfies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert!(
        satisfies.contains(&"App/Login/login-build".to_string()),
        "{satisfies:?}"
    );
    assert!(
        satisfies.contains(&"root/app-build".to_string()),
        "{satisfies:?}"
    );
    let ignored = v["ignored_checks"].as_array().unwrap();
    assert_eq!(ignored.len(), 1);
    assert_eq!(ignored[0]["id"], "root/app-build");
    assert!(ignored[0]["reason"]
        .as_str()
        .unwrap()
        .contains("subsumed by a deeper bazel/build target"));
}

#[test]
#[serial]
fn bazel_build_evidence_accepts_text_plus_log() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("MUSTS.yml"),
        r#"version: 1
checks:
  app-build:
    uses: bazel/build
    with:
      target: //App:App
"#,
    );
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(1);
    // Write the log OUTSIDE the workspace; writing inside the workspace
    // would mutate the scope hash and stale our just-issued task.
    let log_dir = TempDir::new().unwrap();
    let log = log_dir.path().join("build.log");
    fs::write(
        &log,
        b"bazel build //App:App\nINFO: Build completed successfully\n",
    )
    .unwrap();
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg("bazel-build-root")
        .arg("--text")
        .arg("bazel build //App:App succeeded")
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
fn bazel_build_evidence_rejects_missing_text_or_log() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("MUSTS.yml"),
        r#"version: 1
checks:
  c:
    uses: bazel/build
    with:
      target: //x
"#,
    );
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .assert()
        .failure()
        .code(1);

    // Missing the log asset entirely.
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg("bazel-build-root")
        .arg("--text")
        .arg("ok")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("rejected"))
        .stderr(predicate::str::contains("log"));
}
