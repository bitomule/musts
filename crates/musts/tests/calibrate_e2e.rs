//! End-to-end tests for `musts calibrate`.
//!
//! What these cover is the half that does not need a model: the command
//! surface, and the refusal to pretend. A real calibration needs jev and
//! a key, so it cannot run in CI — the classification itself is unit
//! tested in `musts_core::calibrate` against the probabilities this
//! repo's own question actually returned.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("musts").expect("musts binary not built")
}

fn workspace(manifest: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join("MUSTS.yml"), manifest).unwrap();
    dir
}

#[test]
fn a_workspace_with_no_judgment_checks_says_so_instead_of_reporting_nothing() {
    let dir = workspace(
        "version: 1\nchecks:\n  fmt:\n    uses: cargo/fmt\n    paths:\n      - \"**/*.rs\"\n",
    );
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("calibrate")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "No `uses: jev` checks declared. Nothing to calibrate.",
        ));
}

/// A calibration whose controls never ran proves less than no
/// calibration: it must fail loudly and write nothing, rather than
/// record a green built on nothing. `JEVI_DISABLE` is the same door CI
/// goes through.
#[test]
fn a_run_whose_controls_could_not_be_judged_fails_and_records_nothing() {
    let dir = workspace(
        r#"version: 1
checks:
  judged:
    uses: jev
    paths:
      - "**/*.rs"
    with:
      ask: whatever
      expect: "no"
      mode: shadow
      control:
        violating: violating.rs
        clean: clean.rs
      question:
        type: noul
        instructions: "Something is wrong in this file."
        criteria:
          "true": "it is"
          "false": "it is not"
"#,
    );
    std::fs::write(dir.path().join("violating.rs"), "fn a() {}\n").unwrap();
    std::fs::write(dir.path().join("clean.rs"), "fn b() {}\n").unwrap();

    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("calibrate")
        .env("JEVI_DISABLE", "1")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Nothing was calibrated"));
    assert!(
        !dir.path().join(".musts/calibration.json").exists(),
        "a calibration that could not run must leave no record behind"
    );
}

/// `--no-record` is the flag you reach for to look without committing
/// to anything; it must not touch the record either.
#[test]
fn no_record_writes_no_file() {
    let dir = workspace("version: 1\nchecks: {}\n");
    bin()
        .arg("--workspace")
        .arg(dir.path())
        .arg("calibrate")
        .arg("--no-record")
        .assert()
        .success();
    assert!(!dir.path().join(".musts/calibration.json").exists());
}
