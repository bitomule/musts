//! Reference `bazel/build` extension.
//!
//! Implements the deepest-applicable-target policy from `docs/PLAN.md`
//! §6.1 and the evidence contract (text + at least one log asset).
//!
//! `with` payload schema (loaded from `schemas/build.schema.json`):
//!
//! ```json
//! { "target": "//path/to:target" }
//! ```

use std::collections::BTreeMap;
use std::process::ExitCode;

use harness_extension_util::{asset_kind, ipc_main};
use harness_protocol::{
    AssetContract, EvidenceContract, EvidenceValidationRequest, EvidenceValidationResponse,
    IgnoredCheck, MissingEvidence, ResolveRequest, ResolveResponse, Task, TextContract,
    PROTOCOL_VERSION,
};
use serde::Deserialize;

fn main() -> ExitCode {
    ipc_main(resolve, evidence)
}

// ---------------------------------------------------------------------------
// Resolve
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct BuildWith {
    target: String,
}

fn resolve(request: ResolveRequest) -> Result<ResolveResponse, String> {
    // Group checks by scope_path so we can pick the deepest one per
    // scope. Within the same scope, all checks must agree on the
    // target (we treat duplicates as redundant).
    let mut by_scope: BTreeMap<String, (String, Vec<String>, u32)> = BTreeMap::new();
    let mut malformed: Vec<String> = Vec::new();
    for check in &request.checks {
        let with: BuildWith = match serde_json::from_value(check.with_payload.clone()) {
            Ok(w) => w,
            Err(_) => {
                malformed.push(check.id.clone());
                continue;
            }
        };
        let entry = by_scope
            .entry(check.scope_path.clone())
            .or_insert_with(|| (with.target.clone(), Vec::new(), check.depth));
        entry.1.push(check.id.clone());
        // Pick lexicographically smaller target on collision so behaviour
        // is deterministic — duplicate same-scope same-capability checks
        // are unusual and we don't want to silently prefer one over
        // another based on iteration order.
        if with.target < entry.0 {
            entry.0 = with.target;
        }
    }

    // Now apply deepest-target policy: for every scope, ignore it if a
    // strictly deeper scope is also present (handled by the same
    // resolve cycle).
    let scopes: Vec<String> = by_scope.keys().cloned().collect();
    let mut ignored = Vec::new();
    let mut tasks = Vec::new();
    for (scope, (target, satisfies, depth)) in &by_scope {
        let has_deeper = scopes.iter().any(|other| is_deeper(other, scope));
        if has_deeper {
            for id in satisfies {
                ignored.push(IgnoredCheck {
                    id: id.clone(),
                    reason: "a deeper bazel/build target covers the changed scope".into(),
                });
            }
            continue;
        }
        let task_id = format!("bazel-build-{}", task_slug(scope));
        let title = format!("Build {}", target);
        tasks.push(Task {
            id: task_id,
            extension: "bazel/build".into(),
            title,
            satisfies: satisfies.clone(),
            parallelizable: true,
            instructions: vec![
                format!("Run `bazel build {target}`."),
                "Capture stdout/stderr as a log asset.".into(),
                "Record the result with `harness evidence <task-id> --text \"…\" --asset <log>`.".into(),
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
        let _ = depth;
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

fn is_deeper(candidate: &str, scope: &str) -> bool {
    if candidate == scope {
        return false;
    }
    if scope.is_empty() || scope == "root" {
        return !candidate.is_empty() && candidate != "root";
    }
    candidate.starts_with(scope) && candidate.as_bytes().get(scope.len()) == Some(&b'/')
}

fn task_slug(scope: &str) -> String {
    if scope.is_empty() || scope == "root" {
        return "root".into();
    }
    scope.replace('/', "-").to_lowercase()
}

// ---------------------------------------------------------------------------
// Evidence
// ---------------------------------------------------------------------------

fn evidence(
    request: EvidenceValidationRequest,
) -> Result<EvidenceValidationResponse, String> {
    let text = request.submission.text.as_deref().unwrap_or("");
    let log_assets: Vec<&_> = request
        .submission
        .assets
        .iter()
        .filter(|a| asset_kind::is_log_or_text(a))
        .collect();
    let mut missing = Vec::new();
    if text.trim().is_empty() {
        missing.push(MissingEvidence {
            kind: "text".into(),
            message: "Provide a text summary stating the build command and whether it succeeded.".into(),
        });
    }
    if log_assets.is_empty() {
        missing.push(MissingEvidence {
            kind: "log".into(),
            message:
                "Attach the bazel stdout/stderr as a log file (`text/*` or `application/octet-stream`)."
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
        .map(|a| harness_protocol::NormalizedAsset {
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

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::{ResolveCheck, SnapshotHandle};

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
            manifest_path: "HARNESS.yml".into(),
            scope_path: if scope.is_empty() { "root".into() } else { scope.into() },
            depth,
            with_payload: serde_json::json!({ "target": target }),
        }
    }

    #[test]
    fn deepest_target_wins_when_both_root_and_child_present() {
        let response = resolve(req(vec![
            check("root", "app-build", "//App:App", 0),
            check("App/Login", "login-build", "//App/Login:Login", 2),
        ]))
        .unwrap();
        assert_eq!(response.tasks.len(), 1);
        assert!(response.tasks[0].title.contains("//App/Login:Login"));
        assert_eq!(
            response.tasks[0].satisfies,
            vec!["App/Login/login-build".to_string()]
        );
        assert_eq!(response.ignored_checks.len(), 1);
        assert_eq!(response.ignored_checks[0].id, "root/app-build");
    }

    #[test]
    fn only_root_present_emits_root_task() {
        let response = resolve(req(vec![check("root", "app-build", "//App:App", 0)])).unwrap();
        assert_eq!(response.tasks.len(), 1);
        assert!(response.ignored_checks.is_empty());
    }

    #[test]
    fn sibling_scopes_each_get_a_task() {
        let response = resolve(req(vec![
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
        let response = resolve(req(vec![c])).unwrap();
        assert!(response.tasks.is_empty());
        assert_eq!(response.ignored_checks.len(), 1);
        assert!(response.ignored_checks[0]
            .reason
            .contains("does not match"));
    }

    fn evidence_req(text: Option<&str>, assets: Vec<harness_protocol::EvidenceAsset>) -> EvidenceValidationRequest {
        use harness_protocol::{EvidenceSubmission, EvidenceTaskRef};
        EvidenceValidationRequest {
            protocol_version: PROTOCOL_VERSION,
            workspace_root: "/repo".into(),
            task: EvidenceTaskRef {
                id: "bazel-build-root".into(),
                extension: "bazel/build".into(),
                satisfies: vec!["root/app-build".into()],
                evidence_contract: EvidenceContract {
                    text: TextContract { required: true, description: None },
                    assets: vec![AssetContract { kind: "log".into(), required: true, description: None }],
                },
            },
            submission: EvidenceSubmission {
                text: text.map(|s| s.to_string()),
                assets,
            },
            snapshot: SnapshotHandle { handle: "h".into(), dirty_scopes: Vec::new() },
        }
    }

    fn asset(path: &str, mime: &str, size: u64) -> harness_protocol::EvidenceAsset {
        harness_protocol::EvidenceAsset { path: path.into(), mime: mime.into(), size }
    }

    #[test]
    fn evidence_accepts_text_plus_log() {
        let resp = evidence(evidence_req(
            Some("bazel build //App:App succeeded"),
            vec![asset("build.log", "text/plain", 4096)],
        ))
        .unwrap();
        assert!(resp.accepted);
        assert_eq!(resp.satisfies, vec!["root/app-build"]);
    }

    #[test]
    fn evidence_accepts_octet_stream_log() {
        let resp = evidence(evidence_req(
            Some("ok"),
            vec![asset("build.log", "application/octet-stream", 12)],
        ))
        .unwrap();
        assert!(resp.accepted);
    }

    #[test]
    fn evidence_rejects_empty_text() {
        let resp = evidence(evidence_req(
            Some(""),
            vec![asset("a.log", "text/plain", 4)],
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp.missing.iter().any(|m| m.kind == "text"));
    }

    #[test]
    fn evidence_rejects_missing_log() {
        let resp = evidence(evidence_req(Some("ok"), vec![])).unwrap();
        assert!(!resp.accepted);
        assert!(resp.missing.iter().any(|m| m.kind == "log"));
    }

    #[test]
    fn evidence_rejects_zero_byte_log() {
        let resp = evidence(evidence_req(
            Some("ok"),
            vec![asset("a.log", "text/plain", 0)],
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp.missing.iter().any(|m| m.kind == "log"));
    }

    #[test]
    fn is_deeper_segment_aware() {
        assert!(is_deeper("App/Login", "root"));
        assert!(is_deeper("App/Login", "App"));
        assert!(!is_deeper("App/LoginExtra", "App/Login"));
        assert!(!is_deeper("App/Login", "App/Login"));
    }
}
