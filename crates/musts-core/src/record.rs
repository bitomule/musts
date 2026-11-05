//! `musts stamp` and `musts record`: accept a `bazel test` the agent
//! already ran instead of running it again.
//!
//! `musts stamp` runs as Bazel's `--workspace_status_command`, so every
//! invocation's build event stream carries the scope hash of every check
//! as it was when that invocation started. `musts record` reads the
//! stream (`--build_event_json_file`), judges it with
//! [`crate::builtin::bazel_bep`], and writes the ledger entry for each
//! check it proves — only when the tree the run saw is the tree the
//! check would hash now. See `docs/record.md`.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use musts_protocol::EvidenceSubmission;

use crate::bootstrap::StateSession;
use crate::builtin::bazel_bep::{self, Invocation, STAMP_KEY};
use crate::error::{Error, Result};
use crate::evidence::ledger::{fetch_task, find_task_for_check};
use crate::evidence::store::{describe_asset, new_submission_id};
use crate::evidence::submit::{persist_accepted, AcceptedEvidence};
use crate::extension::descriptor::discover_descriptors;
use crate::validate::{compute_current_scope_hashes, compute_current_scopes, declared_checks};

/// Where the documented `.bazelrc` line writes the stream.
pub const DEFAULT_BEP_PATH: &str = ".musts/bep.json";

/// How long `musts stamp` waits for the workspace lock before giving up.
/// Bazel waits for the status command before every build, so a stamp
/// must never be what a build is waiting on.
pub const STAMP_LOCK_WAIT: Duration = Duration::from_secs(5);

/// The line `musts stamp` prints: `MUSTS_SCOPES {"<check>":"<hash>",…}`.
pub fn stamp_line(workspace_root: &Path) -> Result<String> {
    let mut session = StateSession::acquire_waiting(workspace_root, STAMP_LOCK_WAIT)?;
    let hashes = compute_current_scope_hashes(&mut session, workspace_root)?;
    let json = serde_json::to_string(&hashes).unwrap_or_else(|_| "{}".into());
    Ok(format!("{STAMP_KEY} {json}"))
}

/// The verdict on one check.
#[derive(Debug)]
pub struct CheckVerdict {
    pub check_id: String,
    /// `Ok(tests passed)` when recorded; `Err(reason)` when it stays pending.
    pub outcome: std::result::Result<usize, String>,
}

#[derive(Debug)]
pub struct RecordOutcome {
    pub task_id: String,
    /// First 8 characters of the invocation's uuid; empty when the file
    /// could not be read as a build event stream.
    pub invocation: String,
    pub verdicts: Vec<CheckVerdict>,
}

impl RecordOutcome {
    pub fn all_recorded(&self) -> bool {
        self.verdicts.iter().all(|v| v.outcome.is_ok())
    }
}

/// Judge the build event file at `bep_path` for `id` (a task id, or a
/// check id of a `bazel/test` task) and record every check it proves.
pub fn record(
    session: &mut StateSession,
    workspace_root: &Path,
    id: &str,
    bep_path: &Path,
) -> Result<RecordOutcome> {
    let (task_id, requested) = resolve_id(session, id)?;
    let stored = fetch_task(&session.db, &task_id)?.ok_or_else(|| Error::TaskNotFound {
        task_id: task_id.clone(),
    })?;
    if stored.capability != "bazel/test" {
        return Err(Error::RecordUnsupported {
            task_id,
            reason: format!(
                "it is a `{}` task; `musts record` only reads bazel test runs",
                stored.capability
            ),
        });
    }
    let overridden = discover_descriptors(workspace_root)?
        .iter()
        .flat_map(|d| d.capabilities.values())
        .any(|c| c.uses == stored.capability);
    if overridden {
        return Err(Error::RecordUnsupported {
            task_id,
            reason: "`bazel/test` is provided by an installed extension here; use `musts evidence`"
                .into(),
        });
    }
    let satisfies: Vec<String> = serde_json::from_str(&stored.satisfies_json).unwrap_or_default();
    let stored_hashes: BTreeMap<String, String> =
        serde_json::from_str(&stored.scope_hashes_json).unwrap_or_default();
    let checks: Vec<String> = match requested {
        Some(check) => vec![check],
        None => satisfies,
    };

    let shown = bep_path.display().to_string();
    let absolute = if bep_path.is_absolute() {
        bep_path.to_path_buf()
    } else {
        workspace_root.join(bep_path)
    };
    let invocation = match std::fs::read(&absolute) {
        Ok(bytes) => bazel_bep::parse(&bytes, &shown),
        Err(err) => Err(format!(
            "cannot read the build event file {shown} ({err}); run `bazel test` with \
             --build_event_json_file={DEFAULT_BEP_PATH} (see docs/record.md)"
        )),
    };
    let invocation = match invocation {
        Ok(inv) => inv,
        Err(reason) => return Ok(rejected_all(task_id, String::new(), &checks, &reason)),
    };
    if let Some(reason) = bazel_bep::invocation_problem(&invocation, workspace_root) {
        let short = invocation.short_uuid().to_string();
        return Ok(rejected_all(task_id, short, &checks, &reason));
    }

    let declared = declared_checks(workspace_root)?;
    let current = compute_current_scopes(session, workspace_root)?;
    let mut verdicts = Vec::new();
    let mut proven: Vec<(String, String)> = Vec::new();
    for check in &checks {
        let outcome = judge_check(&invocation, check, &declared, &current);
        if outcome.is_ok() {
            let now = current
                .get(check)
                .map(|s| s.hash.clone())
                .unwrap_or_default();
            if stored_hashes.get(check) != Some(&now) {
                return Err(Error::EvidenceStale { task_id });
            }
            proven.push((check.clone(), now));
        }
        verdicts.push(CheckVerdict {
            check_id: check.clone(),
            outcome,
        });
    }

    if !proven.is_empty() {
        let tests: usize = verdicts
            .iter()
            .filter_map(|v| v.outcome.as_ref().ok())
            .sum();
        let summary = format!(
            "Recorded from bazel invocation {}: {tests} test{} passed on the stamped tree.",
            invocation.uuid,
            if tests == 1 { "" } else { "s" }
        );
        let submission = EvidenceSubmission {
            text: Some(summary.clone()),
            assets: describe_asset(&absolute).into_iter().collect(),
        };
        let result = serde_json::json!({
            "recorded_by": "musts record",
            "invocation_id": invocation.uuid,
            "build_event_file": shown,
            "checks": proven.iter().map(|(c, _)| c).collect::<Vec<_>>(),
        });
        persist_accepted(
            session,
            &AcceptedEvidence {
                task_id: &task_id,
                submission_id: &new_submission_id(),
                checks: &proven,
                summary: Some(&summary),
                submission_json: &serde_json::to_string(&submission).unwrap_or_default(),
                result_json: &result.to_string(),
            },
        )?;
    }

    Ok(RecordOutcome {
        task_id,
        invocation: invocation.short_uuid().to_string(),
        verdicts,
    })
}

fn judge_check(
    inv: &Invocation,
    check: &str,
    declared: &BTreeMap<String, (String, serde_json::Value)>,
    current: &BTreeMap<String, crate::validate::CurrentScope>,
) -> std::result::Result<usize, String> {
    let stamp = inv.stamp.as_ref().ok_or_else(|| {
        "the invocation carries no musts stamp; add the .bazelrc lines from docs/record.md"
            .to_string()
    })?;
    let then = stamp.get(check).ok_or_else(|| {
        format!(
            "the invocation's musts stamp has no hash for {check}; the check did not exist then"
        )
    })?;
    let scope = current
        .get(check)
        .ok_or_else(|| format!("{check} has no files in scope now"))?;
    if then != &scope.hash {
        return Err(format!(
            "the tree changed since the invocation: {check} was {} then, is {} now",
            short(then),
            short(&scope.hash)
        ));
    }
    if let Some(start_ms) = inv.start_ms {
        let start_ns = i128::from(start_ms) * 1_000_000;
        if let Some(file) = scope.files.iter().find(|f| f.mtime_ns > start_ns) {
            return Err(format!(
                "{} was modified at {}, after the invocation started at {}",
                file.rel_path,
                clock(file.mtime_ns),
                clock(start_ns)
            ));
        }
    }
    let targets = declared
        .get(check)
        .and_then(|(_, with)| with.get("targets"))
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| format!("{check} declares no bazel/test targets"))?;
    bazel_bep::targets_proven(inv, &targets)
}

fn resolve_id(session: &StateSession, id: &str) -> Result<(String, Option<String>)> {
    if fetch_task(&session.db, id)?.is_some() {
        return Ok((id.to_string(), None));
    }
    match find_task_for_check(&session.db, id)? {
        Some(task_id) => Ok((task_id, Some(id.to_string()))),
        None => Err(Error::TaskNotFound {
            task_id: id.to_string(),
        }),
    }
}

fn rejected_all(
    task_id: String,
    invocation: String,
    checks: &[String],
    reason: &str,
) -> RecordOutcome {
    RecordOutcome {
        task_id,
        invocation,
        verdicts: checks
            .iter()
            .map(|c| CheckVerdict {
                check_id: c.clone(),
                outcome: Err(reason.to_string()),
            })
            .collect(),
    }
}

fn short(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

fn clock(ns: i128) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(ns)
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| ns.to_string())
}
