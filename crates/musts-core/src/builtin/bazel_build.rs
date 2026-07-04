//! Built-in `bazel/build` capability per `docs/PLAN.md` §6.0 / §6.1.
//!
//! Implements the deepest-applicable-target policy: every (scope,
//! target) pair gets a task, except when a *deeper* same-capability
//! scope exists in the same run — in that case the deeper scope's
//! tasks subsume every (target-agnostic) ancestor check. The ancestor's
//! check_ids are merged into each deeper task's `satisfies` so
//! recording evidence converges every subsumed check.
//!
//! Within a scope, each distinct `target` is its own task: dual-platform
//! repos (e.g. iOS + macOS at the root) need to build both. Two checks
//! pointing at the same target in the same scope are the only thing
//! that merges into a single task.
//!
//! `with` payload schema:
//!
//! ```json
//! { "target": "//path/to:target" }
//! ```
//!
//! Evidence contract: text + one or more log assets (`text/*` or
//! `application/octet-stream`).

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
            "required": ["target"],
            "additionalProperties": false,
            "properties": {
                "target": { "type": "string", "minLength": 1 }
            }
        })
    });
    &SCHEMA
}

#[derive(Debug, Deserialize)]
struct BuildWith {
    target: String,
}

struct Bucket {
    satisfies: Vec<String>,
}

pub fn resolve(request: &ResolveRequest) -> Result<ResolveResponse, Error> {
    // Bucket key is (scope_path, target): two checks pointing at the
    // same target inside the same scope merge into one task (the only
    // legitimate dedup), but distinct targets in the same scope each
    // get their own task. Keying by scope_path alone silently dropped
    // every target but one in dual-platform layouts (iOS + macOS
    // declared at the root of the same repo).
    let mut by_scope_target: BTreeMap<(String, String), Bucket> = BTreeMap::new();
    let mut malformed: Vec<String> = Vec::new();
    for check in &request.checks {
        let with: BuildWith = match serde_json::from_value(check.with_payload.clone()) {
            Ok(w) => w,
            Err(_) => {
                malformed.push(check.id.clone());
                continue;
            }
        };
        let entry = by_scope_target
            .entry((check.scope_path.clone(), with.target))
            .or_insert_with(|| Bucket {
                satisfies: Vec::new(),
            });
        entry.satisfies.push(check.id.clone());
    }

    // Partition every (scope, target) pair by whether some sibling
    // scope_path is deeper. The deepest-applicable-target policy
    // operates on scope_path, not on the bucket key — so the "is anyone
    // deeper than me" question is asked against the set of distinct
    // scope_paths, and a winner at scope S absorbs every shallower
    // scope's satisfies regardless of how many targets that shallower
    // scope declared.
    let scope_paths: BTreeSet<String> = by_scope_target.keys().map(|(s, _)| s.clone()).collect();
    let mut winners: Vec<(String, String)> = Vec::new();
    let mut ignored = Vec::new();
    for (key, bucket) in &by_scope_target {
        let (scope, _target) = key;
        if scope_paths.iter().any(|other| is_deeper(other, scope)) {
            for id in &bucket.satisfies {
                ignored.push(IgnoredCheck {
                    id: id.clone(),
                    reason: "subsumed by a deeper bazel/build target in the same run".into(),
                });
            }
        } else {
            winners.push(key.clone());
        }
    }

    // Count winners per scope so single-target scopes keep the stable
    // `bazel-build-<scope>` task id (no ledger churn for the common
    // case); multi-target scopes disambiguate with a target slug.
    let mut winners_per_scope: BTreeMap<String, usize> = BTreeMap::new();
    for (scope, _) in &winners {
        *winners_per_scope.entry(scope.clone()).or_insert(0) += 1;
    }

    let mut tasks = Vec::with_capacity(winners.len());
    for (scope, target) in winners {
        // Clone (don't drain) ancestor satisfies: every winning task must
        // carry every shallower scope's check_ids so the partial-accept
        // rule (PLAN.md §4.2) converges the ancestor as soon as evidence
        // is recorded for ANY winner that subsumes it. Draining would
        // attribute the ancestor's ids to only the first winner iterated.
        let mut merged_satisfies = by_scope_target[&(scope.clone(), target.clone())]
            .satisfies
            .clone();
        for ((other_scope, _), other_bucket) in &by_scope_target {
            if is_deeper(&scope, other_scope) {
                merged_satisfies.extend(other_bucket.satisfies.iter().cloned());
            }
        }
        let task_id = if winners_per_scope[&scope] > 1 {
            format!(
                "bazel-build-{}-{}",
                scope_slug(&scope),
                target_slug(&target)
            )
        } else {
            format!("bazel-build-{}", scope_slug(&scope))
        };
        tasks.push(Task {
            id: task_id,
            extension: "bazel/build".into(),
            title: format!("Build {target}"),
            satisfies: merged_satisfies,
            parallelizable: true,
            command: Some(vec!["bazel".into(), "build".into(), target.clone()]),
            instructions: vec![
                format!("Run `bazel build {target}`."),
                "Capture stdout/stderr as a log asset.".into(),
                "Record the result with `musts evidence <task-id> --text \"…\" --asset <log>`."
                    .into(),
            ],
            evidence_contract: EvidenceContract {
                text: TextContract {
                    required: true,
                    description: Some(
                        "State the command that was run and whether it succeeded.".into(),
                    ),
                },
                assets: vec![AssetContract {
                    kind: "log".into(),
                    required: true,
                    description: Some("Bazel stdout/stderr log.".into()),
                }],
            },
        });
    }
    for id in malformed {
        ignored.push(IgnoredCheck {
            id,
            reason: "with-payload does not match the bazel/build schema".into(),
        });
    }
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
            message: "Provide a text summary stating the build command and whether it succeeded."
                .into(),
        });
    }
    if log_assets.is_empty() {
        missing.push(MissingEvidence {
            kind: "log".into(),
            message: "Attach the bazel stdout/stderr as a log file (`text/*` or \
                 `application/octet-stream`)."
                .into(),
        });
    }
    if let Some(first_empty) = log_assets.iter().find(|a| a.size == 0) {
        missing.push(MissingEvidence {
            kind: "log".into(),
            message: format!(
                "Log asset `{}` is empty; record the real build output.",
                first_empty.path
            ),
        });
    }
    // Content check: a non-empty log that records a bazel failure must not
    // be accepted as green. Previously any non-empty log passed, so a log
    // that literally said the build failed (or "disk full … exit 50")
    // sailed through. Mirrors the cargo capability's log inspection.
    if missing.is_empty() {
        if let Some(problem) = inspect_build_log(&request.workspace_root, &log_assets) {
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
    let normalized_assets = log_assets
        .iter()
        .map(|a| NormalizedAsset {
            kind: "log".into(),
            path: a.path.clone(),
        })
        .collect();
    Ok(EvidenceValidationResponse {
        protocol_version: PROTOCOL_VERSION,
        accepted: true,
        satisfies: request.task.satisfies.clone(),
        summary: Some(format!(
            "Build evidence accepted ({} log asset{}).",
            log_assets.len(),
            if log_assets.len() == 1 { "" } else { "s" },
        )),
        normalized_assets,
        missing: Vec::new(),
        message: None,
    })
}

/// Read the first non-empty log asset and reject it if it records a bazel
/// build failure. Returns `Some(MissingEvidence)` when the log is
/// unreadable or shows a failure marker.
///
/// bazel prints `ERROR:` lines and `FAILED: Build did NOT complete
/// successfully` on failure (via bazel or bazelisk); a genuinely green
/// build never contains these. We reject on their presence rather than
/// requiring a positive success marker, because the exact success line
/// varies across bazel versions and wrapper scripts (`make build`, etc.).
fn inspect_build_log(
    workspace_root: &str,
    log_assets: &[&EvidenceAsset],
) -> Option<MissingEvidence> {
    let log = log_assets.iter().find(|a| a.size > 0)?;
    let abs_path = PathBuf::from(workspace_root).join(&log.path);
    match std::fs::read_to_string(&abs_path) {
        Ok(contents) => build_log_failure(&contents).map(|message| MissingEvidence {
            kind: "log".into(),
            message,
        }),
        Err(err) => Some(MissingEvidence {
            kind: "log".into(),
            message: format!("Could not read log asset `{}`: {err}", log.path),
        }),
    }
}

/// Failure heuristic for a bazel build log. Returns a rejection message
/// when the log records a failure, `None` when it looks clean.
fn build_log_failure(log: &str) -> Option<String> {
    if log.contains("FAILED: Build did NOT complete successfully") {
        return Some(
            "Log contains `FAILED: Build did NOT complete successfully` — the build failed. Fix \
             it and re-capture."
                .into(),
        );
    }
    if let Some(line) = log
        .lines()
        .find(|line| line.trim_start().starts_with("ERROR:"))
    {
        return Some(format!(
            "Log contains a bazel error ({}). Resolve it before evidence is accepted.",
            line.trim()
        ));
    }
    None
}

/// Turn a bazel target label into a slug safe to embed in a task id.
///
/// `//App:NokoruiOS` → `app-nokoruios`. Strips the `//` prefix and
/// folds `/` and `:` into `-`; lowercase so the slug is stable across
/// case-insensitive filesystems (same rationale as `scope_slug`).
fn target_slug(target: &str) -> String {
    target
        .trim_start_matches("//")
        .replace(['/', ':'], "-")
        .to_lowercase()
}

fn is_deeper(candidate: &str, scope: &str) -> bool {
    if candidate == scope {
        return false;
    }
    if scope.is_empty() || scope == "root" {
        return !candidate.is_empty() && candidate != "root";
    }
    candidate.starts_with(scope) && candidate.as_bytes().get(scope.len()) == Some(&b'/')
}

#[cfg(test)]
mod tests {
    use super::*;
    use musts_protocol::{EvidenceSubmission, EvidenceTaskRef, ResolveCheck, SnapshotHandle};

    fn req(checks: Vec<ResolveCheck>) -> ResolveRequest {
        ResolveRequest {
            protocol_version: PROTOCOL_VERSION,
            workspace_root: "/repo".into(),
            capability: "bazel/build".into(),
            changed_files: Vec::new(),
            checks,
            snapshot: SnapshotHandle {
                handle: "h".into(),
                dirty_scopes: Vec::new(),
            },
        }
    }

    fn check(scope: &str, local: &str, target: &str, depth: u32) -> ResolveCheck {
        ResolveCheck {
            id: if scope.is_empty() || scope == "root" {
                format!("root/{local}")
            } else {
                format!("{scope}/{local}")
            },
            local_id: local.into(),
            manifest_path: "MUSTS.yml".into(),
            scope_path: if scope.is_empty() {
                "root".into()
            } else {
                scope.into()
            },
            depth,
            with_payload: serde_json::json!({ "target": target }),
        }
    }

    #[test]
    fn deepest_target_subsumes_ancestor_check_into_one_task() {
        let response = resolve(&req(vec![
            check("root", "app-build", "//App:App", 0),
            check("App/Login", "login-build", "//App/Login:Login", 2),
        ]))
        .unwrap();
        assert_eq!(response.tasks.len(), 1);
        assert!(response.tasks[0].title.contains("//App/Login:Login"));
        assert!(response.tasks[0]
            .satisfies
            .contains(&"App/Login/login-build".to_string()));
        assert!(response.tasks[0]
            .satisfies
            .contains(&"root/app-build".to_string()));
        assert_eq!(response.ignored_checks.len(), 1);
        assert_eq!(response.ignored_checks[0].id, "root/app-build");
    }

    #[test]
    fn shared_ancestor_subsumes_into_every_winner() {
        // Two sibling winners (`App/Login`, `App/Profile`) share an
        // ancestor (`root`). Per PLAN.md §4.2 partial-accept, the
        // ancestor's check id must appear in EVERY winner's `satisfies`
        // so recording evidence for either deep target converges the
        // ancestor — not just the first winner iterated.
        let response = resolve(&req(vec![
            check("root", "app-build", "//App:App", 0),
            check("App/Login", "login-build", "//App/Login:Login", 2),
            check("App/Profile", "profile-build", "//App/Profile:Profile", 2),
        ]))
        .unwrap();
        assert_eq!(response.tasks.len(), 2);
        for task in &response.tasks {
            assert!(
                task.satisfies.contains(&"root/app-build".to_string()),
                "task `{}` should carry root/app-build in satisfies, got {:?}",
                task.id,
                task.satisfies
            );
        }
        assert_eq!(response.ignored_checks.len(), 1);
        assert_eq!(response.ignored_checks[0].id, "root/app-build");
    }

    #[test]
    fn only_root_present_emits_root_task() {
        let response = resolve(&req(vec![check("root", "app-build", "//App:App", 0)])).unwrap();
        assert_eq!(response.tasks.len(), 1);
        assert!(response.ignored_checks.is_empty());
    }

    #[test]
    fn same_scope_distinct_targets_each_get_a_task() {
        // Dual-platform repo: iOS and macOS both declared at root. The
        // resolver used to collapse them into a single task — submitting
        // evidence for one build silently marked the other green.
        let response = resolve(&req(vec![
            check("root", "build-ios", "//App:NokoruiOS", 0),
            check("root", "build-macos", "//App:NokoruMac", 0),
        ]))
        .unwrap();
        assert_eq!(
            response.tasks.len(),
            2,
            "expected one task per distinct target, got {:?}",
            response.tasks.iter().map(|t| &t.id).collect::<Vec<_>>()
        );
        let titles: Vec<_> = response.tasks.iter().map(|t| t.title.as_str()).collect();
        assert!(titles.iter().any(|t| t.contains("//App:NokoruiOS")));
        assert!(titles.iter().any(|t| t.contains("//App:NokoruMac")));
        // Each task carries exactly its own check id — no cross-pollution.
        for task in &response.tasks {
            assert_eq!(task.satisfies.len(), 1);
        }
        // Multi-target scope must disambiguate task ids by target slug.
        let ids: Vec<_> = response.tasks.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"bazel-build-root-app-nokoruios"));
        assert!(ids.contains(&"bazel-build-root-app-nokorumac"));
        assert!(response.ignored_checks.is_empty());
    }

    #[test]
    fn same_scope_same_target_still_merges() {
        // The legitimate dedup case: two checks happen to point at the
        // same target. One task is correct.
        let response = resolve(&req(vec![
            check("root", "build-primary", "//App:App", 0),
            check("root", "build-alias", "//App:App", 0),
        ]))
        .unwrap();
        assert_eq!(response.tasks.len(), 1);
        assert_eq!(response.tasks[0].id, "bazel-build-root");
        assert!(response.tasks[0]
            .satisfies
            .contains(&"root/build-primary".to_string()));
        assert!(response.tasks[0]
            .satisfies
            .contains(&"root/build-alias".to_string()));
    }

    #[test]
    fn deeper_scope_subsumes_every_root_target() {
        // Multi-target ancestor + deeper scope: both root targets are
        // subsumed by the deeper scope (deepest-applicable-target wins),
        // and the deeper task's `satisfies` carries every root check id.
        let response = resolve(&req(vec![
            check("root", "build-ios", "//App:iOS", 0),
            check("root", "build-mac", "//App:Mac", 0),
            check("App/Login", "login-build", "//App/Login:Login", 2),
        ]))
        .unwrap();
        assert_eq!(response.tasks.len(), 1);
        assert!(response.tasks[0].title.contains("//App/Login:Login"));
        for ancestor in ["root/build-ios", "root/build-mac"] {
            assert!(
                response.tasks[0].satisfies.contains(&ancestor.to_string()),
                "deeper task must absorb {ancestor}, got {:?}",
                response.tasks[0].satisfies
            );
        }
        let ignored_ids: Vec<_> = response
            .ignored_checks
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert!(ignored_ids.contains(&"root/build-ios"));
        assert!(ignored_ids.contains(&"root/build-mac"));
    }

    #[test]
    fn sibling_scopes_each_get_a_task() {
        let response = resolve(&req(vec![
            check("App/Login", "login-build", "//App/Login:Login", 2),
            check("App/Profile", "profile-build", "//App/Profile:Profile", 2),
        ]))
        .unwrap();
        assert_eq!(response.tasks.len(), 2);
        assert!(response.ignored_checks.is_empty());
    }

    #[test]
    fn malformed_with_payload_is_reported_as_ignored() {
        let mut c = check("root", "x", "//", 0);
        c.with_payload = serde_json::json!({ "not_target": 1 });
        let response = resolve(&req(vec![c])).unwrap();
        assert!(response.tasks.is_empty());
        assert_eq!(response.ignored_checks.len(), 1);
        assert!(response.ignored_checks[0].reason.contains("does not match"));
    }

    fn evidence_req_at(
        text: Option<&str>,
        assets: Vec<EvidenceAsset>,
        workspace_root: &str,
    ) -> EvidenceValidationRequest {
        EvidenceValidationRequest {
            protocol_version: PROTOCOL_VERSION,
            workspace_root: workspace_root.into(),
            task: EvidenceTaskRef {
                id: "bazel-build-root".into(),
                extension: "bazel/build".into(),
                satisfies: vec!["root/app-build".into()],
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
                text: text.map(|s| s.to_string()),
                assets,
            },
            snapshot: SnapshotHandle {
                handle: "h".into(),
                dirty_scopes: Vec::new(),
            },
        }
    }

    fn asset(path: &str, mime: &str, size: u64) -> EvidenceAsset {
        EvidenceAsset {
            path: path.into(),
            mime: mime.into(),
            size,
        }
    }

    /// Write a log file into a fresh tempdir and return the dir plus the
    /// workspace-relative path the evidence request should carry.
    fn write_log(contents: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("build.log");
        std::fs::write(&path, contents).expect("write log");
        (dir, "build.log".to_string())
    }

    #[test]
    fn evidence_accepts_text_plus_clean_log() {
        let (dir, rel) = write_log("INFO: Build completed successfully, 12 total actions\n");
        let resp = evidence(&evidence_req_at(
            Some("bazel build //App:App succeeded"),
            vec![asset(&rel, "text/plain", 48)],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(resp.accepted, "expected accept, got {resp:?}");
        assert_eq!(resp.satisfies, vec!["root/app-build"]);
    }

    #[test]
    fn evidence_accepts_octet_stream_log() {
        let (dir, rel) = write_log("Target //App:App up-to-date\n");
        let resp = evidence(&evidence_req_at(
            Some("ok"),
            vec![asset(&rel, "application/octet-stream", 28)],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(resp.accepted);
    }

    #[test]
    fn evidence_rejects_failed_build_log() {
        let (dir, rel) = write_log(
            "ERROR: /repo/App/BUILD:3:11: Compiling failed\nFAILED: Build did NOT complete \
             successfully\n",
        );
        let resp = evidence(&evidence_req_at(
            Some("bazel build ran"),
            vec![asset(&rel, "text/plain", 96)],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(!resp.accepted, "a failed build log must not be green");
        assert!(resp
            .missing
            .iter()
            .any(|m| m.message.contains("did NOT complete") || m.message.contains("bazel error")));
    }

    #[test]
    fn evidence_rejects_error_line_only() {
        let (dir, rel) = write_log("ERROR: no such target '//App:Ghost'\n");
        let resp = evidence(&evidence_req_at(
            Some("tried to build"),
            vec![asset(&rel, "text/plain", 40)],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp
            .missing
            .iter()
            .any(|m| m.message.contains("bazel error")));
    }

    #[test]
    fn evidence_rejects_empty_text() {
        let (dir, rel) = write_log("INFO: Build completed successfully\n");
        let resp = evidence(&evidence_req_at(
            Some(""),
            vec![asset(&rel, "text/plain", 36)],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp.missing.iter().any(|m| m.kind == "text"));
    }

    #[test]
    fn evidence_rejects_missing_log() {
        let resp = evidence(&evidence_req_at(Some("ok"), vec![], "/repo")).unwrap();
        assert!(!resp.accepted);
        assert!(resp.missing.iter().any(|m| m.kind == "log"));
    }

    #[test]
    fn evidence_rejects_zero_byte_log() {
        let resp = evidence(&evidence_req_at(
            Some("ok"),
            vec![asset("a.log", "text/plain", 0)],
            "/repo",
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp.missing.iter().any(|m| m.kind == "log"));
    }

    #[test]
    fn evidence_rejects_unreadable_log() {
        let resp = evidence(&evidence_req_at(
            Some("ok"),
            vec![asset("does/not/exist.log", "text/plain", 10)],
            "/repo",
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp
            .missing
            .iter()
            .any(|m| m.message.contains("Could not read log")));
    }

    #[test]
    fn target_slug_strips_prefix_and_normalises_separators() {
        assert_eq!(target_slug("//App:NokoruiOS"), "app-nokoruios");
        assert_eq!(target_slug("//path/to:target"), "path-to-target");
        assert_eq!(target_slug("//App/Login:Login"), "app-login-login");
    }

    #[test]
    fn is_deeper_segment_aware() {
        assert!(is_deeper("App/Login", "root"));
        assert!(is_deeper("App/Login", "App"));
        assert!(!is_deeper("App/LoginExtra", "App/Login"));
        assert!(!is_deeper("App/Login", "App/Login"));
    }
}
