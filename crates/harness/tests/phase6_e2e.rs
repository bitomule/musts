//! Phase 6 end-to-end scenarios per `docs/PLAN.md` §7.3 and §9 Phase 6:
//! scenario 7 (`mav_groups_expectations`) and the full §15 worked example.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("harness").expect("harness binary not built")
}

fn extension_binary(name: &str) -> PathBuf {
    let package = match name {
        "bazel-extension" => "bazel-build-extension",
        "mav-extension" => "mav-expect-extension",
        "stub-extension" => "stub-extension",
        other => panic!("unknown extension binary `{other}`"),
    };
    common::workspace_binary(package, name)
}

fn project_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("workspace root")
}

fn install_mav_descriptor(workspace: &Path) {
    let dir = workspace.join(".harness/extensions/mav");
    let schemas = dir.join("schemas");
    fs::create_dir_all(&schemas).unwrap();
    let source = project_root().join("extensions/mav-expect/schemas/expect.schema.json");
    fs::copy(&source, schemas.join("expect.schema.json")).unwrap();
    let bin = extension_binary("mav-extension");
    fs::write(
        dir.join("extension.yml"),
        format!(
            r#"name: mav
version: 0.1.0
capabilities:
  expect:
    uses: mav/expect
    schema: schemas/expect.schema.json
    resolve:
      command: [{bin:?}, "resolve"]
    evidence:
      command: [{bin:?}, "evidence"]
"#,
            bin = bin.display().to_string(),
        ),
    )
    .unwrap();
}

fn install_bazel_descriptor(workspace: &Path) {
    let dir = workspace.join(".harness/extensions/bazel");
    let schemas = dir.join("schemas");
    fs::create_dir_all(&schemas).unwrap();
    let source = project_root().join("extensions/bazel-build/schemas/build.schema.json");
    fs::copy(&source, schemas.join("build.schema.json")).unwrap();
    let bin = extension_binary("bazel-extension");
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
            bin = bin.display().to_string(),
        ),
    )
    .unwrap();
}

fn write_manifest(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

// ---------------------------------------------------------------------------
// Scenario 7: mav_groups_expectations
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn scenario_7_mav_groups_expectations() {
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        r#"version: 1
checks:
  valid:
    uses: mav/expect
    with:
      expectations:
        - Login works with multiple valid emails.
      evidence:
        - screenshot
        - video
  invalid:
    uses: mav/expect
    with:
      expectations:
        - Invalid email text shows an error when used as email.
      evidence:
        - screenshot
        - mav-report
"#,
    );
    install_mav_descriptor(dir.path());

    let out = bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .arg("--json")
        .assert()
        .failure()
        .code(1);
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    // Both checks fold into one task.
    assert_eq!(v["tasks"].as_array().unwrap().len(), 1);
    let task = &v["tasks"][0];
    let satisfies: Vec<String> = task["satisfies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert!(satisfies.contains(&"root/valid".to_string()));
    assert!(satisfies.contains(&"root/invalid".to_string()));
    // Union of evidence kinds.
    let kinds: Vec<String> = task["evidence_contract"]["assets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap().to_string())
        .collect();
    assert!(kinds.contains(&"screenshot".to_string()));
    assert!(kinds.contains(&"video".to_string()));
    assert!(kinds.contains(&"mav-report".to_string()));
}

// ---------------------------------------------------------------------------
// Full §15 worked example
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn mav_rejects_non_json_mav_report() {
    // PLAN.md §6.2 mandates mav-report must be parseable JSON. Submit
    // garbage with `application/json` MIME and confirm rejection.
    let dir = TempDir::new().unwrap();
    write_manifest(
        &dir.path().join("HARNESS.yml"),
        r#"version: 1
checks:
  flow:
    uses: mav/expect
    with:
      expectations:
        - E
      evidence:
        - mav-report
"#,
    );
    install_mav_descriptor(dir.path());

    let out = bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .arg("--json")
        .assert()
        .failure()
        .code(1);
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let task_id = v["tasks"][0]["id"].as_str().unwrap().to_string();

    // Garbage bytes, but the file ends in .json so MIME detection
    // returns application/json, classifying it as mav-report.
    let assets = TempDir::new().unwrap();
    let report = assets.path().join("report.json");
    fs::write(&report, b"this is not valid JSON at all").unwrap();
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg(&task_id)
        .arg("--text")
        .arg("ok")
        .arg("--asset")
        .arg(&report)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not parseable JSON"));
}

#[test]
#[serial]
fn full_section_15_worked_example() {
    // Mirrors spec §15: root bazel/build, App/Login bazel/build +
    // mav/expect. Modifying App/Login/LoginView.swift dirties both
    // capabilities; bazel/build picks the deepest target, mav/expect
    // emits a single MAV task. After both evidences are recorded,
    // `harness validate` is clean.

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

  login-flow:
    uses: mav/expect
    with:
      expectations:
        - Login works with multiple valid emails.
        - Invalid email text shows an error when used as email.
      evidence:
        - screenshot
        - video
        - mav-report
"#,
    );
    write_manifest(
        &dir.path().join("App/Login/LoginView.swift"),
        "struct LoginView { let v = 1 }\n",
    );
    install_bazel_descriptor(dir.path());
    install_mav_descriptor(dir.path());

    // First validate emits one task per capability for the App/Login scope.
    let out = bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("validate")
        .arg("--json")
        .assert()
        .failure()
        .code(1);
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let tasks: Vec<&serde_json::Value> = v["tasks"].as_array().unwrap().iter().collect();
    assert_eq!(tasks.len(), 2, "expected one bazel task + one mav task");
    let mut bazel_task_id: Option<String> = None;
    let mut mav_task_id: Option<String> = None;
    for t in &tasks {
        match t["extension"].as_str().unwrap() {
            "bazel/build" => bazel_task_id = Some(t["id"].as_str().unwrap().into()),
            "mav/expect" => mav_task_id = Some(t["id"].as_str().unwrap().into()),
            other => panic!("unexpected extension {other}"),
        }
    }
    let bazel_task_id = bazel_task_id.expect("bazel task present");
    let mav_task_id = mav_task_id.expect("mav task present");

    // Root bazel/build is in ignored_checks (deepest target wins).
    let ignored: Vec<String> = v["ignored_checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ignored.contains(&"root/app-build".to_string()));

    // Submit evidence for both tasks. Assets live OUTSIDE the workspace
    // so we don't mutate scope hashes.
    let assets = TempDir::new().unwrap();
    let log = assets.path().join("login-build.log");
    fs::write(&log, b"bazel build //App/Login:Login\nSUCCESS\n").unwrap();
    let screen = assets.path().join("login.png");
    fs::write(&screen, vec![0x89, 0x50, 0x4E, 0x47]).unwrap();
    let video = assets.path().join("login.mp4");
    fs::write(&video, vec![0; 256]).unwrap();
    let report = assets.path().join("mav-report.json");
    fs::write(&report, br#"{"summary":"ok"}"#).unwrap();

    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg(&bazel_task_id)
        .arg("--text")
        .arg("bazel build //App/Login:Login succeeded")
        .arg("--asset")
        .arg(&log)
        .assert()
        .success();
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("evidence")
        .arg(&mav_task_id)
        .arg("--text")
        .arg("MAV: validated both expectations")
        .arg("--asset")
        .arg(&screen)
        .arg("--asset")
        .arg(&video)
        .arg("--asset")
        .arg(&report)
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
