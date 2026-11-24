//! `musts evidence` pipeline per `docs/PLAN.md` §4.2.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use musts_protocol::{
    EvidenceSubmission, EvidenceTaskRef, EvidenceValidationRequest, SnapshotHandle, Task,
    PROTOCOL_VERSION,
};

use crate::bootstrap::StateSession;
use crate::error::{Error, Result};
use crate::evidence::store::{describe_asset, new_submission_id};
use crate::extension::descriptor::{discover_descriptors, Capability, ExtensionDescriptor};
use crate::extension::runtime::{ExtensionRunner, RuntimeOptions};
use crate::state::cache;
use crate::validate::compute_current_scope_hashes;

/// Outcome of a successful evidence submission.
#[derive(Debug)]
pub struct EvidenceSubmissionResult {
    pub task_id: String,
    pub submission_id: String,
    /// check_ids the extension marked satisfied.
    pub satisfied: Vec<String>,
    pub summary: Option<String>,
}

/// One asset path supplied by the user, plus the optional text body.
pub struct SubmissionInputs<'a> {
    pub task_id: &'a str,
    pub text: Option<&'a str>,
    pub asset_paths: &'a [&'a Path],
}

/// Drive a full evidence submission. The CLI builds `inputs` from
/// `musts evidence <task-id> --text "…" --asset PATH …` and calls
/// this; tests do likewise.
pub fn submit(
    session: &mut StateSession,
    workspace_root: &Path,
    runtime_options: &RuntimeOptions,
    inputs: &SubmissionInputs<'_>,
) -> Result<EvidenceSubmissionResult> {
    // 1. Look up the persisted task.
    let stored = cache::fetch_task(&session.musts_dir, inputs.task_id).ok_or_else(|| {
        Error::TaskNotFound {
            task_id: inputs.task_id.to_string(),
        }
    })?;
    let stored_satisfies = stored.satisfies;
    let stored_scope_hashes = stored.scope_hashes;
    let stored_task: Task = stored.task;
    let capability = stored.capability;
    let stored_task_snapshot_hash = stored.task_snapshot_hash;

    // 2. Recompute scope hashes and confirm none have drifted for any
    //    `satisfies` check of this task. Only the task's covered scopes
    //    matter — unrelated edits do not stale the evidence.
    let current_hashes = compute_current_scope_hashes(session, workspace_root)?;
    let current_task_snapshot_hash =
        recompute_task_snapshot_hash(&stored_satisfies, &current_hashes);
    if current_task_snapshot_hash != stored_task_snapshot_hash {
        return Err(Error::EvidenceStale {
            task_id: inputs.task_id.to_string(),
        });
    }

    // 3. Describe each asset in place — evidence is no longer archived into
    //    `.musts/evidence/`; the committed ledger is the durable record, and the asset is validated where it
    //    lives. `describe_asset` returns an absolute path so the capability
    //    validator resolves it via `workspace_root.join(path)`.
    let submission_id = new_submission_id();
    let mut wire_assets = Vec::with_capacity(inputs.asset_paths.len());
    for path in inputs.asset_paths {
        wire_assets.push(describe_asset(path)?);
    }

    // 4. Locate the implementor. Built-in capabilities are checked
    //    after external descriptors so a workspace can override (or
    //    just shadow) a built-in by shipping its own extension. Either
    //    path produces an `EvidenceValidationResponse`.
    let descriptors = discover_descriptors(workspace_root)?;
    let external = find_capability(&descriptors, &capability);
    let builtin = if external.is_none() {
        crate::builtin::lookup(&capability)
    } else {
        None
    };
    if external.is_none() && builtin.is_none() {
        return Err(Error::MissingExtension {
            manifest_path: std::path::PathBuf::from("(persisted task)"),
            check_id: inputs.task_id.to_string(),
            capability: capability.clone(),
            available: crate::builtin::registered_capabilities()
                .collect::<Vec<_>>()
                .join(", "),
        });
    }

    // 5. Build the IPC request and call the validator.
    let submission = EvidenceSubmission {
        text: inputs.text.map(|s| s.to_string()),
        assets: wire_assets,
    };
    let request = EvidenceValidationRequest {
        protocol_version: PROTOCOL_VERSION,
        workspace_root: workspace_root.display().to_string(),
        task: EvidenceTaskRef {
            id: stored_task.id.clone(),
            extension: stored_task.extension.clone(),
            satisfies: stored_satisfies.clone(),
            evidence_contract: stored_task.evidence_contract.clone(),
        },
        submission: submission.clone(),
        snapshot: SnapshotHandle {
            handle: format!("v1:{capability}"),
            dirty_scopes: Vec::new(),
        },
    };
    let response = if let Some((descriptor, cap)) = external {
        let runner = ExtensionRunner {
            capability: capability.clone(),
            descriptor_root: &descriptor.root,
            options: runtime_options.clone(),
        };
        runner.evidence(&cap.evidence, &request)?
    } else {
        (builtin.expect("checked above").evidence)(&request)?
    };

    // 6. Reject when the extension says so. The CLI surfaces both the
    //    extension's freeform `message` and the structured `missing`
    //    list so the agent can act on either.
    if !response.accepted {
        let mut parts = Vec::new();
        if let Some(m) = response.message.clone() {
            parts.push(m);
        }
        if !response.missing.is_empty() {
            parts.push(missing_evidence_message(&response.missing));
        }
        let message = if parts.is_empty() {
            "rejected by extension".to_string()
        } else {
            parts.join(" — ")
        };
        record_rejection(
            session,
            inputs.task_id,
            &stored_satisfies,
            &stored_scope_hashes,
            &submission,
        );
        return Err(Error::EvidenceRejected {
            task_id: inputs.task_id.to_string(),
            capability: capability.clone(),
            message,
        });
    }

    // 7. Honour the partial-accept rule and reject over-claims (PLAN.md §4.2).
    let stored_set: BTreeSet<&str> = stored_satisfies.iter().map(|s| s.as_str()).collect();
    let mut unexpected = Vec::new();
    let mut accepted_now = Vec::new();
    for id in &response.satisfies {
        if stored_set.contains(id.as_str()) {
            accepted_now.push(id.clone());
        } else {
            unexpected.push(id.clone());
        }
    }
    if !unexpected.is_empty() {
        return Err(Error::EvidenceOverclaim {
            task_id: inputs.task_id.to_string(),
            capability: capability.clone(),
            unexpected,
        });
    }

    // 8. Record one ledger entry per accepted-now check, keyed by that
    //    check's declaring-manifest scope_hash.
    let accepted: Vec<(String, String)> = accepted_now
        .iter()
        .map(|cid| {
            let hash = stored_scope_hashes.get(cid).cloned().unwrap_or_default();
            (cid.clone(), hash)
        })
        .collect();
    persist_accepted(
        session,
        &AcceptedEvidence {
            task_id: inputs.task_id,
            checks: &accepted,
            text: submission.text.as_deref(),
        },
    )?;

    Ok(EvidenceSubmissionResult {
        task_id: inputs.task_id.to_string(),
        submission_id,
        satisfied: accepted_now,
        summary: response.summary,
    })
}

/// Accepted evidence for one or more checks of a task, ready to persist.
pub(crate) struct AcceptedEvidence<'a> {
    pub task_id: &'a str,
    /// `(check_id, scope_hash)` of every check the evidence proves.
    pub checks: &'a [(String, String)],
    pub text: Option<&'a str>,
}

/// Write each newly-green `(check_id, scope_hash)` pair to the committed
/// ledger (one file per check), then note the submission in the local
/// history `musts admin stats` reads.
pub(crate) fn persist_accepted(
    session: &mut StateSession,
    evidence: &AcceptedEvidence<'_>,
) -> Result<()> {
    for (cid, scope_hash) in evidence.checks {
        crate::state::lock::record(&session.musts_dir, cid, scope_hash)?;
    }
    record_history(
        session,
        evidence.task_id,
        evidence
            .checks
            .iter()
            .map(|(c, h)| (c.as_str(), h.as_str())),
        true,
        evidence.text,
    );
    Ok(())
}

/// Note a rejected submission in the local history.
///
/// Rejections used to be dropped on the floor, which made "has this check
/// ever actually failed?" unanswerable — and that question is the whole
/// point of `musts admin stats`. A check that reopens on every commit and has
/// never once rejected evidence is cost without safety value, and there
/// was no record to prove it either way.
fn record_rejection(
    session: &mut StateSession,
    task_id: &str,
    stored_satisfies: &[String],
    stored_scope_hashes: &BTreeMap<String, String>,
    submission: &EvidenceSubmission,
) {
    let checks = stored_satisfies.iter().map(|cid| {
        (
            cid.as_str(),
            stored_scope_hashes.get(cid).map_or("", String::as_str),
        )
    });
    record_history(session, task_id, checks, false, submission.text.as_deref());
}

/// History is strictly best-effort: failing to write it must never change
/// the outcome the agent sees. Worst case a submission goes uncounted.
fn record_history<'a>(
    session: &StateSession,
    task_id: &str,
    checks: impl Iterator<Item = (&'a str, &'a str)>,
    accepted: bool,
    text: Option<&str>,
) {
    let submitted_at = unix_seconds_now();
    let text_chars = text.map(|t| t.chars().count());
    let rows: Vec<cache::Submission> = checks
        .map(|(check_id, scope_hash)| cache::Submission {
            check_id: check_id.to_string(),
            scope_hash: scope_hash.to_string(),
            accepted,
            text_chars,
            submitted_at,
        })
        .collect();
    if let Err(err) = cache::append_history(&session.musts_dir, &rows) {
        tracing::debug!(%task_id, %err, "could not record the submission in the local history");
    }
}

fn missing_evidence_message(missing: &[musts_protocol::MissingEvidence]) -> String {
    if missing.is_empty() {
        return "rejected with no further detail".into();
    }
    let mut s = String::from("missing: ");
    for (i, m) in missing.iter().enumerate() {
        if i > 0 {
            s.push_str("; ");
        }
        s.push_str(&format!("{} — {}", m.kind, m.message));
    }
    s
}

fn recompute_task_snapshot_hash(
    satisfies: &[String],
    current_hashes: &BTreeMap<String, String>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    let pairs: BTreeMap<&str, &str> = satisfies
        .iter()
        .map(|c| {
            let h = current_hashes.get(c).map(|s| s.as_str()).unwrap_or("");
            (c.as_str(), h)
        })
        .collect();
    for (cid, h) in &pairs {
        hasher.update(cid.as_bytes());
        hasher.update(b"\0");
        hasher.update(h.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

/// Locate the capability that implements `uses` across loaded descriptors.
/// We do not preserve the order of descriptors here — multiple
/// implementors of the same capability would be flagged at validate time
/// anyway.
fn find_capability<'a>(
    descriptors: &'a [ExtensionDescriptor],
    uses: &str,
) -> Option<(&'a ExtensionDescriptor, &'a Capability)> {
    for d in descriptors {
        for c in d.capabilities.values() {
            if c.uses == uses {
                return Some((d, c));
            }
        }
    }
    None
}

fn unix_seconds_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn recompute_task_snapshot_hash_is_order_independent() {
        let mut hashes = BTreeMap::new();
        hashes.insert("a".to_string(), "h1".to_string());
        hashes.insert("b".to_string(), "h2".to_string());
        let ab = recompute_task_snapshot_hash(&["a".into(), "b".into()], &hashes);
        let ba = recompute_task_snapshot_hash(&["b".into(), "a".into()], &hashes);
        assert_eq!(ab, ba);
    }

    #[test]
    fn recompute_task_snapshot_hash_changes_when_a_hash_changes() {
        let mut hashes = BTreeMap::new();
        hashes.insert("a".to_string(), "h1".to_string());
        let before = recompute_task_snapshot_hash(&["a".into()], &hashes);
        hashes.insert("a".to_string(), "h2".to_string());
        let after = recompute_task_snapshot_hash(&["a".into()], &hashes);
        assert_ne!(before, after);
    }
}
