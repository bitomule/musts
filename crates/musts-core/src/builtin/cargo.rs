//! Built-in `cargo/{fmt,clippy,test}` capabilities per `docs/PLAN.md` §6.0.
//!
//! Evidence-only: the agent runs `cargo` itself, captures stdout/stderr
//! to a log file, and submits it as an asset. The capability validates
//! the log content with capability-specific heuristics:
//!
//! - `cargo/fmt`    — log must not contain `Diff in `.
//! - `cargo/clippy` — log must not contain a line starting with
//!   `error:` or `warning:` (clippy with `-D warnings` surfaces both
//!   as failures).
//! - `cargo/test`   — log must contain `test result: ok.` and must not
//!   contain `test result: FAILED`.

use std::path::PathBuf;
use std::sync::LazyLock;

use musts_protocol::{
    AssetContract, EvidenceAsset, EvidenceContract, EvidenceValidationRequest,
    EvidenceValidationResponse, IgnoredCheck, MissingEvidence, NormalizedAsset, ResolveRequest,
    ResolveResponse, Task, TextContract, PROTOCOL_VERSION,
};
use serde_json::Value as JsonValue;

use super::util::{is_log_or_text, scope_slug};
use crate::error::Error;

pub fn schema() -> &'static JsonValue {
    static SCHEMA: LazyLock<JsonValue> = LazyLock::new(|| {
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "properties": {}
        })
    });
    &SCHEMA
}

pub fn resolve(request: &ResolveRequest) -> Result<ResolveResponse, Error> {
    let capability =
        Capability::parse(&request.capability).ok_or_else(|| Error::ExtensionFailure {
            capability: request.capability.clone(),
            message: format!(
                "built-in cargo capability dispatched with unknown id `{}` (expected cargo/fmt, \
                 cargo/clippy or cargo/test)",
                request.capability
            ),
        })?;

    let mut tasks = Vec::with_capacity(request.checks.len());
    let mut ignored = Vec::new();

    for check in &request.checks {
        if !is_empty_object(&check.with_payload) {
            ignored.push(IgnoredCheck {
                id: check.id.clone(),
                reason: format!("cargo/{} takes no `with` parameters", capability.slug()),
            });
            continue;
        }
        let slug = scope_slug(&check.scope_path);
        tasks.push(Task {
            id: format!("cargo-{}-{}", capability.slug(), slug),
            extension: request.capability.clone(),
            title: format!("Run `{}`", capability.command()),
            satisfies: vec![check.id.clone()],
            parallelizable: true,
            instructions: vec![
                format!("Run `{}` from the workspace root.", capability.command()),
                "Capture combined stdout/stderr to a file (outside the workspace so the snapshot \
                 hash does not change while you submit)."
                    .into(),
                format!(
                    "Record the result with `musts evidence cargo-{}-{} --text \"…\" --asset \
                     <log>`.",
                    capability.slug(),
                    slug
                ),
            ],
            evidence_contract: EvidenceContract {
                text: TextContract {
                    required: true,
                    description: Some(capability.text_description().into()),
                },
                assets: vec![AssetContract {
                    kind: "log".into(),
                    required: true,
                    description: Some(capability.log_description().into()),
                }],
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

pub fn evidence(request: &EvidenceValidationRequest) -> Result<EvidenceValidationResponse, Error> {
    let capability =
        Capability::parse(&request.task.extension).ok_or_else(|| Error::ExtensionFailure {
            capability: request.task.extension.clone(),
            message: format!(
                "built-in cargo capability invoked with unknown id `{}` in task.extension",
                request.task.extension
            ),
        })?;

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
            message: "Provide a one-line summary stating whether the command succeeded.".into(),
        });
    }
    if log_assets.is_empty() {
        missing.push(MissingEvidence {
            kind: "log".into(),
            message: format!(
                "Attach the stdout/stderr of `{}` as a `text/*` or `application/octet-stream` \
                 asset.",
                capability.command()
            ),
        });
    }
    if let Some(empty) = log_assets.iter().find(|a| a.size == 0) {
        missing.push(MissingEvidence {
            kind: "log".into(),
            message: format!(
                "Log asset `{}` is empty; record the real command output.",
                empty.path
            ),
        });
    }

    if missing.is_empty() {
        if let Some(problem) = inspect_log(capability, &request.workspace_root, &log_assets) {
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
            "cargo/{} evidence accepted ({} log asset{}).",
            capability.slug(),
            log_assets.len(),
            if log_assets.len() == 1 { "" } else { "s" },
        )),
        normalized_assets,
        missing: Vec::new(),
        message: None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capability {
    Fmt,
    Clippy,
    Test,
}

impl Capability {
    fn parse(uses: &str) -> Option<Self> {
        match uses {
            "cargo/fmt" => Some(Capability::Fmt),
            "cargo/clippy" => Some(Capability::Clippy),
            "cargo/test" => Some(Capability::Test),
            _ => None,
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Capability::Fmt => "fmt",
            Capability::Clippy => "clippy",
            Capability::Test => "test",
        }
    }

    fn command(self) -> &'static str {
        match self {
            Capability::Fmt => "cargo fmt --check",
            Capability::Clippy => "cargo clippy --workspace --all-targets -- -D warnings",
            Capability::Test => "cargo test --workspace",
        }
    }

    fn text_description(self) -> &'static str {
        match self {
            Capability::Fmt => "State whether `cargo fmt --check` reported any diffs.",
            Capability::Clippy => {
                "State whether `cargo clippy --workspace --all-targets -- -D warnings` was clean."
            }
            Capability::Test => "State whether `cargo test --workspace` was green.",
        }
    }

    fn log_description(self) -> &'static str {
        match self {
            Capability::Fmt => "Stdout/stderr from `cargo fmt --check`.",
            Capability::Clippy => "Stdout/stderr from `cargo clippy`.",
            Capability::Test => "Stdout/stderr from `cargo test`.",
        }
    }
}

fn is_empty_object(value: &JsonValue) -> bool {
    match value {
        JsonValue::Object(map) => map.is_empty(),
        JsonValue::Null => true,
        _ => false,
    }
}

/// Read the first non-empty log asset and run the capability-specific
/// failure heuristic against its contents. Returns `Some(MissingEvidence)`
/// if the log is unreadable or the heuristic rejects it.
fn inspect_log(
    capability: Capability,
    workspace_root: &str,
    log_assets: &[&EvidenceAsset],
) -> Option<MissingEvidence> {
    let log = log_assets.iter().find(|a| a.size > 0)?;
    let abs_path = PathBuf::from(workspace_root).join(&log.path);
    match std::fs::read_to_string(&abs_path) {
        Ok(contents) => capability_failure(capability, &contents).map(|problem| MissingEvidence {
            kind: "log".into(),
            message: problem,
        }),
        Err(err) => Some(MissingEvidence {
            kind: "log".into(),
            message: format!("Could not read log asset `{}`: {err}", log.path),
        }),
    }
}

fn capability_failure(capability: Capability, log: &str) -> Option<String> {
    match capability {
        Capability::Fmt => {
            if log.contains("Diff in ") {
                Some(
                    "Log contains `Diff in ` markers — `cargo fmt --check` reported diffs. Run \
                     `cargo fmt` and re-capture."
                        .into(),
                )
            } else {
                None
            }
        }
        Capability::Clippy => log
            .lines()
            .find(|line| line.starts_with("error:") || line.starts_with("warning:"))
            .map(|line| {
                format!(
                    "Log contains a clippy diagnostic ({}). With `-D warnings` this must be \
                     resolved before evidence is accepted.",
                    line.trim()
                )
            }),
        Capability::Test => {
            if log.contains("test result: FAILED") {
                Some(
                    "Log contains `test result: FAILED`. Fix the failing tests and re-capture."
                        .into(),
                )
            } else if !log.contains("test result: ok.") {
                Some(
                    "Log is missing the `test result: ok.` summary line; the run may have been \
                     truncated or never reached the end."
                        .into(),
                )
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musts_protocol::{EvidenceSubmission, EvidenceTaskRef, ResolveCheck, SnapshotHandle};

    fn resolve_req(capability: &str, checks: Vec<ResolveCheck>) -> ResolveRequest {
        ResolveRequest {
            protocol_version: PROTOCOL_VERSION,
            workspace_root: "/repo".into(),
            capability: capability.into(),
            changed_files: Vec::new(),
            checks,
            snapshot: SnapshotHandle {
                handle: "h".into(),
                dirty_scopes: Vec::new(),
            },
        }
    }

    fn check(scope: &str, local: &str) -> ResolveCheck {
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
            depth: 0,
            with_payload: serde_json::json!({}),
        }
    }

    #[test]
    fn resolve_emits_one_task_per_check() {
        let resp = resolve(&resolve_req("cargo/test", vec![check("root", "test")])).unwrap();
        assert_eq!(resp.tasks.len(), 1);
        assert_eq!(resp.tasks[0].id, "cargo-test-root");
        assert_eq!(resp.tasks[0].satisfies, vec!["root/test"]);
        assert!(resp.ignored_checks.is_empty());
    }

    #[test]
    fn resolve_dispatches_per_capability() {
        for (cap, slug) in [
            ("cargo/fmt", "fmt"),
            ("cargo/clippy", "clippy"),
            ("cargo/test", "test"),
        ] {
            let resp = resolve(&resolve_req(cap, vec![check("root", slug)])).unwrap();
            assert_eq!(resp.tasks[0].id, format!("cargo-{slug}-root"));
            assert_eq!(resp.tasks[0].extension, cap);
        }
    }

    #[test]
    fn resolve_rejects_unknown_capability() {
        let err = resolve(&resolve_req("cargo/audit", vec![])).unwrap_err();
        assert!(format!("{err}").contains("cargo/audit"));
    }

    #[test]
    fn resolve_ignores_checks_with_unexpected_with_payload() {
        let mut c = check("root", "fmt");
        c.with_payload = serde_json::json!({ "stray": true });
        let resp = resolve(&resolve_req("cargo/fmt", vec![c])).unwrap();
        assert!(resp.tasks.is_empty());
        assert_eq!(resp.ignored_checks.len(), 1);
        assert!(resp.ignored_checks[0].reason.contains("no `with`"));
    }

    fn evidence_req(
        capability: &str,
        text: Option<&str>,
        assets: Vec<EvidenceAsset>,
        workspace_root: &str,
    ) -> EvidenceValidationRequest {
        EvidenceValidationRequest {
            protocol_version: PROTOCOL_VERSION,
            workspace_root: workspace_root.into(),
            task: EvidenceTaskRef {
                id: format!(
                    "cargo-{}-root",
                    capability.strip_prefix("cargo/").unwrap_or(capability)
                ),
                extension: capability.into(),
                satisfies: vec!["root/x".into()],
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

    fn write_log(contents: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cmd.log");
        std::fs::write(&path, contents).expect("write log");
        let rel = path
            .strip_prefix(dir.path())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        (dir, rel)
    }

    fn asset(path: &str, size: u64) -> EvidenceAsset {
        EvidenceAsset {
            path: path.into(),
            mime: "text/plain".into(),
            size,
        }
    }

    #[test]
    fn evidence_fmt_accepts_clean_log() {
        let (dir, rel) = write_log("");
        let resp = evidence(&evidence_req(
            "cargo/fmt",
            Some("cargo fmt --check sin diffs"),
            vec![asset(&rel, 1)],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(resp.accepted, "expected accept, got {resp:?}");
        assert_eq!(resp.satisfies, vec!["root/x"]);
    }

    #[test]
    fn evidence_fmt_rejects_diff_in_marker() {
        let (dir, rel) = write_log("Diff in /repo/src/main.rs at line 12:\n-foo\n+bar\n");
        let resp = evidence(&evidence_req(
            "cargo/fmt",
            Some("ran fmt"),
            vec![asset(&rel, 64)],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp.missing.iter().any(|m| m.message.contains("Diff in")));
    }

    #[test]
    fn evidence_clippy_accepts_clean_log() {
        let (dir, rel) = write_log("    Checking foo v0.1.0\n    Finished dev profile in 1.23s\n");
        let resp = evidence(&evidence_req(
            "cargo/clippy",
            Some("clippy clean"),
            vec![asset(&rel, 64)],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(resp.accepted);
    }

    #[test]
    fn evidence_clippy_rejects_warning_line() {
        let (dir, rel) = write_log("warning: unused variable `x`\n  --> src/lib.rs:3:5\n");
        let resp = evidence(&evidence_req(
            "cargo/clippy",
            Some("ran clippy"),
            vec![asset(&rel, 64)],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp
            .missing
            .iter()
            .any(|m| m.message.contains("warning: unused variable")));
    }

    #[test]
    fn evidence_clippy_rejects_error_line() {
        let (dir, rel) = write_log("error: redundant clone\n");
        let resp = evidence(&evidence_req(
            "cargo/clippy",
            Some("ran clippy"),
            vec![asset(&rel, 32)],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(!resp.accepted);
    }

    #[test]
    fn evidence_test_accepts_ok_summary() {
        let (dir, rel) = write_log("running 5 tests\ntest result: ok. 5 passed; 0 failed\n");
        let resp = evidence(&evidence_req(
            "cargo/test",
            Some("all green"),
            vec![asset(&rel, 64)],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(resp.accepted);
    }

    #[test]
    fn evidence_test_rejects_failed_summary() {
        let (dir, rel) =
            write_log("test foo ... FAILED\ntest result: FAILED. 4 passed; 1 failed\n");
        let resp = evidence(&evidence_req(
            "cargo/test",
            Some("oops"),
            vec![asset(&rel, 64)],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp.missing.iter().any(|m| m.message.contains("FAILED")));
    }

    #[test]
    fn evidence_test_rejects_missing_summary() {
        let (dir, rel) = write_log("running 1 test\n... output truncated\n");
        let resp = evidence(&evidence_req(
            "cargo/test",
            Some("partial"),
            vec![asset(&rel, 64)],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp
            .missing
            .iter()
            .any(|m| m.message.contains("test result: ok.")));
    }

    #[test]
    fn evidence_rejects_empty_text() {
        let (dir, rel) = write_log("test result: ok. 1 passed\n");
        let resp = evidence(&evidence_req(
            "cargo/test",
            Some("   "),
            vec![asset(&rel, 32)],
            dir.path().to_str().unwrap(),
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp.missing.iter().any(|m| m.kind == "text"));
    }

    #[test]
    fn evidence_rejects_missing_log() {
        let resp = evidence(&evidence_req("cargo/test", Some("ok"), vec![], "/repo")).unwrap();
        assert!(!resp.accepted);
        assert!(resp.missing.iter().any(|m| m.kind == "log"));
    }

    #[test]
    fn evidence_rejects_zero_byte_log() {
        let resp = evidence(&evidence_req(
            "cargo/test",
            Some("ok"),
            vec![asset("empty.log", 0)],
            "/repo",
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp.missing.iter().any(|m| m.kind == "log"));
    }

    #[test]
    fn evidence_rejects_unreadable_log_path() {
        let resp = evidence(&evidence_req(
            "cargo/test",
            Some("ok"),
            vec![asset("does/not/exist.log", 10)],
            "/repo",
        ))
        .unwrap();
        assert!(!resp.accepted);
        assert!(resp
            .missing
            .iter()
            .any(|m| m.message.contains("Could not read log")));
    }
}
