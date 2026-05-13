//! Configurable test extension. Behaviour is selected via env vars; see
//! `docs/PLAN.md` §7.2.1 for the full matrix.
//!
//! Usage: `stub-extension (resolve|evidence)`.
//!
//! - stdin: the corresponding protocol request JSON.
//! - stdout: the response (unless the failure-mode says otherwise).
//! - exit code 0 on success, anything else on `nonzero_exit`.

use std::io::{Read, Write};
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use musts_protocol::{
    AssetContract, EvidenceContract, EvidenceValidationRequest, EvidenceValidationResponse,
    IgnoredCheck, MissingEvidence, NormalizedAsset, ResolveRequest, ResolveResponse, Task,
    TextContract, PROTOCOL_VERSION,
};

fn main() -> ExitCode {
    let kind = std::env::args().nth(1).unwrap_or_default();
    // Read stdin so the parent's write_all completes deterministically.
    let mut input = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut input);

    let delay_ms: u64 = std::env::var("MUSTS_STUB_DELAY_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if delay_ms > 0 {
        thread::sleep(Duration::from_millis(delay_ms));
    }

    match kind.as_str() {
        "resolve" => handle_resolve(&input),
        "evidence" => handle_evidence(&input),
        other => {
            eprintln!("stub-extension: unknown mode `{other}`");
            ExitCode::from(2)
        }
    }
}

fn handle_resolve(input: &[u8]) -> ExitCode {
    let mode = std::env::var("MUSTS_STUB_RESOLVE_MODE").unwrap_or_else(|_| "ok".into());
    match mode.as_str() {
        "ok" => emit_resolve_ok(input),
        "timeout" => {
            // Diagnostic on stderr so a hung extension still surfaces a
            // hint when core surfaces stderr per PLAN.md §4.6.
            eprintln!("stub-extension: resolve timeout mode — sleeping until killed");
            thread::sleep(Duration::from_secs(300));
            ExitCode::from(0)
        }
        "garbage" => {
            eprintln!("stub-extension: resolve garbage mode — emitting non-JSON stdout");
            let _ = std::io::stdout().write_all(b"this is not JSON at all\n");
            ExitCode::from(0)
        }
        "oversized" => {
            eprintln!("stub-extension: resolve oversized mode — emitting >cap payload");
            emit_padded_resolve(input)
        }
        "nonzero_exit" => {
            eprintln!("stub-extension: simulated failure");
            ExitCode::from(7)
        }
        "bad_protocol_version" => {
            eprintln!("stub-extension: resolve bad_protocol_version mode");
            emit_resolve_with_version(input, 9999)
        }
        other => {
            eprintln!("stub-extension: unknown MUSTS_STUB_RESOLVE_MODE `{other}`");
            ExitCode::from(2)
        }
    }
}

fn handle_evidence(input: &[u8]) -> ExitCode {
    let mode = std::env::var("MUSTS_STUB_EVIDENCE_MODE").unwrap_or_else(|_| "ok".into());
    match mode.as_str() {
        "ok" => emit_evidence_ok(input),
        "timeout" => {
            eprintln!("stub-extension: evidence timeout mode — sleeping until killed");
            thread::sleep(Duration::from_secs(300));
            ExitCode::from(0)
        }
        "garbage" => {
            eprintln!("stub-extension: evidence garbage mode — emitting non-JSON stdout");
            let _ = std::io::stdout().write_all(b"not json\n");
            ExitCode::from(0)
        }
        "oversized" => {
            eprintln!("stub-extension: evidence oversized mode — emitting >cap payload");
            emit_padded_evidence(input)
        }
        "nonzero_exit" => {
            eprintln!("stub-extension: simulated evidence failure");
            ExitCode::from(11)
        }
        "bad_protocol_version" => {
            eprintln!("stub-extension: evidence bad_protocol_version mode");
            emit_evidence_with_version(input, 9999)
        }
        other => {
            eprintln!("stub-extension: unknown MUSTS_STUB_EVIDENCE_MODE `{other}`");
            ExitCode::from(2)
        }
    }
}

fn emit_resolve_ok(input: &[u8]) -> ExitCode {
    let request: ResolveRequest = match serde_json::from_slice(input) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("stub-extension: bad resolve request: {err}");
            return ExitCode::from(2);
        }
    };

    let shape = std::env::var("MUSTS_STUB_RESOLVE_SHAPE").unwrap_or_else(|_| "default".into());
    let response = match shape.as_str() {
        "empty" => ResolveResponse {
            protocol_version: PROTOCOL_VERSION,
            tasks: vec![],
            ignored_checks: vec![],
            notes: vec![],
        },
        "ignore_all" => ResolveResponse {
            protocol_version: PROTOCOL_VERSION,
            tasks: vec![],
            ignored_checks: request
                .checks
                .iter()
                .map(|c| IgnoredCheck {
                    id: c.id.clone(),
                    reason: "stub: ignore_all mode".into(),
                })
                .collect(),
            notes: vec![],
        },
        "multi_task" => {
            // One task per check, with single satisfies — for tests that
            // assert per-check task generation.
            let tasks = request
                .checks
                .iter()
                .enumerate()
                .map(|(i, c)| Task {
                    id: format!("stub-task-{i}"),
                    extension: request.capability.clone(),
                    title: format!("Stub task for {}", c.id),
                    satisfies: vec![c.id.clone()],
                    parallelizable: true,
                    instructions: vec![format!("Pretend to validate {}.", c.id)],
                    evidence_contract: default_evidence_contract(),
                })
                .collect();
            ResolveResponse {
                protocol_version: PROTOCOL_VERSION,
                tasks,
                ignored_checks: vec![],
                notes: vec!["stub: multi_task mode".into()],
            }
        }
        _ => {
            // "default": one task satisfying all dirty checks.
            let satisfies: Vec<_> = request.checks.iter().map(|c| c.id.clone()).collect();
            let title = format!(
                "Stub task ({} check{})",
                satisfies.len(),
                if satisfies.len() == 1 { "" } else { "s" }
            );
            ResolveResponse {
                protocol_version: PROTOCOL_VERSION,
                tasks: vec![Task {
                    id: "stub-task".into(),
                    extension: request.capability.clone(),
                    title,
                    satisfies,
                    parallelizable: true,
                    instructions: vec!["Stub task: no real work required.".into()],
                    evidence_contract: default_evidence_contract(),
                }],
                ignored_checks: vec![],
                notes: vec![],
            }
        }
    };
    write_json(&response)
}

fn emit_evidence_ok(input: &[u8]) -> ExitCode {
    let request: EvidenceValidationRequest = match serde_json::from_slice(input) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("stub-extension: bad evidence request: {err}");
            return ExitCode::from(2);
        }
    };
    let shape =
        std::env::var("MUSTS_STUB_EVIDENCE_SHAPE").unwrap_or_else(|_| "accept_all".into());
    let response = match shape.as_str() {
        "accept_subset" => EvidenceValidationResponse {
            protocol_version: PROTOCOL_VERSION,
            accepted: true,
            // Only the first check in the task's satisfies set.
            satisfies: request
                .task
                .satisfies
                .first()
                .cloned()
                .map(|s| vec![s])
                .unwrap_or_default(),
            summary: Some("stub: accepted subset".into()),
            normalized_assets: assets_passthrough(&request),
            missing: vec![],
            message: None,
        },
        "reject" => EvidenceValidationResponse {
            protocol_version: PROTOCOL_VERSION,
            accepted: false,
            satisfies: vec![],
            summary: None,
            normalized_assets: vec![],
            missing: vec![MissingEvidence {
                kind: "stub".into(),
                message: "stub: simulated rejection".into(),
            }],
            message: Some("Evidence rejected by stub.".into()),
        },
        "overclaim" => EvidenceValidationResponse {
            protocol_version: PROTOCOL_VERSION,
            accepted: true,
            // Claim an extra id that the task does not have.
            satisfies: {
                let mut v = request.task.satisfies.clone();
                v.push("stub/unrelated-check".into());
                v
            },
            summary: Some("stub: deliberate over-claim".into()),
            normalized_assets: vec![],
            missing: vec![],
            message: None,
        },
        _ => EvidenceValidationResponse {
            protocol_version: PROTOCOL_VERSION,
            accepted: true,
            satisfies: request.task.satisfies.clone(),
            summary: Some("stub: accepted".into()),
            normalized_assets: assets_passthrough(&request),
            missing: vec![],
            message: None,
        },
    };
    write_json(&response)
}

fn emit_padded_resolve(input: &[u8]) -> ExitCode {
    let mut response = match serde_json::from_slice::<ResolveRequest>(input) {
        Ok(_) => ResolveResponse {
            protocol_version: PROTOCOL_VERSION,
            tasks: vec![],
            ignored_checks: vec![],
            notes: vec![],
        },
        Err(_) => return ExitCode::from(2),
    };
    response.notes = vec![pad_string()];
    write_json(&response)
}

fn emit_padded_evidence(input: &[u8]) -> ExitCode {
    let request: EvidenceValidationRequest = match serde_json::from_slice(input) {
        Ok(r) => r,
        Err(_) => return ExitCode::from(2),
    };
    let response = EvidenceValidationResponse {
        protocol_version: PROTOCOL_VERSION,
        accepted: true,
        satisfies: request.task.satisfies.clone(),
        summary: Some(pad_string()),
        normalized_assets: vec![],
        missing: vec![],
        message: None,
    };
    write_json(&response)
}

fn emit_resolve_with_version(input: &[u8], version: u32) -> ExitCode {
    let _ = serde_json::from_slice::<ResolveRequest>(input);
    let response = ResolveResponse {
        protocol_version: version,
        tasks: vec![],
        ignored_checks: vec![],
        notes: vec![],
    };
    write_json(&response)
}

fn emit_evidence_with_version(input: &[u8], version: u32) -> ExitCode {
    let _ = serde_json::from_slice::<EvidenceValidationRequest>(input);
    let response = EvidenceValidationResponse {
        protocol_version: version,
        accepted: true,
        satisfies: vec![],
        summary: None,
        normalized_assets: vec![],
        missing: vec![],
        message: None,
    };
    write_json(&response)
}

fn default_evidence_contract() -> EvidenceContract {
    EvidenceContract {
        text: TextContract {
            required: true,
            description: None,
        },
        assets: vec![AssetContract {
            kind: "log".into(),
            required: false,
            description: None,
        }],
    }
}

fn assets_passthrough(req: &EvidenceValidationRequest) -> Vec<NormalizedAsset> {
    req.submission
        .assets
        .iter()
        .enumerate()
        .map(|(i, a)| NormalizedAsset {
            kind: format!("asset-{i}"),
            path: a.path.clone(),
        })
        .collect()
}

fn pad_string() -> String {
    // 5 MiB: exceeds the 4 MiB cap regardless of surrounding JSON.
    let bytes_env = std::env::var("MUSTS_STUB_RESPONSE_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(5 * 1024 * 1024);
    "X".repeat(bytes_env)
}

fn write_json<T: serde::Serialize>(value: &T) -> ExitCode {
    match serde_json::to_writer(std::io::stdout(), value) {
        Ok(()) => ExitCode::from(0),
        Err(err) => {
            eprintln!("stub-extension: serialise failed: {err}");
            ExitCode::from(2)
        }
    }
}
