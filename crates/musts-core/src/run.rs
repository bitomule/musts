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
use crate::builtin::bazel_targets::{no_tests_is_vacuous, passed_tests, PassedTests};
use crate::error::{Error, Result};
use crate::evidence::submit::{submit, SubmissionInputs};
use crate::evidence::EvidenceSubmissionResult;
use crate::extension::descriptor::discover_descriptors;
use crate::extension::runtime::RuntimeOptions;
use crate::snapshot::hash_bytes;
use crate::state::cache;
use crate::validate::compute_current_scope_hashes;

/// How much of a failing log reaches the agent's context.
pub const FAILURE_OUTPUT_CHARS: usize = 6000;

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
        /// The files are unchanged since this exact failure, so nothing
        /// was executed and the earlier result is returned.
        remembered: bool,
    },
    /// The command exited 0 and evidence was accepted and recorded.
    Recorded {
        command: String,
        result: EvidenceSubmissionResult,
        /// Set for `bazel/test`: how many tests passed and how many of
        /// them Bazel served from its test cache.
        tests: Option<PassedTests>,
    },
}

/// Execute the command declared by task `task_id` and, on success, record
/// evidence for it.
pub fn execute(
    session: &mut StateSession,
    workspace_root: &Path,
    runtime_options: &RuntimeOptions,
    task_id: &str,
    force: bool,
) -> Result<RunOutcome> {
    let stored =
        cache::fetch_task(&session.musts_dir, task_id).ok_or_else(|| Error::TaskNotFound {
            task_id: task_id.to_string(),
        })?;
    let task: Task = stored.task;

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

    let command_display = argv.join(" ");
    let fingerprint = failure_fingerprint(
        &command_display,
        &stored.satisfies,
        &compute_current_scope_hashes(session, workspace_root)?,
    );
    if !force {
        if let Some(failed) = cache::load_failures(&session.musts_dir).remove(task_id) {
            if failed.fingerprint == fingerprint && failed.log_path.is_file() {
                let output =
                    std::fs::read_to_string(&failed.log_path).map_err(|source| Error::Io {
                        path: failed.log_path.clone(),
                        source,
                    })?;
                return Ok(RunOutcome::Failed {
                    command: failed.command,
                    code: failed.code,
                    output,
                    log_path: failed.log_path,
                    remembered: true,
                });
            }
        }
    }

    // Execute argv directly (no shell) from the workspace root.
    //
    // The workspace lock is released for the duration: the check's own
    // command touches no musts state, and holding the lock across it
    // turned every `musts run` into a workspace-wide stop sign for as
    // long as the suite took — measured at 60,23 s held out of a 60,28 s
    // run, which is how a second `musts run` in the same worktree got
    // `another musts process is running` from its own first one. Writes
    // are still serialised: the evidence submission below happens with
    // the lock back in hand.
    let log_path = temp_log_path(task_id);
    let (combined, code, spawn_error) = session.unlocked(|| {
        match Command::new(&argv[0])
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
        }
    })?;

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

    let is_bazel_test = stored.capability == "bazel/test";
    let vacuous = is_bazel_test && no_tests_is_vacuous(&argv, code);
    let succeeded = spawn_error.is_none() && (code == Some(0) || vacuous);
    let mut failures = cache::load_failures(&session.musts_dir);
    if !succeeded {
        failures.insert(
            task_id.to_string(),
            cache::FailedRun {
                fingerprint,
                command: command_display.clone(),
                code,
                log_path: log_path.clone(),
            },
        );
        cache::store_failures(&session.musts_dir, &failures)?;
        return Ok(RunOutcome::Failed {
            command: command_display,
            code,
            output: log_body,
            log_path,
            remembered: false,
        });
    }
    if failures.remove(task_id).is_some() {
        cache::store_failures(&session.musts_dir, &failures)?;
    }

    // Success: record evidence through the normal pipeline. The capability
    // validator re-checks the log content, so a "green" exit with a log
    // that still records a failure is caught here too.
    let tests = is_bazel_test.then(|| passed_tests(&combined));
    let auto_text = match &tests {
        Some(tests) if tests.total > 0 => format!(
            "`{command_display}` exited 0 (executed by `musts run`): {}.",
            describe_tests(tests)
        ),
        _ => format!("`{command_display}` exited 0 (executed by `musts run`)."),
    };
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
        tests,
    })
}

/// What a run is a failure *of*: the command and the scope of every check
/// the task covers. The same fingerprint means the same command over the
/// same files.
fn failure_fingerprint(
    command: &str,
    satisfies: &[String],
    current_hashes: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut material = format!("{command}\n");
    let mut checks: Vec<&String> = satisfies.iter().collect();
    checks.sort();
    for check in checks {
        let hash = current_hashes.get(check).map_or("", String::as_str);
        material.push_str(&format!("{check}={hash}\n"));
    }
    hash_bytes(material.as_bytes())
}

/// The last `max_chars` characters of `output`, and whether anything was
/// cut. Cuts on a character boundary, at the start of a line when one is
/// near.
pub fn tail_chars(output: &str, max_chars: usize) -> (&str, bool) {
    let total = output.chars().count();
    if total <= max_chars {
        return (output, false);
    }
    let start = output
        .char_indices()
        .nth(total - max_chars)
        .map_or(0, |(i, _)| i);
    (&output[start..], true)
}

/// `12 tests passed, 10 from Bazel's test cache`.
pub fn describe_tests(tests: &PassedTests) -> String {
    let plural = if tests.total == 1 { "" } else { "s" };
    let mut text = format!("{} test{plural} passed", tests.total);
    if tests.cached > 0 {
        text.push_str(&format!(", {} from Bazel's test cache", tests.cached));
    }
    text
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
    fn tail_keeps_the_end_and_flags_the_cut() {
        assert_eq!(tail_chars("abc", 5), ("abc", false));
        assert_eq!(tail_chars("abcdef", 3), ("def", true));
        assert_eq!(tail_chars("ñandú", 3), ("ndú", true));
    }

    #[test]
    fn fingerprint_changes_with_the_files_or_the_command() {
        let satisfies = vec!["fmt".to_string()];
        let at = |hash: &str| [("fmt".to_string(), hash.to_string())].into();
        let base = failure_fingerprint("cargo fmt", &satisfies, &at("a"));
        assert_eq!(base, failure_fingerprint("cargo fmt", &satisfies, &at("a")));
        assert_ne!(base, failure_fingerprint("cargo fmt", &satisfies, &at("b")));
        assert_ne!(
            base,
            failure_fingerprint("cargo test", &satisfies, &at("a"))
        );
    }

    #[test]
    fn temp_log_path_is_outside_cwd_and_sanitised() {
        let p = temp_log_path("cargo/test-root");
        let name = p.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("musts-run-cargo-test-root-"));
        assert!(name.ends_with(".log"));
        assert!(p.is_absolute());
    }
}
