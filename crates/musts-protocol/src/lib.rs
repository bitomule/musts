//! Wire types for the harness ⇄ extension JSON-over-stdio protocol.
//!
//! See `docs/harness-validation-plan.md` §9 (resolve) and §10 (evidence)
//! and `docs/PLAN.md` §4.6 (IPC contract).

use serde::{Deserialize, Serialize};

/// The protocol version every request and response carries.
///
/// Mismatches are rejected by the core; v2 reserves the right to break
/// the shape.
pub const PROTOCOL_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Resolve
// ---------------------------------------------------------------------------

/// Request sent from core to an extension's resolver.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolveRequest {
    pub protocol_version: u32,
    pub workspace_root: String,
    pub capability: String,
    pub changed_files: Vec<String>,
    pub checks: Vec<ResolveCheck>,
    pub snapshot: SnapshotHandle,
}

/// One applicable check passed to the resolver.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolveCheck {
    /// Globally stable check id: `<scope_path>/<local_id>` (root scope is `root`).
    pub id: String,
    /// Local id as declared in the manifest.
    pub local_id: String,
    /// Manifest path (workspace-relative).
    pub manifest_path: String,
    /// Scope path (workspace-relative, root = `root`).
    pub scope_path: String,
    /// Folder depth (root = 0).
    pub depth: u32,
    /// Extension-owned payload.
    #[serde(rename = "with")]
    pub with_payload: serde_json::Value,
}

/// Opaque snapshot handle. Extensions must not parse its structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotHandle {
    pub handle: String,
    pub dirty_scopes: Vec<String>,
}

/// Response returned from an extension's resolver to core.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolveResponse {
    pub protocol_version: u32,
    #[serde(default)]
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub ignored_checks: Vec<IgnoredCheck>,
    #[serde(default)]
    pub notes: Vec<String>,
}

/// One validation task emitted by an extension.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub extension: String,
    pub title: String,
    pub satisfies: Vec<String>,
    #[serde(default)]
    pub parallelizable: bool,
    pub instructions: Vec<String>,
    pub evidence_contract: EvidenceContract,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IgnoredCheck {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceContract {
    pub text: TextContract,
    #[serde(default)]
    pub assets: Vec<AssetContract>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextContract {
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetContract {
    pub kind: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// Evidence
// ---------------------------------------------------------------------------

/// Request sent from core to an extension's evidence validator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceValidationRequest {
    pub protocol_version: u32,
    pub workspace_root: String,
    pub task: EvidenceTaskRef,
    pub submission: EvidenceSubmission,
    pub snapshot: SnapshotHandle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceTaskRef {
    pub id: String,
    pub extension: String,
    pub satisfies: Vec<String>,
    pub evidence_contract: EvidenceContract,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceSubmission {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub assets: Vec<EvidenceAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceAsset {
    pub path: String,
    pub mime: String,
    pub size: u64,
}

/// Response from the extension after validating evidence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceValidationResponse {
    pub protocol_version: u32,
    pub accepted: bool,
    #[serde(default)]
    pub satisfies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub normalized_assets: Vec<NormalizedAsset>,
    #[serde(default)]
    pub missing: Vec<MissingEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NormalizedAsset {
    pub kind: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissingEvidence {
    pub kind: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_resolve_request() -> ResolveRequest {
        ResolveRequest {
            protocol_version: PROTOCOL_VERSION,
            workspace_root: "/repo".into(),
            capability: "bazel/build".into(),
            changed_files: vec!["App/Login/LoginView.swift".into()],
            checks: vec![ResolveCheck {
                id: "App/Login/login-build".into(),
                local_id: "login-build".into(),
                manifest_path: "App/Login/HARNESS.yml".into(),
                scope_path: "App/Login".into(),
                depth: 2,
                with_payload: serde_json::json!({ "target": "//App/Login:Login" }),
            }],
            snapshot: SnapshotHandle {
                handle: "opaque-core-handle".into(),
                dirty_scopes: vec!["App/Login".into()],
            },
        }
    }

    fn sample_resolve_response() -> ResolveResponse {
        ResolveResponse {
            protocol_version: PROTOCOL_VERSION,
            tasks: vec![Task {
                id: "bazel-build-login".into(),
                extension: "bazel/build".into(),
                title: "Build Login module".into(),
                satisfies: vec!["App/Login/login-build".into()],
                parallelizable: true,
                instructions: vec!["Run `bazel build //App/Login:Login`.".into()],
                evidence_contract: EvidenceContract {
                    text: TextContract {
                        required: true,
                        description: Some("State the command and whether it succeeded.".into()),
                    },
                    assets: vec![AssetContract {
                        kind: "log".into(),
                        required: true,
                        description: None,
                    }],
                },
            }],
            ignored_checks: vec![IgnoredCheck {
                id: "root/app-build".into(),
                reason: "A deeper bazel/build target covers the changed scope.".into(),
            }],
            notes: vec!["bazel/build selected the deepest applicable target.".into()],
        }
    }

    fn sample_evidence_request() -> EvidenceValidationRequest {
        EvidenceValidationRequest {
            protocol_version: PROTOCOL_VERSION,
            workspace_root: "/repo".into(),
            task: EvidenceTaskRef {
                id: "mav-login-flow".into(),
                extension: "mav/expect".into(),
                satisfies: vec!["App/Login/login-flow".into()],
                evidence_contract: EvidenceContract {
                    text: TextContract {
                        required: true,
                        description: None,
                    },
                    assets: vec![AssetContract {
                        kind: "screenshot".into(),
                        required: true,
                        description: None,
                    }],
                },
            },
            submission: EvidenceSubmission {
                text: Some("Validated login flow.".into()),
                assets: vec![EvidenceAsset {
                    path: ".harness/evidence/mav-login-flow/submission-001/success.png".into(),
                    mime: "image/png".into(),
                    size: 182_331,
                }],
            },
            snapshot: SnapshotHandle {
                handle: "opaque".into(),
                dirty_scopes: vec![],
            },
        }
    }

    fn sample_evidence_response_accepted() -> EvidenceValidationResponse {
        EvidenceValidationResponse {
            protocol_version: PROTOCOL_VERSION,
            accepted: true,
            satisfies: vec!["App/Login/login-flow".into()],
            summary: Some("Required assets present.".into()),
            normalized_assets: vec![NormalizedAsset {
                kind: "screenshot".into(),
                path: ".harness/evidence/mav-login-flow/submission-001/success.png".into(),
            }],
            missing: vec![],
            message: None,
        }
    }

    fn sample_evidence_response_rejected() -> EvidenceValidationResponse {
        EvidenceValidationResponse {
            protocol_version: PROTOCOL_VERSION,
            accepted: false,
            satisfies: vec![],
            summary: None,
            normalized_assets: vec![],
            missing: vec![MissingEvidence {
                kind: "screenshot".into(),
                message: "No screenshot asset was submitted.".into(),
            }],
            message: Some("Evidence is incomplete. Capture a screenshot and submit it.".into()),
        }
    }

    #[test]
    fn roundtrip_resolve_request() {
        let original = sample_resolve_request();
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: ResolveRequest = serde_json::from_str(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn roundtrip_resolve_response() {
        let original = sample_resolve_response();
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: ResolveResponse = serde_json::from_str(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn roundtrip_evidence_request() {
        let original = sample_evidence_request();
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: EvidenceValidationRequest = serde_json::from_str(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn roundtrip_evidence_response_accepted() {
        let original = sample_evidence_response_accepted();
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: EvidenceValidationResponse = serde_json::from_str(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn roundtrip_evidence_response_rejected() {
        let original = sample_evidence_response_rejected();
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: EvidenceValidationResponse = serde_json::from_str(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn resolve_response_accepts_defaults_for_optional_arrays() {
        // Minimal response: clean run with no tasks and no notes.
        let raw = r#"{"protocol_version":1}"#;
        let decoded: ResolveResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(decoded.protocol_version, PROTOCOL_VERSION);
        assert!(decoded.tasks.is_empty());
        assert!(decoded.ignored_checks.is_empty());
        assert!(decoded.notes.is_empty());
    }
}
