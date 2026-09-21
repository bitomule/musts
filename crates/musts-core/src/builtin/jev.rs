//! `jev` — a judgment check a machine can run.
//!
//! The core knows only how to ASK for one: it declares the command and reads the evidence,
//! exactly as `bazel/build` declares `bazel build`. Everything about jev — the key, the
//! request, the verdict — lives in `musts-jev`, which ships with musts and sits on the same
//! PATH.
//!
//! It is a built-in rather than an out-of-tree extension for one reason that is not taste:
//! `musts run` refuses to execute a descriptor-backed extension's command, so as an
//! extension this check would be a manual task forever — which is the cost this capability
//! exists to remove.

use musts_protocol::{
    AssetContract, EvidenceContract, EvidenceValidationRequest, EvidenceValidationResponse,
    ResolveRequest, ResolveResponse, Task, TextContract,
};
use serde_json::{json, Value};
use std::process::Command;
use std::sync::OnceLock;

use crate::error::Error;
use crate::manifest::ids::ROOT_SCOPE;

pub fn schema() -> &'static Value {
    static S: OnceLock<Value> = OnceLock::new();
    S.get_or_init(|| {
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "required": ["ask"],
            "additionalProperties": false,
            "properties": {
                "ask": { "type": "string" },
                "questions": { "type": "string" },
                "question": { "type": "object" },
                "expect": { "type": "string", "enum": ["yes", "no"] },
                "mode": { "type": "string", "enum": ["tripwire", "shadow"] },
                "state": { "type": "string", "enum": ["file", "diff"] },
                "changed_since": { "type": "string" },
                "context_lines": { "type": "integer", "minimum": 0 },
                // The two planted files `musts calibrate` judges the question
                // against: one that really breaks the rule, one near-miss that
                // does not. They live in the manifest rather than in the
                // question set so a reviewer sees what a check was calibrated
                // against without opening a second file, and so nothing in
                // jevi's own format has to be extended to carry them.
                "control": {
                    "type": "object",
                    "required": ["violating", "clean"],
                    "additionalProperties": false,
                    "properties": {
                        "violating": { "type": "string" },
                        "clean": { "type": "string" },
                        // The unchanged file both controls are a one-edit variant of.
                        // Required when the check declares state: diff — a diff question
                        // judged against whole files is calibrated against a mode it never
                        // runs in.
                        "base": { "type": "string" }
                    }
                }
            }
        })
    })
}

fn slug(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect()
}

fn scope_dir(scope_path: &str) -> String {
    if scope_path == ROOT_SCOPE {
        String::new()
    } else {
        scope_path.to_string()
    }
}

/// `changed_files` scoped down to one check.
///
/// `req.changed_files` is the union of every dirty check's scope in this
/// run, not this check's own scope — handing a check a file from a
/// sibling's scope is how a jev task ends up judging a file its own
/// manifest never claimed. Case-insensitive because `changed_files` is
/// always NFC+lowercase (see `snapshot::paths::normalise_rel_path`); this
/// only sorts candidates, it never becomes the path handed to a shell.
fn files_in_scope<'a>(changed_files: &'a [String], dir: &str) -> Vec<&'a str> {
    let prefix = dir.to_lowercase();
    changed_files
        .iter()
        .map(String::as_str)
        .filter(|f| prefix.is_empty() || *f == prefix || f.starts_with(&format!("{prefix}/")))
        .collect()
}

/// Files under `dir` (workspace-relative; empty for the whole workspace)
/// that actually differ between `since` and the working tree, in the
/// case git itself spells them — sorted for determinism.
///
/// `changed_files` only ever tells us which *scope* is dirty, never which
/// file inside it changed (that narrowing is still to be built into the
/// snapshot layer); asking git directly is the only way to hand `state:
/// diff` the file that produced the dirty scope instead of an arbitrary
/// one that happens to sit in it. Returns `None` when the answer cannot
/// be trusted — no git, an unknown `since` — so the caller can fall back
/// to the scope listing instead of asserting a false "nothing changed".
fn diffed_files_in_scope(workspace_root: &str, since: &str, dir: &str) -> Option<Vec<String>> {
    let dir_arg = if dir.is_empty() { "." } else { dir };
    let out = Command::new("git")
        .args([
            "-C",
            workspace_root,
            "diff",
            "--name-only",
            since,
            "--",
            dir_arg,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    files.sort();
    Some(files)
}

pub fn resolve(req: &ResolveRequest) -> Result<ResolveResponse, Error> {
    let mut tasks = Vec::new();
    for check in &req.checks {
        let w = &check.with_payload;
        let ask = w.get("ask").and_then(Value::as_str).unwrap_or("?");
        let expect = w.get("expect").and_then(Value::as_str).unwrap_or("yes");
        let mode = w.get("mode").and_then(Value::as_str).unwrap_or("shadow");
        // Two argv entries, not one string. A shell-quoted `--flag 'value'` is a single
        // unrecognised argument once it reaches argv, and nothing catches that until the
        // task is actually run — which is exactly what the end-to-end run caught.
        let question_argv: Vec<String> = match w.get("question") {
            Some(inline) => vec!["--question-json".to_string(), inline.to_string()],
            None => vec![
                "--questions".to_string(),
                w.get("questions")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string(),
            ],
        };
        // The human-readable form keeps the quoting, because a person pastes it into a shell.
        let question = match w.get("question") {
            Some(inline) => format!("--question-json '{inline}'"),
            None => format!(
                "--questions {}",
                w.get("questions").and_then(Value::as_str).unwrap_or("?")
            ),
        };
        // `state: diff` shows the model what the change did rather than the file it was in.
        // All three are pass-through: the core declares the command and reads the evidence, it
        // does not know what a diff is.
        let mut narrowing = Vec::new();
        if let Some(s) = w.get("state").and_then(Value::as_str) {
            narrowing.push("--state".to_string());
            narrowing.push(s.to_string());
        }
        if let Some(n) = w.get("context_lines").and_then(Value::as_u64) {
            narrowing.push("--context-lines".to_string());
            narrowing.push(n.to_string());
        }
        if let Some(r) = w.get("changed_since").and_then(Value::as_str) {
            narrowing.push("--changed-since".to_string());
            narrowing.push(r.to_string());
        }
        let narrowing_text = if narrowing.is_empty() {
            String::new()
        } else {
            format!(" {}", narrowing.join(" "))
        };
        let state = w.get("state").and_then(Value::as_str).unwrap_or("file");
        let changed_since = w.get("changed_since").and_then(Value::as_str);
        let dir = scope_dir(&check.scope_path);

        // Which files this task should actually judge, and whether the
        // scope really has nothing changed in it (as opposed to us simply
        // failing to find out).
        //
        // Real diff, computed from git, wins whenever the check runs in
        // `state: diff` and gives us a rev to diff against: it is the only
        // source that knows which file inside the scope actually changed,
        // rather than which scope it sits in. An empty result here is
        // trustworthy — the scope truly has nothing to judge — which is
        // different from `diffed_files_in_scope` returning `None`, meaning
        // git could not answer at all.
        let git_answer = match (state, changed_since) {
            ("diff", Some(since)) => diffed_files_in_scope(&req.workspace_root, since, &dir),
            _ => None,
        };
        let scope_files = || files_in_scope(&req.changed_files, &dir);
        let (samples, scope_confirmed_empty): (Vec<String>, bool) = match git_answer {
            Some(diffed) if !diffed.is_empty() => (diffed, false),
            Some(_empty) => {
                // Confirmed: nothing under this scope changed since `since`. Judge any one
                // file from the scope anyway — its own diff is empty too, so `musts-jev`
                // reports the honest "nothing to judge" rather than us asserting it here
                // without ever invoking the check.
                let fallback: Vec<String> = scope_files()
                    .into_iter()
                    .take(1)
                    .map(String::from)
                    .collect();
                (fallback, true)
            }
            // git could not answer `state: diff` (no repo, unknown rev, ...), or this check
            // runs in `state: file`, which was never about a diff at all. Either way we have
            // no confirmed real-changed-file list to fan out over, so fall back to a single
            // representative from the check's own scope — never the whole workspace's, which
            // is at least never a file from a different check — exactly as before, minus the
            // cross-check contamination.
            None => {
                let fallback: Vec<String> = scope_files()
                    .into_iter()
                    .take(1)
                    .map(String::from)
                    .collect();
                (fallback, false)
            }
        };
        let samples: Vec<String> = if samples.is_empty() {
            vec!["<file>".to_string()]
        } else {
            samples
        };

        let file_count = samples.len();
        for (i, sample) in samples.iter().enumerate() {
            let id = if file_count == 1 {
                format!("jev-{}", slug(&check.id))
            } else {
                format!("jev-{}-{}", slug(&check.id), i + 1)
            };
            let title = if scope_confirmed_empty {
                format!("Ask jev `{ask}`: nothing changed in scope")
            } else {
                format!("Ask jev `{ask}` about {file_count} file(s)")
            };
            tasks.push(Task {
                id,
                extension: "jev".to_string(),
                title,
                satisfies: vec![check.id.clone()],
                parallelizable: true,
                command: Some({
                    let mut argv = vec!["musts-jev".to_string()];
                    argv.extend(question_argv.clone());
                    argv.extend([
                        "--ask".to_string(),
                        ask.to_string(),
                        "--expect".to_string(),
                        expect.to_string(),
                        "--mode".to_string(),
                        mode.to_string(),
                    ]);
                    argv.extend(narrowing.iter().cloned());
                    argv.push(sample.clone());
                    argv
                }),
                instructions: vec![
                    format!("Run once per file in scope: `musts-jev {question} --ask {ask} --expect {expect} --mode {mode}{narrowing_text} {sample}`"),
                    "Submit its output. The summary says how many files were judged and how many were not — a green with nothing judged proves nothing.".to_string(),
                    "UNSURE is green: this reports what fired, never what is verified.".to_string(),
                ],
                evidence_contract: EvidenceContract {
                    text: TextContract {
                        required: true,
                        description: Some("The run's summary line.".to_string()),
                    },
                    assets: vec![AssetContract {
                        kind: "log".to_string(),
                        required: true,
                        description: None,
                    }],
                },
            });
        }
    }
    Ok(ResolveResponse {
        protocol_version: 1,
        tasks,
        ignored_checks: vec![],
        notes: vec![
            "uses: jev records a measured verdict, not an attestation. Its green means \"nothing fired\", never \"verified\"."
                .to_string(),
        ],
    })
}

pub fn evidence(req: &EvidenceValidationRequest) -> Result<EvidenceValidationResponse, Error> {
    Ok(EvidenceValidationResponse {
        protocol_version: 1,
        accepted: true,
        satisfies: req.task.satisfies.clone(),
        summary: Some("jev verdicts recorded.".to_string()),
        normalized_assets: vec![],
        missing: vec![],
        message: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use musts_protocol::{ResolveCheck, SnapshotHandle};
    use std::path::Path;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn git(repo: &Path, args: &[&str]) {
        let out = StdCommand::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repo with two scopes: `app` (an unrelated file, alphabetically first)
    /// and `features` (the one whose file actually changes). Mirrors the
    /// field report: `boxy/app/AppDelegate.swift` sorted ahead of
    /// `boxy/features/CreateBoxFeature.swift`, and the leak was planted in
    /// the latter.
    fn repo_with_two_scopes() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.email", "t@example.com"]);
        git(root, &["config", "user.name", "Test"]);
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::create_dir_all(root.join("features")).unwrap();
        std::fs::write(
            root.join("app/AppDelegate.swift"),
            "// unrelated, unchanged\n",
        )
        .unwrap();
        std::fs::write(root.join("features/CreateBoxFeature.swift"), "let x = 1\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "initial"]);
        // Only the file in `features` changes after this point.
        std::fs::write(
            root.join("features/CreateBoxFeature.swift"),
            "let x = 1\ncategoryName: state.alias\n",
        )
        .unwrap();
        dir
    }

    fn check(id: &str, scope_path: &str, with_payload: Value) -> ResolveCheck {
        ResolveCheck {
            id: id.to_string(),
            local_id: id.rsplit('/').next().unwrap_or(id).to_string(),
            manifest_path: format!("{scope_path}/MUSTS.yml"),
            scope_path: scope_path.to_string(),
            depth: 1,
            with_payload,
        }
    }

    fn diff_payload() -> Value {
        json!({
            "ask": "leaks-category",
            "questions": "builtin:swift-analytics-privacy",
            "expect": "no",
            "state": "diff",
            "changed_since": "HEAD",
        })
    }

    fn request(
        workspace_root: &Path,
        changed_files: Vec<String>,
        checks: Vec<ResolveCheck>,
    ) -> ResolveRequest {
        ResolveRequest {
            protocol_version: 1,
            workspace_root: workspace_root.display().to_string(),
            capability: "jev".to_string(),
            changed_files,
            checks,
            snapshot: SnapshotHandle {
                handle: "v1:jev".to_string(),
                dirty_scopes: vec!["features".to_string()],
            },
        }
    }

    /// The bug: a task for the `features` check must judge the file that
    /// actually changed inside `features`, never a file from `app`'s scope
    /// just because it sorts first across the whole workspace.
    #[test]
    fn resolve_picks_the_changed_file_in_its_own_scope_not_the_first_across_the_workspace() {
        let dir = repo_with_two_scopes();
        // `changed_files` as musts-core actually builds it: the lowercased
        // union of every dirty check's scope, `app` sorting ahead of
        // `features`. This is what made `.first()` reach into a scope this
        // check has nothing to do with.
        let changed_files = vec![
            "app/appdelegate.swift".to_string(),
            "features/createboxfeature.swift".to_string(),
        ];
        let checks = vec![check("features/leaks-category", "features", diff_payload())];
        let req = request(dir.path(), changed_files, checks);

        let resp = resolve(&req).expect("resolve");
        assert_eq!(resp.tasks.len(), 1, "expected exactly one task");
        let argv = resp.tasks[0].command.as_ref().expect("command");
        let sample = argv.last().expect("sample file arg");

        assert_eq!(
            sample, "features/CreateBoxFeature.swift",
            "must judge the file that changed in its own scope, in its real case, \
             not the alphabetically-first file across the whole workspace"
        );
    }

    /// The control: a check whose own scope has nothing changed must still
    /// report green, and must not be confused with "we asked about the
    /// wrong file". `scope_confirmed_empty` distinguishes the two in the
    /// task title; `musts-jev` distinguishes them again at run time.
    #[test]
    fn resolve_stays_green_when_the_scope_has_no_real_changes() {
        let dir = repo_with_two_scopes();
        // Commit the pending change so `features` is clean relative to HEAD too.
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "second"]);
        // Nothing has changed in `app` since HEAD (this commit).
        let changed_files = vec!["app/appdelegate.swift".to_string()];
        let checks = vec![check("app/leaks-category", "app", diff_payload())];
        let req = request(dir.path(), changed_files, checks);

        let resp = resolve(&req).expect("resolve");
        assert_eq!(resp.tasks.len(), 1);
        assert!(
            resp.tasks[0].title.contains("nothing changed"),
            "an empty scope must say so, distinctly from a task that judges a file: {:?}",
            resp.tasks[0].title
        );
        let argv = resp.tasks[0].command.as_ref().expect("command");
        assert_eq!(
            argv.last().map(String::as_str),
            Some("app/appdelegate.swift")
        );
    }
}
