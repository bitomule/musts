//! `paths:` are relative to the declaring manifest's folder.
//!
//! They used to be workspace-relative, so a manifest at
//! `App/macOSUI/MainWindow/` had to repeat its own location in every
//! pattern. Two independent authors wrote the intuitive form instead —
//! and because a filter matching nothing silently deleted the check, one
//! of them went unnoticed for 89 days.
//!
//! Two things had to change together: the semantics, and the silence. The
//! second matters more — without it, this change would just move the trap
//! rather than remove it.

use std::path::Path;
use std::process::Command;

mod common;

fn musts_bin() -> std::path::PathBuf {
    common::workspace_binary("musts", "musts")
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// Root manifest with no checks, plus a nested manifest whose `paths:`
/// are written in `pattern`.
fn workspace(pattern: &str) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    write(dir.path(), "MUSTS.yml", "version: 1\nchecks: {}\n");
    write(
        dir.path(),
        "App/macOSUI/MainWindow/MUSTS.yml",
        &format!(
            "version: 1\nchecks:\n  sidebar:\n    uses: agent\n    paths: [\"{pattern}\"]\n    with:\n      facts: [\"sidebar ok\"]\n"
        ),
    );
    write(
        dir.path(),
        "App/macOSUI/MainWindow/MacOSMainView.swift",
        "// view\n",
    );
    write(
        dir.path(),
        "App/macOSUI/MainWindow/Other.swift",
        "// other\n",
    );
    dir
}

fn validate(root: &Path) -> (String, i32) {
    let out = Command::new(musts_bin())
        .arg("--workspace")
        .arg(root)
        .arg("validate")
        .output()
        .expect("run musts validate");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// The form every author writes. It must fire.
#[test]
fn a_bare_filename_in_a_nested_manifest_matches_its_neighbour() {
    let dir = workspace("MacOSMainView.swift");
    let (stdout, code) = validate(dir.path());
    assert_eq!(code, 1, "the check must be pending, not absent:\n{stdout}");
    assert!(stdout.contains("sidebar ok"), "{stdout}");
}

#[test]
fn a_glob_in_a_nested_manifest_is_relative_too() {
    let dir = workspace("*View.swift");
    let (stdout, code) = validate(dir.path());
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("sidebar ok"), "{stdout}");
}

/// The migration hazard, and the reason the silence had to go: a pattern
/// still carrying the manifest's own folder now matches nothing. It must
/// say so, and say what to write instead.
#[test]
fn a_scope_prefixed_pattern_is_reported_with_the_fix() {
    let dir = workspace("App/macOSUI/MainWindow/MacOSMainView.swift");
    let (stdout, _) = validate(dir.path());
    assert!(stdout.contains("Ignored checks"), "{stdout}");
    assert!(
        stdout.contains("cannot fire"),
        "must say the check cannot fire: {stdout}"
    );
    assert!(
        stdout.contains("still carries this manifest's own folder"),
        "{stdout}"
    );
    assert!(
        stdout.contains("write `MacOSMainView.swift`"),
        "must print the corrected pattern: {stdout}"
    );
}

/// The core regression this whole change exists to prevent: a check whose
/// `paths:` match nothing must never vanish from every surface while
/// `validate` reports clean.
#[test]
fn a_check_that_cannot_fire_is_never_silently_dropped() {
    let dir = workspace("NoSuchFile.swift");
    let (stdout, code) = validate(dir.path());
    assert!(
        stdout.contains("App/macOSUI/MainWindow/sidebar"),
        "the check must appear somewhere in the report:\n{stdout}"
    );
    assert!(stdout.contains("cannot fire"), "{stdout}");
    // Nothing to validate, so no task — but the check was not hidden.
    assert_eq!(code, 0, "{stdout}");
}

#[test]
fn an_inapplicable_check_appears_in_json_ignored_checks() {
    let dir = workspace("NoSuchFile.swift");
    let out = Command::new(musts_bin())
        .arg("--workspace")
        .arg(dir.path())
        .args(["validate", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("valid JSON");
    let ignored = v["ignored_checks"].as_array().unwrap();
    assert_eq!(ignored.len(), 1, "{v}");
    assert_eq!(ignored[0]["id"], "App/macOSUI/MainWindow/sidebar");
    assert!(ignored[0]["reason"]
        .as_str()
        .unwrap()
        .contains("cannot fire"));
}

/// A root manifest's folder *is* the workspace root, so nothing changes
/// there — which is why the root manifests across seven repos were
/// unaffected by this migration.
#[test]
fn a_root_manifest_is_unaffected() {
    let dir = tempfile::TempDir::new().unwrap();
    write(
        dir.path(),
        "MUSTS.yml",
        "version: 1\nchecks:\n  c:\n    uses: agent\n    paths: [\"src/**/*.rs\"]\n    with:\n      facts: [\"f\"]\n",
    );
    write(dir.path(), "src/deep/a.rs", "// x\n");
    let (stdout, code) = validate(dir.path());
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("agent-root"), "{stdout}");
}

/// `exclude_paths:` has to move with `paths:`, or a check would filter
/// includes one way and excludes another.
#[test]
fn exclude_paths_is_manifest_relative_too() {
    let dir = tempfile::TempDir::new().unwrap();
    write(dir.path(), "MUSTS.yml", "version: 1\nchecks: {}\n");
    write(
        dir.path(),
        "Sub/MUSTS.yml",
        "version: 1\nchecks:\n  c:\n    uses: agent\n    paths: [\"**/*.swift\"]\n    exclude_paths: [\"Generated/**\"]\n    with:\n      facts: [\"f\"]\n",
    );
    write(dir.path(), "Sub/Generated/g.swift", "// generated\n");
    let (stdout, code) = validate(dir.path());
    // Only the generated file exists and it is excluded, so the check has
    // nothing in scope — reported, not hidden.
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("cannot fire"), "{stdout}");

    // Add a non-excluded file and the check becomes applicable.
    write(dir.path(), "Sub/Real.swift", "// real\n");
    let (stdout, code) = validate(dir.path());
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("agent-sub"), "{stdout}");
}
