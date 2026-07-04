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
use crate::evidence::ledger::{fetch_task, insert_atomic, EvidenceRow};
use crate::evidence::store::{describe_asset, new_submission_id};
use crate::extension::descriptor::{discover_descriptors, Capability, ExtensionDescriptor};
use crate::extension::runtime::{ExtensionRunner, RuntimeOptions};
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
    let stored = fetch_task(&session.db, inputs.task_id)?.ok_or_else(|| Error::TaskNotFound {
        task_id: inputs.task_id.to_string(),
    })?;
    let stored_satisfies: Vec<String> =
        serde_json::from_str(&stored.satisfies_json).unwrap_or_default();
    let stored_scope_hashes: BTreeMap<String, String> =
        serde_json::from_str(&stored.scope_hashes_json).unwrap_or_default();
    let stored_task: Task =
        serde_json::from_str(&stored.payload_json).map_err(|err| Error::Db {
            source: rusqlite::Error::FromSqlConversionFailure(
                stored.payload_json.len(),
                rusqlite::types::Type::Text,
                Box::new(err),
            ),
        })?;
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
    //    `.musts/evidence/`; the ledger (`evidence_records` + the committed
    //    lock) is the durable record, and the asset is validated where it
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

    // 8. Persist ledger rows atomically: one per accepted-now check,
    //    keyed by that check's declaring-manifest scope_hash.
    let now_unix = unix_seconds_now();
    let submission_json = serde_json::to_string(&submission).unwrap_or_else(|_| "{}".into());
    let result_json = serde_json::to_string(&response).unwrap_or_else(|_| "{}".into());
    // Materialise per-check scope hashes first so the borrowed-from
    // strings outlive the `EvidenceRow` Vec.
    let resolved_hashes: Vec<String> = accepted_now
        .iter()
        .map(|cid| stored_scope_hashes.get(cid).cloned().unwrap_or_default())
        .collect();
    let rows: Vec<EvidenceRow<'_>> = accepted_now
        .iter()
        .zip(resolved_hashes.iter())
        .map(|(cid, scope_hash)| EvidenceRow {
            task_id: inputs.task_id,
            submission_id: &submission_id,
            check_id: cid,
            scope_hash,
            accepted: true,
            summary: response.summary.as_deref(),
            submission_json: &submission_json,
            result_json: &result_json,
            submitted_at_unix: now_unix,
        })
        .collect();
    insert_atomic(&mut session.db, &rows)?;

    // 8b. Append the newly-green `(check_id, scope_hash)` pairs to the
    //     portable ledger lock so a fresh clone inherits them. Reading
    //     and writing the full lock each time is self-healing: a missed
    //     write recovers on the next accepted submission of any check.
    {
        let mut lock = crate::state::lock::load(&session.musts_dir)?;
        let mut changed = false;
        for (cid, scope_hash) in accepted_now.iter().zip(resolved_hashes.iter()) {
            if lock.record(cid, scope_hash) {
                changed = true;
            }
        }
        if changed {
            crate::state::lock::save(&session.musts_dir, &lock)?;
        }
    }

    Ok(EvidenceSubmissionResult {
        task_id: inputs.task_id.to_string(),
        submission_id,
        satisfied: accepted_now,
        summary: response.summary,
    })
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
