//! Built-in `agent` capability per `docs/PLAN.md` §6.0.
//!
//! Manifest shape:
//!
//! ```yaml
//! checks:
//!   login-form-visual:
//!     uses: agent
//!     with:
//!       facts:
//!         - "El formulario muestra error si el email está vacío."
//!         - "El campo de contraseña está enmascarado."
//! ```
//!
//! Behaviour:
//! - Bucket dirty checks by `scope_path`; emit one task per scope that
//!   lists every fact in that bucket.
//! - Evidence contract: text required, assets optional (any kind).
//! - Evidence validator: text non-empty → accept; empty → reject with
//!   `missing: [{ kind: "text" }]`.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use musts_protocol::{
    AssetContract, EvidenceContract, EvidenceValidationRequest, EvidenceValidationResponse,
    IgnoredCheck, MissingEvidence, NormalizedAsset, ResolveRequest, ResolveResponse, Task,
    TextContract, PROTOCOL_VERSION,
};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use super::util::scope_slug;
use crate::error::Error;

pub fn schema() -> &'static JsonValue {
    static SCHEMA: LazyLock<JsonValue> = LazyLock::new(|| {
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "required": ["facts"],
            "properties": {
                "facts": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1
                }
            },
            "additionalProperties": false
        })
    });
    &SCHEMA
}

#[derive(Debug, Deserialize)]
struct AgentWith {
    facts: Vec<String>,
}

pub fn resolve(request: &ResolveRequest) -> Result<ResolveResponse, Error> {
    let mut by_scope: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut ignored = Vec::new();
    for check in &request.checks {
        let payload: AgentWith = match serde_json::from_value(check.with_payload.clone()) {
            Ok(p) => p,
            Err(_) => {
                ignored.push(IgnoredCheck {
                    id: check.id.clone(),
                    reason: "with-payload does not match the agent schema".into(),
                });
                continue;
            }
        };
        let entry = by_scope.entry(check.scope_path.clone()).or_default();
        entry.satisfies.push(check.id.clone());
        entry.facts.extend(payload.facts);
    }

    let mut tasks = Vec::with_capacity(by_scope.len());
    for (scope, bucket) in by_scope {
        let slug = scope_slug(&scope);
        let title = if scope == "root" {
            "Agent: verify facts at the workspace root".to_string()
        } else {
            format!("Agent: verify facts under {scope}")
        };
        let mut instructions =
            vec!["Verify each of these facts about the current workspace state:".to_string()];
        for f in &bucket.facts {
            instructions.push(format!("  - {f}"));
        }
        instructions.push(format!(
            "Record evidence with `musts evidence agent-{slug} --text \"<summary>\"`; attach any \
             screenshots, logs, or other assets that support your conclusion."
        ));
        tasks.push(Task {
            id: format!("agent-{slug}"),
            extension: "agent".into(),
            title,
            satisfies: bucket.satisfies,
            parallelizable: true,
            instructions,
            // No required `assets` — the agent decides what to attach.
            evidence_contract: EvidenceContract {
                text: TextContract {
                    required: true,
                    description: Some(
                        "Summarise which facts you verified and how (one paragraph is plenty)."
                            .into(),
                    ),
                },
                assets: Vec::<AssetContract>::new(),
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
struct Bucket {
    satisfies: Vec<String>,
    facts: Vec<String>,
}

pub fn evidence(request: &EvidenceValidationRequest) -> Result<EvidenceValidationResponse, Error> {
    let text = request.submission.text.as_deref().unwrap_or("");
    if text.trim().is_empty() {
        return Ok(EvidenceValidationResponse {
            protocol_version: PROTOCOL_VERSION,
            accepted: false,
            satisfies: Vec::new(),
            summary: None,
            normalized_assets: Vec::new(),
            missing: vec![MissingEvidence {
                kind: "text".into(),
                message: "Provide a text summary of which facts you verified.".into(),
            }],
            message: Some("Evidence is incomplete.".into()),
        });
    }
    let normalized_assets = request
        .submission
        .assets
        .iter()
        .map(|a| NormalizedAsset {
            kind: classify_asset(&a.mime),
            path: a.path.clone(),
        })
        .collect();
    Ok(EvidenceValidationResponse {
        protocol_version: PROTOCOL_VERSION,
        accepted: true,
        satisfies: request.task.satisfies.clone(),
        summary: Some(format!(
            "Agent verification accepted ({} asset{}).",
            request.submission.assets.len(),
            if request.submission.assets.len() == 1 {
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

fn classify_asset(mime: &str) -> String {
    if mime.starts_with("image/") {
        "screenshot".into()
    } else if mime.starts_with("video/") {
        "video".into()
    } else if mime == "application/json" {
        "json".into()
    } else if mime.starts_with("text/") || mime == "application/octet-stream" {
        "log".into()
    } else {
        "asset".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musts_protocol::{
        EvidenceAsset, EvidenceContract, EvidenceSubmission, EvidenceTaskRef, ResolveCheck,
        SnapshotHandle, TextContract,
    };

    fn req(checks: Vec<ResolveCheck>) -> ResolveRequest {
        ResolveRequest {
            protocol_version: PROTOCOL_VERSION,
            workspace_root: "/repo".into(),
            capability: "agent".into(),
            changed_files: Vec::new(),
            checks,
            snapshot: SnapshotHandle {
                handle: "h".into(),
                dirty_scopes: Vec::new(),
            },
        }
    }

    fn check(scope: &str, local: &str, facts: Vec<&str>) -> ResolveCheck {
        let payload = serde_json::json!({
            "facts": facts,
        });
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
            with_payload: payload,
        }
    }

    #[test]
    fn resolve_buckets_two_checks_into_one_task_per_scope() {
        let response = resolve(&req(vec![
            check("App/Login", "valid", vec!["F1"]),
            check("App/Login", "invalid", vec!["F2"]),
        ]))
        .unwrap();
        assert_eq!(response.tasks.len(), 1);
        let task = &response.tasks[0];
        assert_eq!(task.extension, "agent");
        assert_eq!(task.id, "agent-app-login");
        assert!(task.instructions.iter().any(|i| i.contains("F1")));
        assert!(task.instructions.iter().any(|i| i.contains("F2")));
        assert_eq!(task.satisfies.len(), 2);
        // Evidence contract: text required, no asset requirements.
        assert!(task.evidence_contract.text.required);
        assert!(task.evidence_contract.assets.is_empty());
    }

    #[test]
    fn resolve_separates_scopes() {
        let response = resolve(&req(vec![
            check("App/Login", "a", vec!["F1"]),
            check("App/Profile", "a", vec!["F2"]),
        ]))
        .unwrap();
        assert_eq!(response.tasks.len(), 2);
    }

    #[test]
    fn resolve_root_scope_uses_root_slug() {
        let response = resolve(&req(vec![check("root", "x", vec!["F"])])).unwrap();
        assert_eq!(response.tasks[0].id, "agent-root");
        assert!(response.tasks[0].title.contains("root"));
    }

    #[test]
    fn resolve_ignores_malformed_payload() {
        let mut c = check("root", "x", vec!["F"]);
        c.with_payload = serde_json::json!({ "not_facts": 1 });
        let response = resolve(&req(vec![c])).unwrap();
        assert!(response.tasks.is_empty());
        assert_eq!(response.ignored_checks.len(), 1);
    }

    fn evidence_req(text: Option<&str>, assets: Vec<EvidenceAsset>) -> EvidenceValidationRequest {
        EvidenceValidationRequest {
            protocol_version: PROTOCOL_VERSION,
            workspace_root: "/repo".into(),
            task: EvidenceTaskRef {
                id: "agent-root".into(),
                extension: "agent".into(),
                satisfies: vec!["root/x".into()],
                evidence_contract: EvidenceContract {
                    text: TextContract {
                        required: true,
                        description: None,
                    },
                    assets: Vec::new(),
                },
            },
            submission: EvidenceSubmission {
                text: text.map(|s| s.into()),
                assets,
            },
            snapshot: SnapshotHandle {
                handle: "h".into(),
                dirty_scopes: Vec::new(),
            },
        }
    }

    #[test]
    fn evidence_accepts_text_with_no_assets() {
        let r = evidence(&evidence_req(Some("verified all facts"), vec![])).unwrap();
        assert!(r.accepted);
        assert_eq!(r.satisfies, vec!["root/x"]);
    }

    #[test]
    fn evidence_accepts_text_with_arbitrary_assets() {
        let r = evidence(&evidence_req(
            Some("verified"),
            vec![
                EvidenceAsset {
                    path: "shot.png".into(),
                    mime: "image/png".into(),
                    size: 100,
                },
                EvidenceAsset {
                    path: "x.weird".into(),
                    mime: "application/x-weird".into(),
                    size: 1,
                },
            ],
        ))
        .unwrap();
        assert!(r.accepted);
        // Assets are normalised by MIME to a best-effort kind label,
        // not rejected for being "unexpected".
        assert_eq!(r.normalized_assets.len(), 2);
        assert!(r.normalized_assets.iter().any(|a| a.kind == "screenshot"));
        assert!(r.normalized_assets.iter().any(|a| a.kind == "asset"));
    }

    #[test]
    fn evidence_rejects_empty_text() {
        let r = evidence(&evidence_req(Some(""), vec![])).unwrap();
        assert!(!r.accepted);
        assert_eq!(r.missing.len(), 1);
        assert_eq!(r.missing[0].kind, "text");
    }

    #[test]
    fn evidence_rejects_missing_text() {
        let r = evidence(&evidence_req(None, vec![])).unwrap();
        assert!(!r.accepted);
        assert_eq!(r.missing[0].kind, "text");
    }

    #[test]
    fn schema_rejects_missing_facts() {
        let s = schema();
        let validator = jsonschema::validator_for(s).unwrap();
        let bad = serde_json::json!({});
        assert!(!validator.is_valid(&bad));
    }

    #[test]
    fn schema_rejects_empty_facts_array() {
        let s = schema();
        let validator = jsonschema::validator_for(s).unwrap();
        let bad = serde_json::json!({ "facts": [] });
        assert!(!validator.is_valid(&bad));
    }

    #[test]
    fn schema_rejects_non_string_fact() {
        let s = schema();
        let validator = jsonschema::validator_for(s).unwrap();
        let bad = serde_json::json!({ "facts": [42] });
        assert!(!validator.is_valid(&bad));
    }

    #[test]
    fn schema_rejects_additional_properties() {
        let s = schema();
        let validator = jsonschema::validator_for(s).unwrap();
        let bad = serde_json::json!({ "facts": ["x"], "extra": "y" });
        assert!(!validator.is_valid(&bad));
    }

    #[test]
    fn schema_accepts_valid_payload() {
        let s = schema();
        let validator = jsonschema::validator_for(s).unwrap();
        let good = serde_json::json!({ "facts": ["F1", "F2"] });
        assert!(validator.is_valid(&good));
    }
}
