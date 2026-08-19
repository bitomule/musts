//! E2E for `musts lint`.
//!
//! Fixtures are the real manifests the audit was drawn from, trimmed to
//! the offending shape.

use std::path::Path;
use std::process::Command;

mod common;

fn musts_bin() -> std::path::PathBuf {
    common::workspace_binary("musts", "musts")
}

fn ws(manifest: &str, files: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("MUSTS.yml"), manifest).unwrap();
    for f in files {
        let p = dir.path().join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "x\n").unwrap();
    }
    dir
}

fn lint(root: &Path, extra: &[&str]) -> (String, i32) {
    let out = Command::new(musts_bin())
        .arg("--workspace")
        .arg(root)
        .arg("lint")
        .args(extra)
        .output()
        .expect("run musts lint");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// HiddenFace, both checks: `Run `bazelisk test …` and confirm … (exit 0)`
/// under `uses: agent`.
#[test]
fn a_deterministic_fact_under_agent_is_reported() {
    let dir = ws(
        "version: 1\nchecks:\n  unit:\n    uses: agent\n    paths: [\"src/**\"]\n    with:\n      facts:\n        - \"Run `bazelisk test //T:T` and confirm all unit tests pass (exit 0).\"\n",
        &["src/a.swift"],
    );
    let (stdout, code) = lint(dir.path(), &[]);
    assert!(
        stdout.contains("deterministic-fact-under-agent"),
        "{stdout}"
    );
    assert!(
        stdout.contains("musts run"),
        "must name the cheaper path: {stdout}"
    );
    assert_eq!(code, 0, "advice must not gate CI");
}

/// Nokoru's `live-diarization-matrix` / `device-speech-harness`.
#[test]
fn a_path_condition_in_prose_is_reported() {
    let dir = ws(
        "version: 1\nchecks:\n  matrix:\n    uses: agent\n    paths: [\"src/**\"]\n    with:\n      facts:\n        - \"If changes touch src/Transcription/**, the matrix was re-run.\"\n        - \"If changes are unrelated to those paths, this check is trivially satisfied.\"\n",
        &["src/a.swift"],
    );
    let (stdout, _) = lint(dir.path(), &[]);
    assert!(stdout.contains("paths-written-as-prose"), "{stdout}");
    assert!(stdout.contains("`paths:`"), "{stdout}");
}

/// The trap one repo documented in a 15-line header comment rather than
/// being told about: `*View.swift` is case-insensitive, so it also
/// matches `RequestReview.swift`.
#[test]
fn a_case_insensitive_glob_collision_names_the_real_file() {
    let dir = ws(
        "version: 1\nchecks:\n  snapshot:\n    uses: agent\n    paths: [\"src/*View.swift\"]\n    with:\n      facts: [\"f\"]\n",
        &["src/HomeView.swift", "src/RequestReview.swift"],
    );
    let (stdout, _) = lint(dir.path(), &[]);
    assert!(stdout.contains("glob-case-insensitive"), "{stdout}");
    assert!(stdout.contains("RequestReview.swift"), "{stdout}");
}

#[test]
fn a_star_crossing_directories_names_the_real_file() {
    let dir = ws(
        "version: 1\nchecks:\n  ui:\n    uses: agent\n    paths: [\"src/*.swift\"]\n    with:\n      facts: [\"f\"]\n",
        &["src/a.swift", "src/deep/b.swift"],
    );
    let (stdout, _) = lint(dir.path(), &[]);
    assert!(stdout.contains("glob-crosses-directories"), "{stdout}");
    assert!(stdout.contains("src/deep/b.swift"), "{stdout}");
}

/// Found in Nokoru: a check whose every glob matches nothing, so it can
/// never fire. Invisible without this.
#[test]
fn a_glob_matching_nothing_is_reported() {
    let dir = ws(
        "version: 1\nchecks:\n  dead:\n    uses: agent\n    paths: [\"MacOSMainView.swift\"]\n    with:\n      facts: [\"f\"]\n",
        &["src/a.swift"],
    );
    let (stdout, _) = lint(dir.path(), &[]);
    assert!(stdout.contains("glob-matches-nothing"), "{stdout}");
}

/// An unknown key means the manifest does not do what it says, so it is
/// an error and gates CI — unlike everything else here.
#[test]
fn an_unknown_key_is_an_error_and_exits_nonzero() {
    let dir = ws(
        "version: 1\nchecks:\n  c:\n    uses: agent\n    paths: [\"src/**\"]\n    excludes: [\"src/gen/**\"]\n    with:\n      facts: [\"f\"]\n",
        &["src/a.swift"],
    );
    let (stdout, code) = lint(dir.path(), &[]);
    assert!(stdout.contains("unknown-key"), "{stdout}");
    assert!(stdout.contains("error"), "{stdout}");
    assert_eq!(code, 1, "an error-level finding must gate CI");
}

#[test]
fn a_clean_manifest_says_so_and_exits_zero() {
    let dir = ws(
        "version: 1\nchecks:\n  test:\n    uses: cargo/test\n    paths: [\"src/**/*.rs\"]\n",
        &["src/a.rs"],
    );
    let (stdout, code) = lint(dir.path(), &[]);
    assert!(stdout.contains("No authoring problems found"), "{stdout}");
    assert_eq!(code, 0);
}

/// A runnable check with no `paths:` is correct and cheap — `musts run`
/// executes it and no agent reads the output. Warning there would fire
/// on every root manifest in every repo and train people to ignore lint.
#[test]
fn a_runnable_check_without_paths_is_not_nagged_about() {
    let dir = ws(
        "version: 1\nchecks:\n  fmt:\n    uses: cargo/fmt\n",
        &["src/a.rs"],
    );
    let (stdout, code) = lint(dir.path(), &[]);
    assert!(!stdout.contains("no-paths-filter"), "{stdout}");
    assert_eq!(code, 0);
}

#[test]
fn an_agent_check_without_paths_reports_its_real_blast_radius() {
    let dir = ws(
        "version: 1\nchecks:\n  everything:\n    uses: agent\n    with:\n      facts: [\"f\"]\n",
        &["src/a.swift", "src/b.swift", "docs/c.md"],
    );
    let (stdout, _) = lint(dir.path(), &[]);
    assert!(stdout.contains("no-paths-filter"), "{stdout}");
    assert!(stdout.contains("file(s) today"), "{stdout}");
}

#[test]
fn json_output_is_machine_readable() {
    let dir = ws(
        "version: 1\nchecks:\n  c:\n    uses: agent\n    paths: [\"src/**\"]\n    with:\n      facts:\n        - \"Run `make test` and confirm it exited 0.\"\n",
        &["src/a.swift"],
    );
    let (stdout, _) = lint(dir.path(), &["--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let f = &v["findings"][0];
    assert_eq!(f["rule"], "deterministic-fact-under-agent");
    assert_eq!(f["severity"], "warning");
    assert_eq!(f["check"], "root/c");
    assert_eq!(f["manifest"], "MUSTS.yml");
}

/// Lint is static. It must not need — or create — any state.
#[test]
fn lint_creates_no_state_directory() {
    let dir = ws(
        "version: 1\nchecks:\n  c:\n    uses: cargo/fmt\n    paths: [\"src/**\"]\n",
        &["src/a.rs"],
    );
    lint(dir.path(), &[]);
    assert!(
        !dir.path().join(".musts").exists(),
        "lint must not bootstrap state"
    );
}

/// A `.mustsignore` rule that hides the files a check exists to protect
/// is the expensive version of this rule. Boxy's `*.png` was meant for
/// root-level screenshots; unanchored, it hid 122 snapshot baselines and
/// the snapshot check went blind.
#[test]
fn an_ignore_rule_hiding_the_files_is_named_as_the_cause() {
    let dir = ws(
        "version: 1\nchecks:\n  snapshot:\n    uses: agent\n    paths: [\"Tests/__Snapshots__/**\"]\n    with:\n      facts: [\"f\"]\n",
        &["Tests/__Snapshots__/Foo/a.1.png"],
    );
    std::fs::write(dir.path().join(".mustsignore"), "*.png\n").unwrap();
    let (stdout, _) = lint(dir.path(), &[]);
    assert!(stdout.contains("exist and are excluded"), "{stdout}");
    assert!(stdout.contains("blind to them"), "{stdout}");
    assert!(!stdout.contains("Check for a typo"), "{stdout}");
}

#[test]
fn a_truly_missing_path_still_reads_as_a_typo() {
    let dir = ws(
        "version: 1\nchecks:\n  c:\n    uses: agent\n    paths: [\"NoSuchDir/**\"]\n    with:\n      facts: [\"f\"]\n",
        &["src/a.swift"],
    );
    let (stdout, _) = lint(dir.path(), &[]);
    assert!(stdout.contains("Check for a typo"), "{stdout}");
}

/// `paths:` is workspace-relative even in a nested manifest. Writing it
/// relative to the manifest's own folder silently matches nothing.
#[test]
fn a_nested_manifest_with_a_relative_glob_gets_the_prefix_suggested() {
    let dir = ws("version: 1\nchecks: {}\n", &["Sub/UI/a.swift"]);
    std::fs::write(
        dir.path().join("Sub/MUSTS.yml"),
        "version: 1\nchecks:\n  ui:\n    uses: agent\n    paths: [\"UI/*.swift\"]\n    with:\n      facts: [\"f\"]\n",
    )
    .unwrap();
    let (stdout, _) = lint(dir.path(), &[]);
    assert!(stdout.contains("`Sub/UI/*.swift` would"), "{stdout}");
    assert!(stdout.contains("workspace root"), "{stdout}");
}

/// A correct finding can still be unwanted: one repo's manifest header
/// deliberately reasons about `*` crossing `/` and builds its globs to be
/// disjoint because of it. A lint nobody can silence is a lint everybody
/// ignores.
#[test]
fn a_suppression_comment_silences_that_rule_only() {
    let manifest = |header: &str| {
        format!(
            "version: 1\n{header}checks:\n  c:\n    uses: agent\n    paths: [\"src/*.swift\"]\n    with:\n      facts: [\"f\"]\n"
        )
    };
    let files = ["src/a.swift", "src/deep/b.swift"];

    let noisy = ws(&manifest(""), &files);
    assert!(
        lint(noisy.path(), &[])
            .0
            .contains("glob-crosses-directories"),
        "baseline: the warning must fire without a suppression"
    );

    let quiet = ws(
        &manifest("# musts-lint: allow glob-crosses-directories\n"),
        &files,
    );
    let (stdout, code) = lint(quiet.path(), &[]);
    assert!(!stdout.contains("glob-crosses-directories"), "{stdout}");
    assert_eq!(code, 0);

    let other = ws(
        &manifest("# musts-lint: allow glob-matches-nothing\n"),
        &files,
    );
    assert!(
        lint(other.path(), &[])
            .0
            .contains("glob-crosses-directories"),
        "suppressing one rule must not mute the rest"
    );
}

/// Suppressing an error-level rule must also drop the exit code, or the
/// opt-out does not actually let CI pass.
#[test]
fn suppressing_an_error_rule_also_clears_the_exit_code() {
    let dir = ws(
        "version: 1\n# musts-lint: allow unknown-key\nchecks:\n  c:\n    uses: agent\n    paths: [\"src/**\"]\n    excludes: [\"x\"]\n    with:\n      facts: [\"f\"]\n",
        &["src/a.swift"],
    );
    let (stdout, code) = lint(dir.path(), &[]);
    assert!(!stdout.contains("unknown-key"), "{stdout}");
    assert_eq!(code, 0, "a suppressed error must not gate CI");
}
