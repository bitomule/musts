//! `musts run <task-id>` — execute a deterministic check's command and
//! record evidence from the real result, so the agent never has to
//! re-run the command just to satisfy the loop.
//!
//! Only **built-in deterministic** capabilities are runnable: the task
//! must carry a machine `command` (populated by `cargo/*` and
//! `bazel/build`) and the capability must not be provided by an installed
//! external extension. This keeps `musts run` from executing arbitrary
//! commands an untrusted descriptor could inject — it only ever runs the
//! fixed argv of its own built-ins. Judgment capabilities (`agent`,
//! `mav/expect`) carry no command and stay on the `musts evidence` path.
//!
//! On success the captured log flows through the normal evidence pipeline
//! (`evidence::submit`), so the same content checks that guard a
//! hand-submitted log also guard a `musts run` one — musts watched the
//! exit code *and* re-validates the output.

use std::path::{Path, PathBuf};
use std::process::Command;

use musts_protocol::Task;

use crate::bootstrap::StateSession;
use crate::error::{Error, Result};
use crate::evidence::ledger::fetch_task;
use crate::evidence::submit::{submit, SubmissionInputs};
use crate::evidence::EvidenceSubmissionResult;
use crate::extension::descriptor::discover_descriptors;
use crate::extension::runtime::RuntimeOptions;

/// Result of a `musts run`.
#[derive(Debug)]
pub enum RunOutcome {
    /// The task has no runnable command (judgment task) or is provided by
    /// an external extension. The agent should use `musts evidence`.
    NotRunnable { reason: String },
    /// The command ran and exited non-zero. No evidence recorded. The
    /// combined output is returned so the caller can surface it.
    Failed {
        command: String,
        code: Option<i32>,
        output: String,
        log_path: PathBuf,
    },
    /// The command exited 0 and evidence was accepted and recorded.
    Recorded {
        command: String,
        result: EvidenceSubmissionResult,
    },
}

/// Execute the command declared by task `task_id` and, on success, record
/// evidence for it.
pub fn execute(
    session: &mut StateSession,
    workspace_root: &Path,
    runtime_options: &RuntimeOptions,
    task_id: &str,
) -> Result<RunOutcome> {
    let stored = fetch_task(&session.db, task_id)?.ok_or_else(|| Error::TaskNotFound {
        task_id: task_id.to_string(),
    })?;
    let task: Task = serde_json::from_str(&stored.payload_json).map_err(|err| Error::Db {
        source: rusqlite::Error::FromSqlConversionFailure(
            stored.payload_json.len(),
            rusqlite::types::Type::Text,
            Box::new(err),
        ),
    })?;

    let Some(argv) = task.command.filter(|c| !c.is_empty()) else {
        return Ok(RunOutcome::NotRunnable {
            reason: format!(
                "task `{task_id}` has no runnable command — it needs agent-produced evidence. \
                 Do the validation and submit it with `musts evidence {task_id} …`."
            ),
        });
    };

    // Safety boundary: only run commands from built-in capabilities that
    // are NOT overridden by an installed extension. A descriptor-backed
    // extension could otherwise have its `command` executed here.
    if crate::builtin::lookup(&stored.capability).is_none() {
        return Ok(RunOutcome::NotRunnable {
            reason: format!(
                "`musts run` only executes built-in deterministic checks (cargo/*, bazel/build); \
                 `{}` is not one. Use `musts evidence`.",
                stored.capability
            ),
        });
    }
    let descriptors = discover_descriptors(workspace_root)?;
    let externally_overridden = descriptors
        .iter()
        .flat_map(|d| d.capabilities.values())
        .any(|c| c.uses == stored.capability);
    if externally_overridden {
        return Ok(RunOutcome::NotRunnable {
            reason: format!(
                "capability `{}` is provided by an installed extension, so `musts run` will not \
                 execute it. Validate it yourself and use `musts evidence`.",
                stored.capability
            ),
        });
    }

    // Execute argv directly (no shell) from the workspace root.
    let command_display = argv.join(" ");
    let log_path = temp_log_path(task_id);
    let (combined, code, spawn_error) = match Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(workspace_root)
        .output()
    {
        Ok(out) => {
            let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !stderr.is_empty() {
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                combined.push_str(&stderr);
            }
            (combined, out.status.code(), None)
        }
        Err(err) => (
            format!("failed to spawn `{}`: {err}", argv[0]),
            None,
            Some(err),
        ),
    };

    // Prefix the log with the exact command so it is self-documenting and
    // never empty — a silently-succeeding command like `cargo fmt --check`
    // produces no output, which the log validators reject as "empty". The
    // header line starts with `$ `, so it never trips a content heuristic
    // (clippy looks for `error:`/`warning:` line starts; fmt for `Diff in`;
    // test for `test result:`).
    let log_body = format!("$ {command_display}\n{combined}");
    std::fs::write(&log_path, log_body.as_bytes()).map_err(|source| Error::Io {
        path: log_path.clone(),
        source,
    })?;

    let succeeded = spawn_error.is_none() && code == Some(0);
    if !succeeded {
        return Ok(RunOutcome::Failed {
            command: command_display,
            code,
            output: log_body,
            log_path,
        });
    }

    // Success: record evidence through the normal pipeline. The capability
    // validator re-checks the log content, so a "green" exit with a log
    // that still records a failure is caught here too.
    let auto_text = format!("`{command_display}` exited 0 (executed by `musts run`).");
    let asset_paths: [&Path; 1] = [log_path.as_path()];
    let inputs = SubmissionInputs {
        task_id,
        text: Some(&auto_text),
        asset_paths: &asset_paths,
    };
    let result = submit(session, workspace_root, runtime_options, &inputs)?;
    Ok(RunOutcome::Recorded {
        command: command_display,
        result,
    })
}

/// A per-run log path in the OS temp dir (outside the workspace, so
/// writing it never perturbs a scope hash). Includes the process id to
/// avoid collisions between concurrent runs.
fn temp_log_path(task_id: &str) -> PathBuf {
    let slug: String = task_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    std::env::temp_dir().join(format!("musts-run-{slug}-{}.log", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_log_path_is_outside_cwd_and_sanitised() {
        let p = temp_log_path("cargo/test-root");
        let name = p.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("musts-run-cargo-test-root-"));
        assert!(name.ends_with(".log"));
        assert!(p.is_absolute());
    }
}
