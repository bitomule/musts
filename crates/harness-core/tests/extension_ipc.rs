//! End-to-end tests for the extension IPC runtime against the real stub
//! binary. Drives every PLAN.md §7.2.1 failure mode.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use harness_core::extension::{
    descriptor::{Capability, Command, ExtensionDescriptor},
    runtime::{ExtensionRunner, RuntimeOptions, MAX_RESPONSE_BYTES},
};
use harness_core::Error;
use harness_protocol::{
    AssetContract, EvidenceContract, EvidenceSubmission, EvidenceTaskRef,
    EvidenceValidationRequest, ResolveCheck, ResolveRequest, SnapshotHandle, TextContract,
    PROTOCOL_VERSION,
};
use serial_test::serial;

/// Locate the compiled stub binary. `cargo test` produces it under the
/// workspace's `target/<profile>/` directory; we walk back from the test
/// binary's path.
fn stub_binary() -> PathBuf {
    // The test binary is `<workspace>/target/<profile>/deps/<name>-<hash>`.
    let test_bin = std::env::current_exe().expect("test binary path");
    // …/deps/<test>
    let deps_dir = test_bin.parent().expect("deps dir");
    let profile_dir = deps_dir.parent().expect("profile dir");
    let candidate = profile_dir.join("stub-extension");
    if !candidate.exists() {
        panic!(
            "stub-extension binary not found at {} — run `cargo build -p stub-extension` first \
             (cargo test --workspace usually triggers this)",
            candidate.display()
        );
    }
    candidate
}

fn descriptor_for_stub(workspace_root: PathBuf) -> ExtensionDescriptor {
    let stub = stub_binary();
    let mut capabilities = BTreeMap::new();
    capabilities.insert(
        "stub".into(),
        Capability {
            uses: "stub/cap".into(),
            schema: None,
            schema_path: None,
            resolve: Command {
                argv: vec![stub.display().to_string(), "resolve".into()],
            },
            evidence: Command {
                argv: vec![stub.display().to_string(), "evidence".into()],
            },
        },
    );
    ExtensionDescriptor {
        root: workspace_root,
        name: "stub".into(),
        version: "0.1.0".into(),
        capabilities,
        descriptor_bytes: Vec::new(),
    }
}

fn sample_resolve_request() -> ResolveRequest {
    ResolveRequest {
        protocol_version: PROTOCOL_VERSION,
        workspace_root: "/repo".into(),
        capability: "stub/cap".into(),
        changed_files: vec!["App/Login/LoginView.swift".into()],
        checks: vec![ResolveCheck {
            id: "App/Login/login-build".into(),
            local_id: "login-build".into(),
            manifest_path: "App/Login/HARNESS.yml".into(),
            scope_path: "App/Login".into(),
            depth: 2,
            with_payload: serde_json::json!({}),
        }],
        snapshot: SnapshotHandle {
            handle: "h".into(),
            dirty_scopes: vec!["App/Login".into()],
        },
    }
}

fn sample_evidence_request() -> EvidenceValidationRequest {
    EvidenceValidationRequest {
        protocol_version: PROTOCOL_VERSION,
        workspace_root: "/repo".into(),
        task: EvidenceTaskRef {
            id: "stub-task".into(),
            extension: "stub/cap".into(),
            satisfies: vec![
                "App/Login/login-build".into(),
                "App/Login/login-flow".into(),
            ],
            evidence_contract: EvidenceContract {
                text: TextContract {
                    required: true,
                    description: None,
                },
                assets: vec![AssetContract {
                    kind: "log".into(),
                    required: false,
                    description: None,
                }],
            },
        },
        submission: EvidenceSubmission {
            text: Some("text".into()),
            assets: vec![],
        },
        snapshot: SnapshotHandle {
            handle: "h".into(),
            dirty_scopes: vec![],
        },
    }
}

fn runner_with(descriptor: &ExtensionDescriptor, timeout: Duration) -> ExtensionRunner<'_> {
    ExtensionRunner {
        capability: "stub/cap".into(),
        descriptor_root: &descriptor.root,
        options: RuntimeOptions {
            timeout,
            max_response_bytes: MAX_RESPONSE_BYTES,
            workspace_root: descriptor.root.clone(),
        },
    }
}

// ---------------------------------------------------------------------------
// Resolve-side scenarios
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn resolve_round_trip_default_shape() {
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = descriptor_for_stub(workspace.path().to_path_buf());
    let cap = &descriptor.capabilities["stub"];
    let runner = runner_with(&descriptor, Duration::from_secs(10));

    let response = runner
        .resolve(&cap.resolve, &sample_resolve_request())
        .unwrap();
    assert_eq!(response.protocol_version, PROTOCOL_VERSION);
    assert_eq!(response.tasks.len(), 1);
    assert_eq!(response.tasks[0].id, "stub-task");
    assert_eq!(response.tasks[0].satisfies, vec!["App/Login/login-build"]);
}

#[test]
#[serial]
fn resolve_ignore_all_shape() {
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = descriptor_for_stub(workspace.path().to_path_buf());
    let cap = &descriptor.capabilities["stub"];
    let runner = runner_with(&descriptor, Duration::from_secs(10));

    std::env::set_var("HARNESS_STUB_RESOLVE_SHAPE", "ignore_all");
    let response = runner
        .resolve(&cap.resolve, &sample_resolve_request())
        .unwrap();
    std::env::remove_var("HARNESS_STUB_RESOLVE_SHAPE");
    assert!(response.tasks.is_empty());
    assert_eq!(response.ignored_checks.len(), 1);
    assert_eq!(response.ignored_checks[0].id, "App/Login/login-build");
}

#[test]
#[serial]
fn resolve_timeout_is_rejected() {
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = descriptor_for_stub(workspace.path().to_path_buf());
    let cap = &descriptor.capabilities["stub"];
    let runner = runner_with(&descriptor, Duration::from_millis(150));

    std::env::set_var("HARNESS_STUB_RESOLVE_MODE", "timeout");
    let err = runner
        .resolve(&cap.resolve, &sample_resolve_request())
        .unwrap_err();
    std::env::remove_var("HARNESS_STUB_RESOLVE_MODE");
    assert!(
        matches!(err, Error::ExtensionTimeout { .. }),
        "expected timeout, got: {err}"
    );
    assert_eq!(err.exit_code(), 2);
}

#[test]
#[serial]
fn resolve_garbage_stdout_is_rejected() {
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = descriptor_for_stub(workspace.path().to_path_buf());
    let cap = &descriptor.capabilities["stub"];
    let runner = runner_with(&descriptor, Duration::from_secs(5));

    std::env::set_var("HARNESS_STUB_RESOLVE_MODE", "garbage");
    let err = runner
        .resolve(&cap.resolve, &sample_resolve_request())
        .unwrap_err();
    std::env::remove_var("HARNESS_STUB_RESOLVE_MODE");
    let message = format!("{err}");
    assert!(
        message.contains("not valid JSON") || message.contains("data after"),
        "unexpected: {message}"
    );
}

#[test]
#[serial]
fn resolve_oversized_response_is_rejected() {
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = descriptor_for_stub(workspace.path().to_path_buf());
    let cap = &descriptor.capabilities["stub"];
    let runner = runner_with(&descriptor, Duration::from_secs(10));

    std::env::set_var("HARNESS_STUB_RESOLVE_MODE", "oversized");
    let err = runner
        .resolve(&cap.resolve, &sample_resolve_request())
        .unwrap_err();
    std::env::remove_var("HARNESS_STUB_RESOLVE_MODE");
    let message = format!("{err}");
    assert!(message.contains("exceeds"), "unexpected: {message}");
}

#[test]
#[serial]
fn resolve_nonzero_exit_is_surfaced_with_stderr() {
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = descriptor_for_stub(workspace.path().to_path_buf());
    let cap = &descriptor.capabilities["stub"];
    let runner = runner_with(&descriptor, Duration::from_secs(5));

    std::env::set_var("HARNESS_STUB_RESOLVE_MODE", "nonzero_exit");
    let err = runner
        .resolve(&cap.resolve, &sample_resolve_request())
        .unwrap_err();
    std::env::remove_var("HARNESS_STUB_RESOLVE_MODE");
    let message = format!("{err}");
    assert!(
        message.contains("simulated failure"),
        "stderr was not surfaced: {message}"
    );
}

#[test]
#[serial]
fn resolve_bad_protocol_version_is_rejected() {
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = descriptor_for_stub(workspace.path().to_path_buf());
    let cap = &descriptor.capabilities["stub"];
    let runner = runner_with(&descriptor, Duration::from_secs(5));

    std::env::set_var("HARNESS_STUB_RESOLVE_MODE", "bad_protocol_version");
    let err = runner
        .resolve(&cap.resolve, &sample_resolve_request())
        .unwrap_err();
    std::env::remove_var("HARNESS_STUB_RESOLVE_MODE");
    let message = format!("{err}");
    assert!(
        message.contains("protocol_version"),
        "unexpected: {message}"
    );
}

// ---------------------------------------------------------------------------
// Evidence-side scenarios
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn evidence_accept_all_round_trip() {
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = descriptor_for_stub(workspace.path().to_path_buf());
    let cap = &descriptor.capabilities["stub"];
    let runner = runner_with(&descriptor, Duration::from_secs(5));

    let response = runner
        .evidence(&cap.evidence, &sample_evidence_request())
        .unwrap();
    assert!(response.accepted);
    assert_eq!(response.satisfies.len(), 2);
}

#[test]
#[serial]
fn evidence_accept_subset() {
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = descriptor_for_stub(workspace.path().to_path_buf());
    let cap = &descriptor.capabilities["stub"];
    let runner = runner_with(&descriptor, Duration::from_secs(5));

    std::env::set_var("HARNESS_STUB_EVIDENCE_SHAPE", "accept_subset");
    let response = runner
        .evidence(&cap.evidence, &sample_evidence_request())
        .unwrap();
    std::env::remove_var("HARNESS_STUB_EVIDENCE_SHAPE");
    assert!(response.accepted);
    assert_eq!(response.satisfies, vec!["App/Login/login-build"]);
}

#[test]
#[serial]
fn evidence_reject_shape() {
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = descriptor_for_stub(workspace.path().to_path_buf());
    let cap = &descriptor.capabilities["stub"];
    let runner = runner_with(&descriptor, Duration::from_secs(5));

    std::env::set_var("HARNESS_STUB_EVIDENCE_SHAPE", "reject");
    let response = runner
        .evidence(&cap.evidence, &sample_evidence_request())
        .unwrap();
    std::env::remove_var("HARNESS_STUB_EVIDENCE_SHAPE");
    assert!(!response.accepted);
    assert_eq!(response.missing.len(), 1);
}

#[test]
#[serial]
fn evidence_overclaim_returns_extra_satisfies() {
    // The runtime itself does not reject overclaims — that's the
    // ledger's job in Phase 4. We just confirm we round-tripped the
    // shape so the ledger has the info to reject.
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = descriptor_for_stub(workspace.path().to_path_buf());
    let cap = &descriptor.capabilities["stub"];
    let runner = runner_with(&descriptor, Duration::from_secs(5));

    std::env::set_var("HARNESS_STUB_EVIDENCE_SHAPE", "overclaim");
    let response = runner
        .evidence(&cap.evidence, &sample_evidence_request())
        .unwrap();
    std::env::remove_var("HARNESS_STUB_EVIDENCE_SHAPE");
    assert!(response
        .satisfies
        .contains(&"stub/unrelated-check".to_string()));
}

#[test]
#[serial]
fn evidence_timeout_is_rejected() {
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = descriptor_for_stub(workspace.path().to_path_buf());
    let cap = &descriptor.capabilities["stub"];
    let runner = runner_with(&descriptor, Duration::from_millis(150));

    std::env::set_var("HARNESS_STUB_EVIDENCE_MODE", "timeout");
    let err = runner
        .evidence(&cap.evidence, &sample_evidence_request())
        .unwrap_err();
    std::env::remove_var("HARNESS_STUB_EVIDENCE_MODE");
    assert!(matches!(err, Error::ExtensionTimeout { .. }));
}

#[test]
#[serial]
fn evidence_bad_protocol_version_is_rejected() {
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = descriptor_for_stub(workspace.path().to_path_buf());
    let cap = &descriptor.capabilities["stub"];
    let runner = runner_with(&descriptor, Duration::from_secs(5));

    std::env::set_var("HARNESS_STUB_EVIDENCE_MODE", "bad_protocol_version");
    let err = runner
        .evidence(&cap.evidence, &sample_evidence_request())
        .unwrap_err();
    std::env::remove_var("HARNESS_STUB_EVIDENCE_MODE");
    let message = format!("{err}");
    assert!(
        message.contains("protocol_version"),
        "unexpected: {message}"
    );
}

// ---------------------------------------------------------------------------
// Deadlock test: stdin must be closed before reading stdout
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn stdin_is_closed_before_reading_stdout() {
    // The stub uses `read_to_end(stdin())` which blocks until EOF. If
    // core failed to close stdin, this test would hang at the runtime's
    // timeout. We use a short timeout to fail fast if the regression
    // re-appears.
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = descriptor_for_stub(workspace.path().to_path_buf());
    let cap = &descriptor.capabilities["stub"];
    let runner = runner_with(&descriptor, Duration::from_secs(3));

    let response = runner
        .resolve(&cap.resolve, &sample_resolve_request())
        .unwrap();
    assert_eq!(response.protocol_version, PROTOCOL_VERSION);
}
