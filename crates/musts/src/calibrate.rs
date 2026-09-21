//! `musts calibrate` — the I/O half of calibration.
//!
//! The judgement lives in `musts_core::calibrate`, which is pure. This
//! file does the two things that are not: it reads file states out of
//! git history, and it runs `musts-jev` on them. The core never talks to
//! the network and never shells out, exactly as for every other
//! capability.
//!
//! Three rules about the sample, and the first is the one that matters:
//! a question calibrated against changes it already approved blesses
//! itself. So samples come only from commits **strictly older** than the
//! commit that introduced the question, and when that leaves fewer
//! samples than asked for, the report says so instead of reaching for
//! newer history.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{anyhow, Context};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use musts_core::calibrate::{
    render_text, summarise, CalibrationRecord, ControlResult, Controls, Expect, QuestionReport,
    Run, Sample,
};
use musts_core::manifest;
use serde_json::Value;

/// Commits inspected while looking for enough samples. A question whose
/// scope is a corner of the repo needs to look further back than one
/// covering everything.
const DEFAULT_SCAN: usize = 60;
pub const DEFAULT_SAMPLES: usize = 20;

/// Where the committed record lives.
const RECORD_PATH: &str = ".musts/calibration.json";

/// One `uses: jev` check, read out of the manifest.
struct JevCheck {
    check_id: String,
    ask: String,
    expect: Expect,
    /// Path to the question set, workspace-relative. `None` for an
    /// inline `question:`, which has no file and therefore no
    /// introduction commit.
    questions: Option<String>,
    /// The `--questions`/`--question-json` argument, ready to pass on.
    question_arg: Vec<String>,
    control: Option<(String, String)>,
    /// Passed through so a calibration measures the check as it actually runs. A question
    /// narrowed to emission sites in production and calibrated against whole files is
    /// calibrated against something else, and the controls would then hold for a mode nobody
    /// uses.
    sites: Option<String>,
    scope: PathFilter,
    /// Manifest folder, workspace-relative. `paths:` are relative to it.
    manifest_dir: PathBuf,
}

struct PathFilter {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

impl PathFilter {
    /// Same semantics as a check's effective scope in
    /// `musts_core::validate`: case-insensitive, and `*` stops at `/`.
    fn compile(paths: &[String], exclude: &[String]) -> anyhow::Result<Self> {
        Ok(Self {
            include: Self::set(paths)?,
            exclude: Self::set(exclude)?,
        })
    }

    fn set(patterns: &[String]) -> anyhow::Result<Option<GlobSet>> {
        if patterns.is_empty() {
            return Ok(None);
        }
        let mut builder = GlobSetBuilder::new();
        for pat in patterns {
            builder.add(
                GlobBuilder::new(pat)
                    .case_insensitive(true)
                    .literal_separator(true)
                    .build()
                    .with_context(|| format!("invalid glob `{pat}`"))?,
            );
        }
        Ok(Some(builder.build()?))
    }

    fn matches(&self, rel: &str) -> bool {
        let included = self.include.as_ref().is_none_or(|s| s.is_match(rel));
        let excluded = self.exclude.as_ref().is_some_and(|s| s.is_match(rel));
        included && !excluded
    }
}

pub fn run(
    root: &Path,
    samples_wanted: usize,
    scan: usize,
    json: bool,
    write: bool,
) -> anyhow::Result<ExitCode> {
    let checks = jev_checks(root)?;
    if checks.is_empty() {
        println!("No `uses: jev` checks declared. Nothing to calibrate.");
        return Ok(ExitCode::from(0));
    }
    let jev = jev_binary();
    let work = std::env::temp_dir().join(format!("musts-calibrate-{}", std::process::id()));
    std::fs::create_dir_all(&work)?;

    let mut reports = Vec::new();
    let mut model = None;
    for check in &checks {
        let report = calibrate_one(root, &work, &jev, check, samples_wanted, scan, &mut model)?;
        reports.push(report);
    }
    let _ = std::fs::remove_dir_all(&work);

    let head = git(root, &["rev-parse", "HEAD"])
        .map_or_else(|| "unknown".to_string(), |h| h.trim().to_string());
    let record = CalibrationRecord::new(now(), head, reports);

    if json {
        println!("{}", serde_json::to_string_pretty(&record)?);
    } else {
        print!("{}", render_text(&record));
    }
    if write {
        let path = root.join(RECORD_PATH);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&record)?),
        )?;
        if !json {
            println!("\nRecorded in {RECORD_PATH}. No manifest was edited.");
        }
    }

    // A verdict that asks a person to act is not an error: the exit code
    // stays 0 so calibration never becomes a gate nobody can pass. What
    // it is, is printed.
    Ok(ExitCode::from(0))
}

fn calibrate_one(
    root: &Path,
    work: &Path,
    jev: &Path,
    check: &JevCheck,
    samples_wanted: usize,
    scan: usize,
    model: &mut Option<String>,
) -> anyhow::Result<QuestionReport> {
    let controls = match &check.control {
        Some((violating, clean)) => {
            let v = ask(jev, check, root, violating)?;
            let c = ask(jev, check, root, clean)?;
            for (result, path) in [(&v, violating), (&c, clean)] {
                if result.verdict == "skipped" || result.verdict == "broken" {
                    return Err(anyhow!(
                        "jev could not judge the control `{path}` ({}). Nothing was calibrated, \
                         and nothing was written: a calibration whose controls did not run \
                         proves less than no calibration at all.",
                        result.note.as_deref().unwrap_or("no reason given")
                    ));
                }
            }
            model.clone_from(&v.model);
            Some(Controls {
                violating: ControlResult {
                    file: violating.clone(),
                    p: v.p.unwrap_or(0.0),
                    fired: fired(&v.verdict),
                    verdict: v.verdict,
                },
                clean: ControlResult {
                    file: clean.clone(),
                    p: c.p.unwrap_or(0.0),
                    fired: fired(&c.verdict),
                    verdict: c.verdict,
                },
            })
        }
        None => None,
    };

    let introduced_at = introduced_at(root, check);
    let picked = pick_samples(root, check, introduced_at.as_deref(), samples_wanted, scan);

    let mut samples = Vec::new();
    for (commit, file) in picked {
        let Some(staged) = stage(root, work, &commit, &file) else {
            continue;
        };
        let answer = ask(jev, check, work, &staged)?;
        if answer.verdict == "skipped" || answer.verdict == "broken" {
            continue;
        }
        if model.is_none() {
            model.clone_from(&answer.model);
        }
        samples.push(Sample {
            file,
            commit,
            p: answer.p.unwrap_or(0.0),
            verdict: answer.verdict,
        });
    }

    Ok(summarise(Run {
        check_id: check.check_id.clone(),
        ask: check.ask.clone(),
        expect: check.expect,
        introduced_at,
        samples_wanted,
        samples,
        controls,
        model: model.clone(),
    }))
}

/// Commits older than the one that introduced the question, newest
/// first, with one file state per path. Deduping by path keeps a
/// churn-heavy file from being the whole sample.
fn pick_samples(
    root: &Path,
    check: &JevCheck,
    introduced_at: Option<&str>,
    wanted: usize,
    scan: usize,
) -> Vec<(String, String)> {
    let start = introduced_at.map_or_else(|| "HEAD".to_string(), |c| format!("{c}^"));
    let Some(log) = git(
        root,
        &[
            "log",
            "--no-merges",
            "--format=%H",
            "-n",
            &scan.to_string(),
            &start,
        ],
    ) else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for commit in log.lines() {
        if out.len() >= wanted {
            break;
        }
        let Some(files) = git(
            root,
            &[
                "show",
                "--format=",
                "--name-only",
                "--diff-filter=AM",
                commit,
            ],
        ) else {
            continue;
        };
        for file in files.lines() {
            if out.len() >= wanted {
                break;
            }
            if file.is_empty() || seen.contains(file) || !in_scope(check, file) {
                continue;
            }
            seen.insert(file.to_string());
            out.push((commit.to_string(), file.to_string()));
        }
    }
    out
}

/// `paths:` are relative to the declaring manifest's folder, so a
/// workspace-relative candidate is stripped of that prefix before it is
/// matched.
fn in_scope(check: &JevCheck, workspace_rel: &str) -> bool {
    let prefix = check.manifest_dir.to_string_lossy().replace('\\', "/");
    let rel = if prefix.is_empty() || prefix == "." {
        workspace_rel
    } else {
        match workspace_rel.strip_prefix(&format!("{prefix}/")) {
            Some(r) => r,
            None => return false,
        }
    };
    check.scope.matches(rel)
}

/// Write a historic file state into the scratch tree at its own relative
/// path, so jev sees the path it really had.
fn stage(root: &Path, work: &Path, commit: &str, file: &str) -> Option<String> {
    let content = git(root, &["show", &format!("{commit}:{file}")])?;
    let target = work.join(file);
    std::fs::create_dir_all(target.parent()?).ok()?;
    std::fs::write(&target, content).ok()?;
    Some(file.to_string())
}

/// Did the check say something about this file? `WARN` counts: it is an abstention that
/// landed on the violating side of even, and it reaches the report exactly as a `FAIL` does.
/// Reading only `FAIL` made a planted control hold or not hold depending on which side of the
/// abstention band that run came back on — measured at 0.87 to 0.93 on one unchanged file,
/// which is a calibration that flips on nothing.
fn fired(verdict: &str) -> bool {
    verdict == "FAIL" || verdict == "WARN"
}

struct Answer {
    verdict: String,
    p: Option<f64>,
    model: Option<String>,
    note: Option<String>,
}

fn ask(jev: &Path, check: &JevCheck, root: &Path, file: &str) -> anyhow::Result<Answer> {
    let mut cmd = Command::new(jev);
    cmd.args(&check.question_arg)
        .args(["--ask", &check.ask])
        .args([
            "--expect",
            match check.expect {
                Expect::Yes => "yes",
                Expect::No => "no",
            },
        ])
        // Never `shadow`: calibration must not append rows to the shadow
        // log, which is a record of what the check saw in real runs.
        .args(["--mode", "tripwire"])
        .args(["--root", &root.to_string_lossy()])
        .arg("--json");
    if let Some(sites) = &check.sites {
        cmd.args(["--sites", sites]);
    }
    cmd.arg(file);
    let out = cmd
        .output()
        .with_context(|| format!("could not run {}", jev.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // With `--sites` one call answers about several sites, so there are several objects. The
    // run itself is red if ANY site fired, and a calibration that read the last object would
    // score a ten-site file by whichever site happens to come last — scoring the wrong line
    // and never saying so.
    let objects: Vec<&str> = stdout
        .lines()
        .filter(|l| l.trim_start().starts_with('{'))
        .collect();
    let line = objects
        .iter()
        .find(|l| l.contains("\"verdict\":\"FAIL\"") || l.contains("\"verdict\":\"WARN\""))
        .or_else(|| objects.last())
        .copied()
        .ok_or_else(|| {
            anyhow!(
                "{} answered nothing readable for {file}: {}",
                jev.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )
        })?;
    let v: Value = serde_json::from_str(line)?;
    Ok(Answer {
        verdict: v["verdict"].as_str().unwrap_or("broken").to_string(),
        p: v["p"].as_f64(),
        model: v["model"].as_str().map(str::to_string),
        note: v["note"].as_str().map(str::to_string),
    })
}

/// The commit where this question first existed: the boundary every
/// sample has to be older than.
///
/// It looks for the `ask:` id anywhere in the repo rather than in the
/// file the question lives in today, and that is the whole subtlety. A
/// question can move — this repo's own `self_comparing` started in
/// `.musts/extensions/jev/questions/self-comparing.json` and later went
/// inline into `MUSTS.yml` — and asking where it lives *now* dates it
/// from the move. Measured here: the narrower lookup answered `202a50a8`
/// when the question had existed since `325574e`, which put the commit
/// that introduced the question back into its own sample. Searching the
/// whole repo for the id gets both spellings with one rule.
fn introduced_at(root: &Path, check: &JevCheck) -> Option<String> {
    let by_id = git(
        root,
        &[
            "log",
            &format!("-S{}", check.ask),
            "--reverse",
            "--format=%H",
        ],
    )
    .and_then(|out| out.lines().next().map(str::to_string));
    let commit = match by_id {
        Some(c) if !c.trim().is_empty() => c,
        // An id too generic to search for, or a history that does not
        // contain it. Fall back to the question file's own birth.
        _ => git(
            root,
            &[
                "log",
                "--diff-filter=A",
                "--format=%H",
                "-1",
                "--",
                check.questions.as_deref()?,
            ],
        )?,
    };
    let commit = commit.trim();
    (!commit.is_empty()).then(|| commit.to_string())
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// The `musts-jev` that ships with *this* `musts` when there is one, so a
/// release binary never silently calibrates through a different build
/// that happens to be earlier on `PATH`.
fn jev_binary() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("musts-jev")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("musts-jev"))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Every `uses: jev` check in the workspace, with what calibration needs
/// from it.
fn jev_checks(root: &Path) -> anyhow::Result<Vec<JevCheck>> {
    let mut out = Vec::new();
    for entry in manifest::discover(root)? {
        let bytes = std::fs::read(&entry.abs_path)?;
        let Ok(parsed) = manifest::parse(&entry.rel_path, &bytes) else {
            continue;
        };
        let scope = manifest::scope_path_for(&entry.rel_path);
        let manifest_dir = entry
            .rel_path
            .parent()
            .map_or_else(PathBuf::new, Path::to_path_buf);
        for (local_id, check) in &parsed.checks {
            if check.uses != "jev" {
                continue;
            }
            let w = &check.with_payload;
            let ask = w["ask"].as_str().unwrap_or_default().to_string();
            let expect =
                Expect::parse(w["expect"].as_str().unwrap_or("yes")).unwrap_or(Expect::Yes);
            let questions = w["questions"].as_str().map(str::to_string);
            let question_arg = match (&questions, w.get("question")) {
                // `builtin:<name>` addresses a question set inside the binary; it is not a
                // path and joining the workspace root onto it produces a file that will
                // never exist.
                (Some(path), _) if path.starts_with("builtin:") => {
                    vec!["--questions".to_string(), path.clone()]
                }
                (Some(path), _) => vec![
                    "--questions".to_string(),
                    root.join(path).display().to_string(),
                ],
                (None, Some(inline)) => vec!["--question-json".to_string(), inline.to_string()],
                (None, None) => continue,
            };
            let control = w["control"]["violating"]
                .as_str()
                .zip(w["control"]["clean"].as_str())
                .map(|(v, c)| (v.to_string(), c.to_string()));
            out.push(JevCheck {
                check_id: manifest::check_id(&scope, local_id),
                ask,
                expect,
                questions,
                question_arg,
                control,
                sites: w["sites"].as_str().map(str::to_string),
                scope: PathFilter::compile(&check.paths, &check.exclude_paths)?,
                manifest_dir: manifest_dir.clone(),
            });
        }
    }
    Ok(out)
}

pub const SCAN_DEFAULT: usize = DEFAULT_SCAN;

#[cfg(test)]
mod tests {
    use super::*;

    fn check(paths: &[&str], exclude: &[&str], dir: &str) -> JevCheck {
        JevCheck {
            check_id: "c".into(),
            ask: "q".into(),
            expect: Expect::No,
            questions: None,
            question_arg: vec![],
            control: None,
            sites: None,
            scope: PathFilter::compile(
                &paths.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
                &exclude.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
            )
            .unwrap(),
            manifest_dir: PathBuf::from(dir),
        }
    }

    #[test]
    fn scope_matches_the_checks_own_paths() {
        let c = check(&["crates/**/*.rs"], &[], "");
        assert!(in_scope(&c, "crates/musts-core/src/lib.rs"));
        assert!(!in_scope(&c, "docs/README.md"));
    }

    /// `paths:` are relative to the manifest folder, so a nested
    /// manifest must not sample files from a sibling subtree.
    #[test]
    fn a_nested_manifests_paths_are_relative_to_its_own_folder() {
        let c = check(&["src/**/*.rs"], &[], "crates/musts-core");
        assert!(in_scope(&c, "crates/musts-core/src/lib.rs"));
        assert!(!in_scope(&c, "crates/musts/src/main.rs"));
        assert!(!in_scope(&c, "src/lib.rs"));
    }

    #[test]
    fn exclude_paths_subtract_after_paths() {
        let c = check(&["crates/**/*.rs"], &["**/tests/**"], "");
        assert!(in_scope(&c, "crates/musts/src/main.rs"));
        assert!(!in_scope(&c, "crates/musts/tests/e2e.rs"));
    }

    /// `*` stopping at `/` is the manifest-wide rule; calibration must
    /// sample the same files the check actually covers, not more.
    #[test]
    fn a_single_star_does_not_cross_a_slash() {
        let c = check(&["crates/*.rs"], &[], "");
        assert!(in_scope(&c, "crates/top.rs"));
        assert!(!in_scope(&c, "crates/musts/src/main.rs"));
    }
}
