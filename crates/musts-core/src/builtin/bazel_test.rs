//! Built-in `bazel/test` capability.
//!
//! `docs/musts-design.md` §8.2 has specified this schema since the design
//! was written, but nothing implemented it. The consequence in the wild:
//! five of seven iOS repos hand-rolled an identical
//! `.musts/extensions/bazel/` shim (`extension.yml` + `bazel-test.sh`) to
//! get it back. That shim is the de facto spec this follows.
//!
//! `with` payload schema (per §8.2):
//!
//! ```json
//! { "targets": ["//App/Tests:UnitTests", "//App/Tests:SnapshotTests"] }
//! ```
//!
//! Grouping policy, per §"Extensions Resolve Checks Into Tasks" —
//! "`bazel/test` may group multiple targets into one test command":
//! every target declared by the checks in one scope runs as a single
//! `bazel test` invocation. This differs from `bazel/build`, which gives
//! each target its own task, and the reason is that bazel's own test
//! runner parallelises across targets far better than separate
//! invocations do, and one log covers the lot.
//!
//! Scopes stay separate. Unlike `bazel/build` there is no
//! deepest-scope subsumption: a deeper module's tests do not run its
//! parent's tests, so collapsing them would mark checks green that
//! nothing executed.
//!
//! Evidence contract: text + one or more log assets.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::LazyLock;

use musts_protocol::{
    AssetContract, EvidenceAsset, EvidenceContract, EvidenceValidationRequest,
    EvidenceValidationResponse, IgnoredCheck, MissingEvidence, NormalizedAsset, ResolveRequest,
    ResolveResponse, Task, TextContract, PROTOCOL_VERSION,
};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use super::util::{is_log_or_text, scope_slug};
use crate::error::Error;

pub fn schema() -> &'static JsonValue {
    static SCHEMA: LazyLock<JsonValue> = LazyLock::new(|| {
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "required": ["targets"],
            "additionalProperties": false,
            "properties": {
                "targets": {
                    "type": "array",
                    "items": { "type": "string", "minLength": 1 },
                    "minItems": 1
                }
            }
        })
    });
    &SCHEMA
}

#[derive(Debug, Deserialize)]
struct TestWith {
    targets: Vec<String>,
}

#[derive(Default)]
struct Bucket {
    /// Sorted and deduped, so the command — and therefore the task id and
    /// the evidence it satisfies — does not depend on manifest ordering.
    targets: BTreeSet<String>,
    satisfies: Vec<String>,
}

pub fn resolve(request: &ResolveRequest) -> Result<ResolveResponse, Error> {
    let mut by_scope: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut ignored: Vec<IgnoredCheck> = Vec::new();

    for check in &request.checks {
        let with: TestWith = match serde_json::from_value(check.with_payload.clone()) {
            Ok(w) => w,
            Err(_) => {
                ignored.push(IgnoredCheck {
                    id: check.id.clone(),
                    reason: "with-payload does not match the bazel/test schema".into(),
                });
                continue;
            }
        };
        // An empty `targets` passes JSON-Schema `minItems` only if the
        // schema was bypassed, but a resolve must never emit a command
        // with no targets — `bazel test` with none is a no-op that would
        // record a green log proving nothing.
        if with.targets.iter().all(|t| t.trim().is_empty()) {
            ignored.push(IgnoredCheck {
                id: check.id.clone(),
                reason: "no non-empty bazel/test targets declared".into(),
            });
            continue;
        }
        let entry = by_scope.entry(check.scope_path.clone()).or_default();
        entry
            .targets
            .extend(with.targets.into_iter().filter(|t| !t.trim().is_empty()));
        entry.satisfies.push(check.id.clone());
    }

    let tasks = by_scope
        .into_iter()
        .map(|(scope, bucket)| {
            let targets: Vec<String> = bucket.targets.into_iter().collect();
            let mut argv = vec!["bazel".to_string(), "test".to_string()];
            argv.extend(targets.iter().cloned());
            argv.push("--test_output=errors".into());
            let display = argv.join(" ");
            Task {
                id: format!("bazel-test-{}", scope_slug(&scope)),
                extension: "bazel/test".into(),
                title: format!(
                    "Test {} target{}",
                    targets.len(),
                    if targets.len() == 1 { "" } else { "s" }
                ),
                satisfies: bucket.satisfies,
                parallelizable: true,
                command: Some(argv),
                instructions: vec![
                    format!("Run `{display}`."),
                    "Capture stdout/stderr as a log asset.".into(),
                    "Record the result with `musts evidence <task-id> --text \"…\" --asset <log>`."
                        .into(),
                ],
                evidence_contract: EvidenceContract {
                    text: TextContract {
                        required: true,
                        description: Some(
                            "State the command that was run and whether every test passed.".into(),
                        ),
                    },
                    assets: vec![AssetContract {
                        kind: "log".into(),
                        required: true,
                        description: Some("bazel test stdout/stderr log.".into()),
                    }],
                },
            }
        })
        .collect();

    Ok(ResolveResponse {
        protocol_version: PROTOCOL_VERSION,
        tasks,
        ignored_checks: ignored,
        notes: Vec::new(),
    })
}

pub fn evidence(request: &EvidenceValidationRequest) -> Result<EvidenceValidationResponse, Error> {
    let text = request.submission.text.as_deref().unwrap_or("");
    let log_assets: Vec<&EvidenceAsset> = request
        .submission
        .assets
        .iter()
        .filter(|a| is_log_or_text(a))
        .collect();

    let mut missing = Vec::new();
    if text.trim().is_empty() {
        missing.push(MissingEvidence {
            kind: "text".into(),
            message: "Provide a text summary stating the test command and whether every test \
                      passed."
                .into(),
        });
    }
    if log_assets.is_empty() {
        missing.push(MissingEvidence {
            kind: "log".into(),
            message: "Attach the bazel test stdout/stderr as a log file (`text/*` or \
                      `application/octet-stream`)."
                .into(),
        });
    }
    if let Some(empty) = log_assets.iter().find(|a| a.size == 0) {
        missing.push(MissingEvidence {
            kind: "log".into(),
            message: format!(
                "Log asset `{}` is empty; record the real test output.",
                empty.path
            ),
        });
    }
    if missing.is_empty() {
        if let Some(problem) = inspect_test_log(&request.workspace_root, &log_assets) {
            missing.push(problem);
        }
    }

    if !missing.is_empty() {
        return Ok(EvidenceValidationResponse {
            protocol_version: PROTOCOL_VERSION,
            accepted: false,
            satisfies: Vec::new(),
            summary: None,
            normalized_assets: Vec::new(),
            missing,
            message: Some("Evidence is incomplete.".into()),
        });
    }

    Ok(EvidenceValidationResponse {
        protocol_version: PROTOCOL_VERSION,
        accepted: true,
        satisfies: request.task.satisfies.clone(),
        summary: Some(format!(
            "bazel/test evidence accepted ({} log asset{}).",
            log_assets.len(),
            if log_assets.len() == 1 { "" } else { "s" }
        )),
        normalized_assets: log_assets
            .iter()
            .map(|a| NormalizedAsset {
                kind: "log".into(),
                path: a.path.clone(),
            })
            .collect(),
        missing: Vec::new(),
        message: None,
    })
}

fn inspect_test_log(
    workspace_root: &str,
    log_assets: &[&EvidenceAsset],
) -> Option<MissingEvidence> {
    let log = log_assets.iter().find(|a| a.size > 0)?;
    let abs = PathBuf::from(workspace_root).join(&log.path);
    match std::fs::read_to_string(&abs) {
        Ok(contents) => test_log_failure(&contents).map(|message| MissingEvidence {
            kind: "log".into(),
            message,
        }),
        Err(err) => Some(MissingEvidence {
            kind: "log".into(),
            message: format!("Could not read log asset `{}`: {err}", log.path),
        }),
    }
}

/// Failure heuristic for a `bazel test` log.
///
/// Three distinct ways a test run fails, and a green run emits none of
/// them. Keyed on these rather than on bare `ERROR:` lines, which
/// wrappers and subtools emit on successful runs too, or on a positive
/// success marker, whose wording moves between bazel versions.
fn test_log_failure(log: &str) -> Option<String> {
    const MARKERS: &[(&str, &str)] = &[
        (
            "Build did NOT complete successfully",
            "the build under test failed",
        ),
        ("FAILED TO BUILD", "a test target failed to build"),
        ("Executed 0 out of 0 tests", "no tests ran at all"),
    ];
    for (marker, why) in MARKERS {
        if log.contains(marker) {
            return Some(format!(
                "Log contains `{marker}` — {why}. Fix it and re-capture."
            ));
        }
    }
    // Per-target status lines: bazel prints `//target   FAILED in 1.2s`
    // (and `TIMEOUT`, `FLAKY`) in its summary. A green run only ever
    // prints `PASSED`, `NO STATUS`, or `CACHED`.
    for line in log.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("//") {
            continue;
        }
        for status in ["FAILED", "TIMEOUT"] {
            if trimmed.contains(status) {
                return Some(format!(
                    "Log records a `{status}` test target ({}). Fix it and re-capture.",
                    trimmed.split_whitespace().next().unwrap_or(trimmed)
                ));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use musts_protocol::{EvidenceSubmission, EvidenceTaskRef, ResolveCheck, SnapshotHandle};

    fn req(checks: Vec<ResolveCheck>) -> ResolveRequest {
        ResolveRequest {
            protocol_version: PROTOCOL_VERSION,
            workspace_root: "/repo".into(),
            capability: "bazel/test".into(),
            changed_files: Vec::new(),
            checks,
            snapshot: SnapshotHandle {
                handle: "h".into(),
                dirty_scopes: Vec::new(),
            },
        }
    }

    fn check(scope: &str, local: &str, targets: &[&str]) -> ResolveCheck {
        ResolveCheck {
            id: if scope == "root" {
                format!("root/{local}")
            } else {
                format!("{scope}/{local}")
            },
            local_id: local.into(),
            manifest_path: "MUSTS.yml".into(),
            scope_path: scope.into(),
            depth: 0,
            with_payload: serde_json::json!({ "targets": targets }),
        }
    }

    #[test]
    fn targets_in_one_scope_group_into_a_single_command() {
        let r = resolve(&req(vec![
            check("root", "unit", &["//App/Tests:Unit"]),
            check("root", "snapshot", &["//App/Tests:Snapshot"]),
        ]))
        .unwrap();
        assert_eq!(r.tasks.len(), 1, "one scope is one test invocation");
        let argv = r.tasks[0].command.as_ref().unwrap();
        assert_eq!(argv[0], "bazel");
        assert_eq!(argv[1], "test");
        assert!(argv.contains(&"//App/Tests:Unit".to_string()));
        assert!(argv.contains(&"//App/Tests:Snapshot".to_string()));
        assert!(argv.contains(&"--test_output=errors".to_string()));
        // Both checks converge on the one run that covered them.
        assert_eq!(r.tasks[0].satisfies.len(), 2);
    }

    #[test]
    fn target_order_does_not_depend_on_manifest_order() {
        let a = resolve(&req(vec![check("root", "c", &["//b:b", "//a:a"])])).unwrap();
        let b = resolve(&req(vec![check("root", "c", &["//a:a", "//b:b"])])).unwrap();
        assert_eq!(a.tasks[0].command, b.tasks[0].command);
    }

    #[test]
    fn duplicate_targets_across_checks_run_once() {
        let r = resolve(&req(vec![
            check("root", "one", &["//App:Tests"]),
            check("root", "two", &["//App:Tests"]),
        ]))
        .unwrap();
        let argv = r.tasks[0].command.as_ref().unwrap();
        assert_eq!(
            argv.iter().filter(|a| *a == "//App:Tests").count(),
            1,
            "a target listed twice must not be passed twice: {argv:?}"
        );
        assert_eq!(r.tasks[0].satisfies.len(), 2);
    }

    /// Unlike `bazel/build`, a deeper scope must NOT subsume its parent:
    /// running a module's tests does not run the parent's tests, so
    /// collapsing them would mark a check green that nothing executed.
    #[test]
    fn a_deeper_scope_does_not_subsume_its_parent() {
        let r = resolve(&req(vec![
            check("root", "all", &["//App:AllTests"]),
            check("App/Login", "login", &["//App/Login:Tests"]),
        ]))
        .unwrap();
        assert_eq!(r.tasks.len(), 2, "each scope tests itself");
        assert!(
            r.ignored_checks.is_empty(),
            "nothing may be silently subsumed: {:?}",
            r.ignored_checks
        );
    }

    #[test]
    fn malformed_payload_is_ignored_not_run() {
        let mut c = check("root", "bad", &["//x:x"]);
        c.with_payload = serde_json::json!({ "target": "//x:x" });
        let r = resolve(&req(vec![c])).unwrap();
        assert!(r.tasks.is_empty());
        assert_eq!(r.ignored_checks.len(), 1);
        assert!(r.ignored_checks[0].reason.contains("does not match"));
    }

    #[test]
    fn an_all_empty_target_list_never_produces_a_command() {
        // `bazel test` with no targets exits 0 and tests nothing, which
        // would record a green log proving nothing.
        let r = resolve(&req(vec![check("root", "bad", &["", "  "])])).unwrap();
        assert!(r.tasks.is_empty(), "{:?}", r.tasks);
        assert_eq!(r.ignored_checks.len(), 1);
        assert!(r.ignored_checks[0].reason.contains("no non-empty"));
    }

    fn ev(text: Option<&str>, assets: Vec<EvidenceAsset>, root: &str) -> EvidenceValidationRequest {
        EvidenceValidationRequest {
            protocol_version: PROTOCOL_VERSION,
            workspace_root: root.into(),
            task: EvidenceTaskRef {
                id: "bazel-test-root".into(),
                extension: "bazel/test".into(),
                satisfies: vec!["root/unit".into()],
                evidence_contract: EvidenceContract {
                    text: TextContract {
                        required: true,
                        description: None,
                    },
                    assets: vec![AssetContract {
                        kind: "log".into(),
                        required: true,
                        description: None,
                    }],
                },
            },
            submission: EvidenceSubmission {
                text: text.map(str::to_string),
                assets,
            },
            snapshot: SnapshotHandle {
                handle: "h".into(),
                dirty_scopes: Vec::new(),
            },
        }
    }

    fn with_log(contents: &str) -> (tempfile::TempDir, EvidenceAsset) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.log"), contents).unwrap();
        (
            dir,
            EvidenceAsset {
                path: "test.log".into(),
                mime: "text/plain".into(),
                size: contents.len() as u64,
            },
        )
    }

    #[test]
    fn accepts_a_passing_run() {
        let (dir, asset) = with_log(
            "//App/Tests:Unit    PASSED in 4.2s\nExecuted 1 out of 1 test: 1 test passes.\n",
        );
        let r = evidence(&ev(
            Some("all tests passed"),
            vec![asset],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(r.accepted, "{r:?}");
        assert_eq!(r.satisfies, vec!["root/unit"]);
    }

    #[test]
    fn rejects_a_failing_target() {
        let (dir, asset) =
            with_log("//App/Tests:Unit    PASSED in 4.2s\n//App/Tests:Snapshot   FAILED in 2.1s\n");
        let r = evidence(&ev(
            Some("ran the tests"),
            vec![asset],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(!r.accepted, "a FAILED target must not be green");
        assert!(r.missing.iter().any(|m| m.message.contains("FAILED")));
    }

    #[test]
    fn rejects_a_timeout() {
        let (dir, asset) = with_log("//App/Tests:Slow   TIMEOUT in 900.0s\n");
        let r = evidence(&ev(Some("ran"), vec![asset], dir.path().to_str().unwrap())).unwrap();
        assert!(!r.accepted);
        assert!(r.missing.iter().any(|m| m.message.contains("TIMEOUT")));
    }

    #[test]
    fn rejects_a_run_where_nothing_executed() {
        let (dir, asset) = with_log("Executed 0 out of 0 tests: 0 tests pass.\n");
        let r = evidence(&ev(
            Some("looks green to me"),
            vec![asset],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(!r.accepted, "an empty run proves nothing");
        assert!(r.missing.iter().any(|m| m.message.contains("no tests ran")));
    }

    #[test]
    fn rejects_a_build_failure_under_test() {
        let (dir, asset) = with_log("ERROR: something\nBuild did NOT complete successfully\n");
        let r = evidence(&ev(Some("ran"), vec![asset], dir.path().to_str().unwrap())).unwrap();
        assert!(!r.accepted);
    }

    /// The same care `bazel/build` takes: a green run whose log happens
    /// to mention "FAILED" outside a target status line is still green.
    #[test]
    fn accepts_a_green_run_that_merely_mentions_failure() {
        let (dir, asset) = with_log(
            "INFO: retry policy: FAILED runs are retried once\n//App/Tests:Unit   PASSED in \
             1.0s\nExecuted 1 out of 1 test: 1 test passes.\n",
        );
        let r = evidence(&ev(
            Some("green"),
            vec![asset],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(
            r.accepted,
            "the word FAILED outside a `//target` status line is not a failure: {r:?}"
        );
    }

    #[test]
    fn rejects_missing_text_and_missing_log() {
        let r = evidence(&ev(Some(""), vec![], "/repo")).unwrap();
        assert!(!r.accepted);
        assert!(r.missing.iter().any(|m| m.kind == "text"));
        assert!(r.missing.iter().any(|m| m.kind == "log"));
    }

    #[test]
    fn rejects_an_empty_log() {
        let r = evidence(&ev(
            Some("ok"),
            vec![EvidenceAsset {
                path: "t.log".into(),
                mime: "text/plain".into(),
                size: 0,
            }],
            "/repo",
        ))
        .unwrap();
        assert!(!r.accepted);
    }

    #[test]
    fn schema_matches_the_design_doc() {
        let s = schema();
        assert_eq!(s["required"], serde_json::json!(["targets"]));
        assert_eq!(s["properties"]["targets"]["type"], "array");
        assert_eq!(s["properties"]["targets"]["minItems"], 1);
        assert_eq!(s["additionalProperties"], false);
    }
}
