//! Built-in `mav/expect` capability per `docs/PLAN.md` §6.0 / §6.2 /
//! §16.2.
//!
//! Bucket checks by `scope_path` so one MAV session validates every
//! expectation declared in that scope. Cross-scope merging is left for
//! future work — different scopes typically represent different
//! features.
//!
//! `with` payload schema:
//!
//! ```json
//! {
//!   "expectations": ["…"],
//!   "evidence": ["screenshot" | "video" | "mav-report" | "accessibility-tree" | "logs"]
//! }
//! ```
//!
//! Evidence contract: text + the required asset kinds. Kinds are
//! classified by MIME; `mav-report`/`accessibility-tree` must be
//! parseable JSON.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::LazyLock;

use musts_protocol::{
    AssetContract, EvidenceAsset, EvidenceContract, EvidenceValidationRequest,
    EvidenceValidationResponse, IgnoredCheck, MissingEvidence, NormalizedAsset, ResolveRequest,
    ResolveResponse, Task, TextContract, PROTOCOL_VERSION,
};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use super::util::{is_image, is_json, is_log_or_text, is_video, scope_slug};
use crate::error::Error;

pub fn schema() -> &'static JsonValue {
    static SCHEMA: LazyLock<JsonValue> = LazyLock::new(|| {
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "expectations": {
                    "type": "array",
                    "items": { "type": "string", "minLength": 1 }
                },
                "evidence": {
                    "type": "array",
                    "items": {
                        "type": "string",
                        "enum": [
                            "screenshot",
                            "video",
                            "mav-report",
                            "accessibility-tree",
                            "logs"
                        ]
                    }
                }
            }
        })
    });
    &SCHEMA
}

#[derive(Debug, Deserialize)]
struct ExpectWith {
    #[serde(default)]
    expectations: Vec<String>,
    #[serde(default)]
    evidence: Vec<String>,
}

const VALID_KINDS: &[&str] = &[
    "screenshot",
    "video",
    "mav-report",
    "accessibility-tree",
    "logs",
];

pub fn resolve(request: &ResolveRequest) -> Result<ResolveResponse, Error> {
    let mut by_scope: BTreeMap<String, BucketAccum> = BTreeMap::new();
    let mut ignored = Vec::new();
    for check in &request.checks {
        let payload: ExpectWith = match serde_json::from_value(check.with_payload.clone()) {
            Ok(v) => v,
            Err(_) => {
                ignored.push(IgnoredCheck {
                    id: check.id.clone(),
                    reason: "with-payload does not match the mav/expect schema".into(),
                });
                continue;
            }
        };
        if payload
            .evidence
            .iter()
            .any(|k| !VALID_KINDS.contains(&k.as_str()))
        {
            ignored.push(IgnoredCheck {
                id: check.id.clone(),
                reason: "evidence list contains an unknown asset kind".into(),
            });
            continue;
        }

        let entry = by_scope.entry(check.scope_path.clone()).or_default();
        entry.satisfies.push(check.id.clone());
        for e in payload.expectations {
            entry.expectations.insert(e);
        }
        for k in payload.evidence {
            entry.kinds.insert(k);
        }
    }

    let mut tasks = Vec::with_capacity(by_scope.len());
    for (scope, bucket) in by_scope {
        let task_id = format!("mav-expect-{}", scope_slug(&scope));
        let mut instructions = Vec::new();
        instructions.push("Use MAV to validate:".to_string());
        for e in &bucket.expectations {
            instructions.push(format!("  - {e}"));
        }
        if !bucket.kinds.is_empty() {
            instructions.push("Capture the required evidence:".to_string());
            for k in &bucket.kinds {
                instructions.push(format!("  - {k}"));
            }
        }
        instructions.push(format!(
            "Record evidence with `musts evidence {task_id} --text \"<summary>\" --asset <path>`."
        ));
        let assets: Vec<AssetContract> = bucket
            .kinds
            .iter()
            .map(|k| AssetContract {
                kind: k.clone(),
                required: true,
                description: None,
            })
            .collect();
        tasks.push(Task {
            id: task_id,
            extension: "mav/expect".into(),
            title: format!("Validate MAV expectations for {scope}"),
            satisfies: bucket.satisfies,
            parallelizable: false,
            // UI expectations need agent-driven capture; not `musts run`-able.
            command: None,
            instructions,
            evidence_contract: EvidenceContract {
                text: TextContract {
                    required: true,
                    description: Some(
                        "Summarise which expectations passed and any deviations.".into(),
                    ),
                },
                assets,
            },
        });
    }
    Ok(ResolveResponse {
        protocol_version: PROTOCOL_VERSION,
        tasks,
        ignored_checks: ignored,
        notes: Vec::new(),
    })
}

#[derive(Default)]
struct BucketAccum {
    satisfies: Vec<String>,
    expectations: BTreeSet<String>,
    kinds: BTreeSet<String>,
}

pub fn evidence(request: &EvidenceValidationRequest) -> Result<EvidenceValidationResponse, Error> {
    let text = request.submission.text.as_deref().unwrap_or("");
    let assets: &[EvidenceAsset] = &request.submission.assets;
    let required_kinds: Vec<String> = request
        .task
        .evidence_contract
        .assets
        .iter()
        .filter(|a| a.required)
        .map(|a| a.kind.clone())
        .collect();

    let mut missing: Vec<MissingEvidence> = Vec::new();
    if text.trim().is_empty() {
        missing.push(MissingEvidence {
            kind: "text".into(),
            message: "Provide a text summary of the MAV session.".into(),
        });
    }

    let mut classified: BTreeMap<String, Vec<&EvidenceAsset>> = BTreeMap::new();
    for asset in assets {
        if let Some(kind) = classify_asset(asset) {
            classified.entry(kind).or_default().push(asset);
        }
    }

    let workspace_root = Path::new(&request.workspace_root);
    for kind in &required_kinds {
        let entries = classified_for(&classified, kind);
        match entries {
            None => missing.push(MissingEvidence {
                kind: kind.clone(),
                message: format!("No `{kind}` asset was submitted."),
            }),
            Some(list) => {
                if let Some(asset) = list.iter().find(|a| a.size == 0) {
                    missing.push(MissingEvidence {
                        kind: kind.clone(),
                        message: format!("{} asset `{}` is empty.", human_kind(kind), asset.path),
                    });
                    continue;
                }
                if kind == "mav-report" || kind == "accessibility-tree" {
                    for asset in list {
                        if let Some(problem) = check_json_asset(workspace_root, kind, asset) {
                            missing.push(problem);
                        }
                    }
                }
            }
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

    let mut normalized_assets: Vec<NormalizedAsset> = Vec::new();
    for (kind, list) in &classified {
        for asset in list {
            normalized_assets.push(NormalizedAsset {
                kind: kind.clone(),
                path: asset.path.clone(),
            });
        }
    }

    Ok(EvidenceValidationResponse {
        protocol_version: PROTOCOL_VERSION,
        accepted: true,
        satisfies: request.task.satisfies.clone(),
        summary: Some(format!(
            "MAV evidence accepted ({} asset{}).",
            normalized_assets.len(),
            if normalized_assets.len() == 1 {
                ""
            } else {
                "s"
            },
        )),
        normalized_assets,
        missing: Vec::new(),
        message: None,
    })
}

/// Look up classified assets for a required kind, with the `mav-report`
/// ↔ `accessibility-tree` alias from `classify_asset` applied so a JSON
/// asset can satisfy either requirement.
fn classified_for<'a>(
    classified: &'a BTreeMap<String, Vec<&'a EvidenceAsset>>,
    required: &str,
) -> Option<&'a Vec<&'a EvidenceAsset>> {
    if let Some(list) = classified.get(required) {
        return Some(list);
    }
    if required == "accessibility-tree" {
        return classified.get("mav-report");
    }
    None
}

fn classify_asset(asset: &EvidenceAsset) -> Option<String> {
    if is_image(asset) {
        Some("screenshot".into())
    } else if is_video(asset) {
        Some("video".into())
    } else if is_json(asset) {
        // Both `mav-report` and `accessibility-tree` ride on JSON; MIME
        // alone can't distinguish them. We classify any JSON asset as
        // `mav-report`, and the required-kinds loop accepts a
        // `mav-report` asset against an `accessibility-tree` requirement
        // via the alias in `classified_for`.
        Some("mav-report".into())
    } else if is_log_or_text(asset) {
        Some("logs".into())
    } else {
        None
    }
}

/// Streamingly parse `asset` as JSON, returning a [`MissingEvidence`] with
/// `kind` if either I/O or `serde_json` rejects it.
fn check_json_asset(
    workspace_root: &Path,
    kind: &str,
    asset: &EvidenceAsset,
) -> Option<MissingEvidence> {
    let abs = workspace_root.join(&asset.path);
    let file = match File::open(&abs) {
        Ok(f) => f,
        Err(err) => {
            return Some(MissingEvidence {
                kind: kind.into(),
                message: format!(
                    "could not read {} asset `{}`: {err}",
                    human_kind(kind),
                    asset.path
                ),
            });
        }
    };
    if let Err(err) = serde_json::from_reader::<_, JsonValue>(BufReader::new(file)) {
        return Some(MissingEvidence {
            kind: kind.into(),
            message: format!(
                "{} asset `{}` is not parseable JSON: {err}",
                human_kind(kind),
                asset.path
            ),
        });
    }
    None
}

fn human_kind(kind: &str) -> &'static str {
    match kind {
        "screenshot" => "Screenshot",
        "video" => "Video",
        "mav-report" => "MAV report",
        "accessibility-tree" => "Accessibility tree",
        "logs" => "Log",
        _ => "Asset",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musts_protocol::{EvidenceSubmission, EvidenceTaskRef, ResolveCheck, SnapshotHandle};

    fn req(checks: Vec<ResolveCheck>) -> ResolveRequest {
        ResolveRequest {
            protocol_version: PROTOCOL_VERSION,
            workspace_root: "/repo".into(),
            capability: "mav/expect".into(),
            changed_files: Vec::new(),
            checks,
            snapshot: SnapshotHandle {
                handle: "h".into(),
                dirty_scopes: Vec::new(),
            },
        }
    }

    fn check(
        scope: &str,
        local: &str,
        expectations: Vec<&str>,
        evidence: Vec<&str>,
    ) -> ResolveCheck {
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
            with_payload: serde_json::json!({
                "expectations": expectations,
                "evidence": evidence,
            }),
        }
    }

    #[test]
    fn groups_two_checks_in_one_scope_into_one_task() {
        let response = resolve(&req(vec![
            check(
                "App/Login",
                "valid",
                vec!["Login works with multiple valid emails."],
                vec!["screenshot", "video"],
            ),
            check(
                "App/Login",
                "invalid",
                vec!["Invalid email text shows an error when used as email."],
                vec!["screenshot", "video", "mav-report"],
            ),
        ]))
        .unwrap();
        assert_eq!(response.tasks.len(), 1);
        let task = &response.tasks[0];
        assert_eq!(task.satisfies.len(), 2);
        assert!(task
            .instructions
            .iter()
            .any(|s| s.contains("Login works with multiple valid emails.")));
        assert!(task
            .instructions
            .iter()
            .any(|s| s.contains("Invalid email text")));
        let kinds: Vec<_> = task
            .evidence_contract
            .assets
            .iter()
            .map(|a| a.kind.clone())
            .collect();
        assert!(kinds.contains(&"screenshot".to_string()));
        assert!(kinds.contains(&"video".to_string()));
        assert!(kinds.contains(&"mav-report".to_string()));
    }

    #[test]
    fn sibling_scopes_each_get_one_task() {
        let response = resolve(&req(vec![
            check("App/Login", "a", vec!["E1"], vec!["screenshot"]),
            check("App/Profile", "a", vec!["E2"], vec!["screenshot"]),
        ]))
        .unwrap();
        assert_eq!(response.tasks.len(), 2);
    }

    #[test]
    fn malformed_payload_is_ignored_with_reason() {
        let mut c = check("root", "x", vec!["E"], vec!["screenshot"]);
        c.with_payload = serde_json::json!({ "expectations": "not an array" });
        let response = resolve(&req(vec![c])).unwrap();
        assert!(response.tasks.is_empty());
        assert_eq!(response.ignored_checks.len(), 1);
    }

    #[test]
    fn unknown_evidence_kind_is_ignored() {
        let c = check("root", "x", vec!["E"], vec!["holographic"]);
        let response = resolve(&req(vec![c])).unwrap();
        assert!(response.tasks.is_empty());
        assert_eq!(response.ignored_checks.len(), 1);
        assert!(response.ignored_checks[0]
            .reason
            .contains("unknown asset kind"));
    }

    fn evidence_req(
        text: Option<&str>,
        assets: Vec<EvidenceAsset>,
        required_kinds: Vec<&str>,
    ) -> EvidenceValidationRequest {
        evidence_req_with_root(text, assets, required_kinds, "/repo")
    }

    fn evidence_req_with_root(
        text: Option<&str>,
        assets: Vec<EvidenceAsset>,
        required_kinds: Vec<&str>,
        workspace_root: &str,
    ) -> EvidenceValidationRequest {
        EvidenceValidationRequest {
            protocol_version: PROTOCOL_VERSION,
            workspace_root: workspace_root.into(),
            task: EvidenceTaskRef {
                id: "mav-expect-root".into(),
                extension: "mav/expect".into(),
                satisfies: vec!["root/x".into()],
                evidence_contract: EvidenceContract {
                    text: TextContract {
                        required: true,
                        description: None,
                    },
                    assets: required_kinds
                        .into_iter()
                        .map(|k| AssetContract {
                            kind: k.into(),
                            required: true,
                            description: None,
                        })
                        .collect(),
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

    #[test]
    fn evidence_accepts_all_required_kinds() {
        let workspace = tempfile::tempdir().unwrap();
        let report = workspace.path().join("c.json");
        std::fs::write(&report, br#"{"summary":"ok"}"#).unwrap();
        let resp = evidence(&evidence_req_with_root(
            Some("ok"),
            vec![
                asset("a.png", "image/png", 100),
                asset("b.mp4", "video/mp4", 100),
                asset("c.json", "application/json", 16),
            ],
            vec!["screenshot", "video", "mav-report"],
            workspace.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(resp.accepted, "missing: {:?}", resp.missing);
    }

    #[test]
    fn evidence_rejects_invalid_mav_report_json() {
        let workspace = tempfile::tempdir().unwrap();
        let report = workspace.path().join("report.json");
        std::fs::write(&report, b"not valid json").unwrap();
        let resp = evidence(&evidence_req_with_root(
            Some("ok"),
            vec![asset("report.json", "application/json", 14)],
            vec!["mav-report"],
            workspace.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp
            .missing
            .iter()
            .any(|m| m.message.contains("not parseable JSON")));
    }

    #[test]
    fn evidence_accepts_accessibility_tree_from_json_asset() {
        // `classify_asset` tags every JSON asset as `mav-report` (MIME
        // can't distinguish accessibility-tree from mav-report). The
        // `classified_for` alias must let a `mav-report` asset satisfy
        // an `accessibility-tree` requirement.
        let workspace = tempfile::tempdir().unwrap();
        let tree = workspace.path().join("tree.json");
        std::fs::write(&tree, br#"{"role":"button"}"#).unwrap();
        let resp = evidence(&evidence_req_with_root(
            Some("ok"),
            vec![asset("tree.json", "application/json", 17)],
            vec!["accessibility-tree"],
            workspace.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(resp.accepted, "missing: {:?}", resp.missing);
    }

    #[test]
    fn evidence_rejects_missing_kind() {
        let resp = evidence(&evidence_req(
            Some("ok"),
            vec![asset("a.png", "image/png", 100)],
            vec!["screenshot", "video"],
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp.missing.iter().any(|m| m.kind == "video"));
    }

    #[test]
    fn evidence_rejects_zero_byte_asset() {
        let resp = evidence(&evidence_req(
            Some("ok"),
            vec![asset("a.png", "image/png", 0)],
            vec!["screenshot"],
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp
            .missing
            .iter()
            .any(|m| m.kind == "screenshot" && m.message.contains("empty")));
    }

    #[test]
    fn evidence_rejects_missing_text() {
        let resp = evidence(&evidence_req(
            Some(""),
            vec![asset("a.png", "image/png", 100)],
            vec!["screenshot"],
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp.missing.iter().any(|m| m.kind == "text"));
    }
}
