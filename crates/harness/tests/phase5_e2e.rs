//! Phase 5 end-to-end scenarios per `docs/PLAN.md` §7.3 and §9 Phase 5:
//! scenario 6 (`bazel_picks_deepest_target`) and the build half of §15.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("harness").expect("harness binary not built")
}

fn bazel_binary() -> PathBuf {
    let test_bin = std::env::current_exe().unwrap();
    let profile = test_bin.parent().unwrap().parent().unwrap();
    profile.join("bazel-extension")
}

fn project_root() -> PathBuf {
    // crates/harness/tests/phase5_e2e.rs → ../../../
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("workspace root")
}

fn install_bazel_descriptor(workspace: &Path) {
    let dir = workspace.join(".harness/extensions/bazel");
    let schemas = dir.join("schemas");
    fs::create_dir_all(&schemas).unwrap();
    let source_schema = project_root().join("extensions/bazel-build/schemas/build.schema.json");
    fs::copy(&source_schema, schemas.join("build.schema.json")).unwrap();
    let stub = bazel_binary();
    fs::write(
        dir.join("extension.yml"),
        format!(
            r#"name: bazel
version: 0.1.0
capabilities:
  build:
    uses: bazel/build
    schema: schemas/build.schema.json
    resolve:
      command: [{bin:?}, "resolve"]
    evidence:
      command: [{bin:?}, "evidence"]
"#,
            bin = stub.display().to_string(),
        ),
    )
    .unwrap();
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
        &dir.path().join("HARNESS.yml"),
        r#"version: 1
checks:
  app-build:
    uses: bazel/build
    with:
      target: //App:App
"#,
    );
    write_manifest(
        &dir.path().join("App/Login/HARNESS.yml"),
        r#"version: 1
checks:
  login-build:
    uses: bazel/build
    with:
      target: //App/Login:Login
"#,
    );
    install_bazel_descriptor(dir.path());

    let out = bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .arg("--json")
        .assert()
        .failure()
        .code(1);
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    // Exactly one task: the deepest target. Root is ignored.
    assert_eq!(v["tasks"].as_array().unwrap().len(), 1);
    let task = &v["tasks"][0];
    assert!(task["title"]
        .as_str()
        .unwrap()
        .contains("//App/Login:Login"));
    assert_eq!(task["satisfies"][0], "App/Login/login-build");
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
        &dir.path().join("HARNESS.yml"),
        r#"version: 1
checks:
  app-build:
    uses: bazel/build
    with:
      target: //App:App
"#,
    );
    install_bazel_descriptor(dir.path());
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
        .stdout(predicate::str::contains("Harness validation clean."));
}

#[test]
#[serial]
fn bazel_build_evidence_rejects_missing_text_or_log() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        r#"version: 1
checks:
  c:
    uses: bazel/build
    with:
      target: //x
"#,
    );
    install_bazel_descriptor(dir.path());
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
