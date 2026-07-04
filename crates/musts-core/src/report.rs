//! Render the validation result for the agent.
//!
//! Two surfaces:
//! - Text per `docs/musts-design.md` §11.2 — multi-line,
//!   agent-friendly.
//! - JSON per `docs/PLAN.md` §5 — stable shape, frozen at first ship.
//!
//! Both are pure functions over [`ValidateReport`]; the orchestrator
//! builds the report and lets the CLI pick a renderer.

use std::collections::BTreeSet;

use musts_protocol::{EvidenceContract, IgnoredCheck, Task};
use serde::Serialize;
use serde_json::{json, Value as JsonValue};

/// One note from an extension's resolve response, tagged with the
/// capability that produced it.
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityNote {
    pub capability: String,
    pub note: String,
}

/// Aggregated output of a single `musts validate` run.
#[derive(Debug, Clone, Serialize)]
pub struct ValidateReport {
    pub workspace_root: String,
    pub tasks: Vec<Task>,
    pub ignored_checks: Vec<IgnoredCheck>,
    pub notes: Vec<CapabilityNote>,
    /// Ids of tasks that are byte-for-byte the same request as the previous
    /// `musts validate` run (same `satisfies`, same scope hashes). The text
    /// renderer prints these compactly instead of repeating the full body,
    /// so a loop that re-runs `validate` doesn't re-inject identical task
    /// specs every time. Not part of the frozen JSON shape.
    #[serde(skip)]
    pub repeated_task_ids: Vec<String>,
}

impl ValidateReport {
    pub fn is_clean(&self) -> bool {
        self.tasks.is_empty()
    }
}

/// Render the compact text representation for agents.
pub fn render_text(report: &ValidateReport) -> String {
    let mut out = String::new();
    if report.is_clean() {
        out.push_str("Musts validation clean.\n");
        push_notes_section(&mut out, &report.notes);
        push_ignored_section(&mut out, &report.ignored_checks);
        return out;
    }
    out.push_str(&format!(
        "Musts validation pending: {} task{}.\n\n",
        report.tasks.len(),
        if report.tasks.len() == 1 { "" } else { "s" }
    ));
    let repeated: BTreeSet<&str> = report
        .repeated_task_ids
        .iter()
        .map(String::as_str)
        .collect();
    for (i, task) in report.tasks.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // Only the compact form for *runnable* tasks that are unchanged: a
        // `musts run <id>` line is self-sufficient for any reader. Judgment
        // tasks (`agent`/`mav`) are never compacted — their `do:` (the
        // facts to verify / flow to drive) is unrecoverable for a reader
        // that didn't see the prior report (a fresh agent session, CI, or
        // the pre-commit hook that re-runs validate).
        if repeated.contains(task.id.as_str()) && task.command.is_some() {
            push_repeated_runnable_task(&mut out, i + 1, task);
        } else {
            push_task(&mut out, i + 1, task);
        }
    }
    out.push('\n');
    push_ignored_section(&mut out, &report.ignored_checks);
    push_notes_section(&mut out, &report.notes);
    out.push_str("Run runnable checks with `musts run <task-id>`; record judgment checks with `musts evidence`. Then rerun `musts validate` until clean.\n");
    out
}

fn push_task(out: &mut String, index: usize, task: &Task) {
    out.push_str(&format!("{}. {}\n", index, task.id));
    out.push_str(&format!("   do: {}\n", task_action(task)));
    if task.command.is_some() {
        // Deterministic built-in check: musts can run it and record the
        // evidence itself.
        out.push_str(&format!("   run: musts run {}\n", task.id));
    } else {
        out.push_str(&format!(
            "   evidence: {}\n",
            evidence_contract_summary(&task.evidence_contract)
        ));
        out.push_str(&format!(
            "   submit: musts evidence {}{}\n",
            task.id,
            evidence_submit_args(&task.evidence_contract)
        ));
    }
}

/// Compact form for a **runnable** task unchanged since the previous
/// `musts validate`. The `musts run <id>` line is all a reader needs, so
/// the full body isn't repeated.
fn push_repeated_runnable_task(out: &mut String, index: usize, task: &Task) {
    out.push_str(&format!(
        "{}. {} (unchanged since last validate)\n",
        index, task.id
    ));
    out.push_str(&format!("   run: musts run {}\n", task.id));
}

fn task_action(task: &Task) -> String {
    let useful: Vec<&str> = task
        .instructions
        .iter()
        .map(String::as_str)
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("Record evidence")
                && !trimmed.starts_with("Record the result")
                && !trimmed.starts_with("Capture combined stdout/stderr")
        })
        .collect();
    if useful.is_empty() {
        task.title.clone()
    } else {
        useful.join(" ")
    }
}

fn evidence_contract_summary(contract: &EvidenceContract) -> String {
    let mut parts = Vec::new();
    if contract.text.required {
        parts.push("text".to_string());
    }
    for asset in &contract.assets {
        if asset.required {
            parts.push(asset.kind.clone());
        } else {
            parts.push(format!("{} optional", asset.kind));
        }
    }
    if parts.is_empty() {
        "none".into()
    } else {
        parts.join(" + ")
    }
}

fn evidence_submit_args(contract: &EvidenceContract) -> String {
    let mut args = String::new();
    if contract.text.required {
        args.push_str(" --text \"...\"");
    }
    for asset in &contract.assets {
        if asset.required {
            args.push_str(&format!(" --asset <{}>", asset.kind));
        }
    }
    args
}

fn push_ignored_section(out: &mut String, ignored: &[IgnoredCheck]) {
    if ignored.is_empty() {
        return;
    }
    out.push_str("Ignored checks:\n");
    for ic in ignored {
        out.push_str(&format!("  - {}: {}\n", ic.id, ic.reason));
    }
    out.push('\n');
}

fn push_notes_section(out: &mut String, notes: &[CapabilityNote]) {
    if notes.is_empty() {
        return;
    }
    out.push_str("Notes:\n");
    for n in notes {
        out.push_str(&format!("  - [{}] {}\n", n.capability, n.note));
    }
    out.push('\n');
}

/// Render the JSON representation per PLAN.md §5 (frozen shape).
pub fn render_json(report: &ValidateReport) -> JsonValue {
    json!({
        "protocol_version": 1,
        "status": if report.is_clean() { "clean" } else { "pending" },
        "workspace_root": report.workspace_root,
        "tasks": report.tasks,
        "ignored_checks": report.ignored_checks,
        "notes": report.notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use musts_protocol::{AssetContract, EvidenceContract, IgnoredCheck, Task, TextContract};

    fn sample_task() -> Task {
        Task {
            id: "bazel-build-login".into(),
            extension: "bazel/build".into(),
            title: "Build Login module".into(),
            satisfies: vec!["App/Login/login-build".into()],
            parallelizable: true,
            command: None,
            instructions: vec![
                "Run `bazel build //App/Login:Login`.".into(),
                "Capture stdout/stderr as a log asset.".into(),
            ],
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
        }
    }

    fn pending_report() -> ValidateReport {
        ValidateReport {
            workspace_root: "/repo".into(),
            tasks: vec![sample_task()],
            ignored_checks: vec![IgnoredCheck {
                id: "root/app-build".into(),
                reason: "deeper build covers the scope".into(),
            }],
            notes: vec![CapabilityNote {
                capability: "bazel/build".into(),
                note: "selected deepest applicable target".into(),
            }],
            repeated_task_ids: vec![],
        }
    }

    fn clean_report() -> ValidateReport {
        ValidateReport {
            workspace_root: "/repo".into(),
            tasks: vec![],
            ignored_checks: vec![],
            notes: vec![],
            repeated_task_ids: vec![],
        }
    }

    #[test]
    fn text_clean_render() {
        let out = render_text(&clean_report());
        assert!(out.starts_with("Musts validation clean."));
        assert!(!out.contains("No pending validation tasks"));
    }

    #[test]
    fn text_pending_render_is_compact_and_actionable() {
        let out = render_text(&pending_report());
        assert!(out.contains("Musts validation pending: 1 task."));
        assert!(out.contains("1. bazel-build-login"));
        assert!(out.contains("do: Run `bazel build //App/Login:Login`."));
        assert!(out.contains("evidence: text + log"));
        assert!(
            out.contains("submit: musts evidence bazel-build-login --text \"...\" --asset <log>")
        );
        assert!(!out.contains("Extension: bazel/build"));
        assert!(!out.contains("Satisfies:"));
        assert!(out.contains("Ignored checks:"));
        assert!(out.contains("- root/app-build:"));
        assert!(out.contains("Notes:"));
        assert!(out.contains("[bazel/build]"));
        assert!(out.contains("rerun `musts validate` until clean"));
    }

    /// A runnable (deterministic) task carrying a machine command.
    fn runnable_task() -> Task {
        let mut t = sample_task();
        t.id = "cargo-test-root".into();
        t.extension = "cargo/test".into();
        t.command = Some(vec!["cargo".into(), "test".into(), "--workspace".into()]);
        t
    }

    #[test]
    fn text_runnable_task_shows_run_line() {
        let report = ValidateReport {
            tasks: vec![runnable_task()],
            ..pending_report()
        };
        let out = render_text(&report);
        assert!(out.contains("run: musts run cargo-test-root"));
        // Deterministic tasks don't advertise the manual evidence path.
        assert!(!out.contains("submit: musts evidence cargo-test-root"));
    }

    #[test]
    fn text_repeated_runnable_task_is_compact() {
        let report = ValidateReport {
            tasks: vec![runnable_task()],
            repeated_task_ids: vec!["cargo-test-root".into()],
            ..pending_report()
        };
        let out = render_text(&report);
        // Compact: id + a self-sufficient `musts run` line, no full body.
        assert!(out.contains("cargo-test-root (unchanged since last validate)"));
        assert!(out.contains("run: musts run cargo-test-root"));
        assert!(!out.contains("do: Run `bazel build"));
    }

    #[test]
    fn text_repeated_judgment_task_is_never_compacted() {
        // `sample_task` has no command (judgment). Even when marked
        // repeated, its full `do:` must print — a fresh reader (CI, the
        // pre-commit hook, a new session) needs the instructions.
        let mut report = pending_report();
        report.repeated_task_ids = vec!["bazel-build-login".into()];
        let out = render_text(&report);
        assert!(out.contains("do: Run `bazel build //App/Login:Login`."));
        assert!(!out.contains("unchanged since last validate"));
    }

    #[test]
    fn text_non_repeated_task_shows_full_body() {
        // Sanity: without the id in repeated_task_ids, the full body prints.
        let out = render_text(&pending_report());
        assert!(out.contains("do: Run `bazel build //App/Login:Login`."));
    }

    #[test]
    fn json_pending_has_stable_shape() {
        let v = render_json(&pending_report());
        assert_eq!(v["protocol_version"], 1);
        assert_eq!(v["status"], "pending");
        assert_eq!(v["workspace_root"], "/repo");
        assert_eq!(v["tasks"][0]["id"], "bazel-build-login");
        // Mirrors spec §9.4 — `extension` (not `capability`) on tasks.
        assert_eq!(v["tasks"][0]["extension"], "bazel/build");
        assert_eq!(v["ignored_checks"][0]["id"], "root/app-build");
        assert_eq!(v["notes"][0]["capability"], "bazel/build");
        // A judgment task (no command) omits the key — additive shape.
        assert!(v["tasks"][0].get("command").is_none());
        // `repeated_task_ids` is never serialised (#[serde(skip)]).
        assert!(v.get("repeated_task_ids").is_none());
    }

    #[test]
    fn json_runnable_task_carries_command_array() {
        // Additive shape change: deterministic tasks expose `command` so a
        // machine consumer (and `musts run`) can see the argv. Absent for
        // judgment tasks; present here.
        let report = ValidateReport {
            tasks: vec![runnable_task()],
            ..pending_report()
        };
        let v = render_json(&report);
        assert_eq!(v["tasks"][0]["command"][0], "cargo");
        assert_eq!(v["tasks"][0]["command"][2], "--workspace");
    }

    #[test]
    fn json_clean_has_empty_arrays() {
        let v = render_json(&clean_report());
        assert_eq!(v["status"], "clean");
        assert!(v["tasks"].as_array().unwrap().is_empty());
        assert!(v["ignored_checks"].as_array().unwrap().is_empty());
        assert!(v["notes"].as_array().unwrap().is_empty());
    }
}
