//! Render the validation result for the agent.
//!
//! Two surfaces:
//! - Text per `docs/harness-validation-plan.md` §11.2 — multi-line,
//!   agent-friendly.
//! - JSON per `docs/PLAN.md` §5 — stable shape, frozen at first ship.
//!
//! Both are pure functions over [`ValidateReport`]; the orchestrator
//! builds the report and lets the CLI pick a renderer.

use harness_protocol::{AssetContract, EvidenceContract, IgnoredCheck, Task, TextContract};
use serde::Serialize;
use serde_json::{json, Value as JsonValue};

/// One note from an extension's resolve response, tagged with the
/// capability that produced it.
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityNote {
    pub capability: String,
    pub note: String,
}

/// Aggregated output of a single `harness validate` run.
#[derive(Debug, Clone, Serialize)]
pub struct ValidateReport {
    pub workspace_root: String,
    pub tasks: Vec<Task>,
    pub ignored_checks: Vec<IgnoredCheck>,
    pub notes: Vec<CapabilityNote>,
}

impl ValidateReport {
    pub fn is_clean(&self) -> bool {
        self.tasks.is_empty()
    }
}

/// Render the text representation per spec §11.2.
pub fn render_text(report: &ValidateReport) -> String {
    let mut out = String::new();
    if report.is_clean() {
        out.push_str("Harness validation clean.\n");
        out.push_str("No pending validation tasks for the current workspace snapshot.\n");
        push_notes_section(&mut out, &report.notes);
        push_ignored_section(&mut out, &report.ignored_checks);
        return out;
    }
    out.push_str("Harness validation pending.\n\n");
    for (i, task) in report.tasks.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        push_task(&mut out, task);
    }
    out.push('\n');
    push_ignored_section(&mut out, &report.ignored_checks);
    push_notes_section(&mut out, &report.notes);
    out.push_str("Completion rule:\n");
    out.push_str("  Repeat `harness validate` after recording evidence.\n");
    out.push_str("  The task is not done until this report is empty.\n");
    out
}

fn push_task(out: &mut String, task: &Task) {
    out.push_str(&format!("Task: {}\n", task.id));
    out.push_str(&format!("Title: {}\n", task.title));
    out.push_str(&format!("Extension: {}\n", task.extension));
    if !task.satisfies.is_empty() {
        out.push_str("Satisfies:\n");
        for s in &task.satisfies {
            out.push_str(&format!("  - {s}\n"));
        }
    }
    if task.parallelizable {
        out.push_str("Parallelizable: yes\n");
    }
    if !task.instructions.is_empty() {
        out.push_str("Instructions:\n");
        for (idx, line) in task.instructions.iter().enumerate() {
            out.push_str(&format!("  {}. {line}\n", idx + 1));
        }
    }
    push_evidence_contract(out, &task.evidence_contract);
}

fn push_evidence_contract(out: &mut String, contract: &EvidenceContract) {
    out.push_str("Evidence required:\n");
    push_text_contract(out, &contract.text);
    for asset in &contract.assets {
        push_asset_contract(out, asset);
    }
}

fn push_text_contract(out: &mut String, text: &TextContract) {
    if text.required {
        let desc = text
            .description
            .as_deref()
            .unwrap_or("State the validation result.");
        out.push_str(&format!("  - text (required): {desc}\n"));
    }
}

fn push_asset_contract(out: &mut String, asset: &AssetContract) {
    let required = if asset.required {
        "required"
    } else {
        "optional"
    };
    let desc = asset
        .description
        .as_deref()
        .map(|d| format!(": {d}"))
        .unwrap_or_default();
    out.push_str(&format!("  - {} ({required}){desc}\n", asset.kind));
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
    use harness_protocol::{AssetContract, EvidenceContract, IgnoredCheck, Task, TextContract};

    fn sample_task() -> Task {
        Task {
            id: "bazel-build-login".into(),
            extension: "bazel/build".into(),
            title: "Build Login module".into(),
            satisfies: vec!["App/Login/login-build".into()],
            parallelizable: true,
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
        }
    }

    fn clean_report() -> ValidateReport {
        ValidateReport {
            workspace_root: "/repo".into(),
            tasks: vec![],
            ignored_checks: vec![],
            notes: vec![],
        }
    }

    #[test]
    fn text_clean_render() {
        let out = render_text(&clean_report());
        assert!(out.starts_with("Harness validation clean."));
        assert!(out.contains("No pending validation tasks"));
    }

    #[test]
    fn text_pending_render_contains_task_and_completion_rule() {
        let out = render_text(&pending_report());
        assert!(out.contains("Harness validation pending."));
        assert!(out.contains("Task: bazel-build-login"));
        assert!(out.contains("Extension: bazel/build"));
        assert!(out.contains("Satisfies:"));
        assert!(out.contains("- App/Login/login-build"));
        assert!(out.contains("Ignored checks:"));
        assert!(out.contains("- root/app-build:"));
        assert!(out.contains("Notes:"));
        assert!(out.contains("[bazel/build]"));
        assert!(out.contains("Completion rule:"));
        assert!(out.contains("until this report is empty"));
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
