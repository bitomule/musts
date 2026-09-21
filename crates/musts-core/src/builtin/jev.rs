//! `jev` — a judgment check a machine can run.
//!
//! The core knows only how to ASK for one: it declares the command and reads the evidence,
//! exactly as `bazel/build` declares `bazel build`. Everything about jev — the key, the
//! request, the verdict — lives in `musts-jev`, which ships with musts and sits on the same
//! PATH.
//!
//! It is a built-in rather than an out-of-tree extension for one reason that is not taste:
//! `musts run` refuses to execute a descriptor-backed extension's command, so as an
//! extension this check would be a manual task forever — which is the cost this capability
//! exists to remove.

use musts_protocol::{
    AssetContract, EvidenceContract, EvidenceValidationRequest, EvidenceValidationResponse,
    ResolveRequest, ResolveResponse, Task, TextContract,
};
use serde_json::{json, Value};
use std::sync::OnceLock;

use crate::error::Error;

pub fn schema() -> &'static Value {
    static S: OnceLock<Value> = OnceLock::new();
    S.get_or_init(|| {
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "required": ["ask"],
            "additionalProperties": false,
            "properties": {
                "ask": { "type": "string" },
                "questions": { "type": "string" },
                "question": { "type": "object" },
                "expect": { "type": "string", "enum": ["yes", "no"] },
                "mode": { "type": "string", "enum": ["tripwire", "shadow"] },
                "state": { "type": "string", "enum": ["file", "diff"] },
                "changed_since": { "type": "string" },
                "context_lines": { "type": "integer", "minimum": 0 },
                // The two planted files `musts calibrate` judges the question
                // against: one that really breaks the rule, one near-miss that
                // does not. They live in the manifest rather than in the
                // question set so a reviewer sees what a check was calibrated
                // against without opening a second file, and so nothing in
                // jevi's own format has to be extended to carry them.
                "control": {
                    "type": "object",
                    "required": ["violating", "clean"],
                    "additionalProperties": false,
                    "properties": {
                        "violating": { "type": "string" },
                        "clean": { "type": "string" },
                        // The unchanged file both controls are a one-edit variant of.
                        // Required when the check declares state: diff — a diff question
                        // judged against whole files is calibrated against a mode it never
                        // runs in.
                        "base": { "type": "string" }
                    }
                }
            }
        })
    })
}

fn slug(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect()
}

pub fn resolve(req: &ResolveRequest) -> Result<ResolveResponse, Error> {
    let mut tasks = Vec::new();
    for check in &req.checks {
        let w = &check.with_payload;
        let ask = w.get("ask").and_then(Value::as_str).unwrap_or("?");
        let expect = w.get("expect").and_then(Value::as_str).unwrap_or("yes");
        let mode = w.get("mode").and_then(Value::as_str).unwrap_or("shadow");
        // Two argv entries, not one string. A shell-quoted `--flag 'value'` is a single
        // unrecognised argument once it reaches argv, and nothing catches that until the
        // task is actually run — which is exactly what the end-to-end run caught.
        let question_argv: Vec<String> = match w.get("question") {
            Some(inline) => vec!["--question-json".to_string(), inline.to_string()],
            None => vec![
                "--questions".to_string(),
                w.get("questions")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string(),
            ],
        };
        // The human-readable form keeps the quoting, because a person pastes it into a shell.
        let question = match w.get("question") {
            Some(inline) => format!("--question-json '{inline}'"),
            None => format!(
                "--questions {}",
                w.get("questions").and_then(Value::as_str).unwrap_or("?")
            ),
        };
        // `state: diff` shows the model what the change did rather than the file it was in.
        // All three are pass-through: the core declares the command and reads the evidence, it
        // does not know what a diff is.
        let mut narrowing = Vec::new();
        if let Some(s) = w.get("state").and_then(Value::as_str) {
            narrowing.push("--state".to_string());
            narrowing.push(s.to_string());
        }
        if let Some(n) = w.get("context_lines").and_then(Value::as_u64) {
            narrowing.push("--context-lines".to_string());
            narrowing.push(n.to_string());
        }
        if let Some(r) = w.get("changed_since").and_then(Value::as_str) {
            narrowing.push("--changed-since".to_string());
            narrowing.push(r.to_string());
        }
        let narrowing_text = if narrowing.is_empty() {
            String::new()
        } else {
            format!(" {}", narrowing.join(" "))
        };
        let sample = req
            .changed_files
            .first()
            .map(String::as_str)
            .unwrap_or("<file>");
        tasks.push(Task {
            id: format!("jev-{}", slug(&check.id)),
            extension: "jev".to_string(),
            title: format!("Ask jev `{ask}` about {} file(s)", req.changed_files.len()),
            satisfies: vec![check.id.clone()],
            parallelizable: true,
            command: Some({
                let mut argv = vec!["musts-jev".to_string()];
                argv.extend(question_argv);
                argv.extend([
                    "--ask".to_string(),
                    ask.to_string(),
                    "--expect".to_string(),
                    expect.to_string(),
                    "--mode".to_string(),
                    mode.to_string(),
                ]);
                argv.extend(narrowing.iter().cloned());
                argv.push(sample.to_string());
                argv
            }),
            instructions: vec![
                format!("Run once per file in scope: `musts-jev {question} --ask {ask} --expect {expect} --mode {mode}{narrowing_text} {sample}`"),
                "Submit its output. The summary says how many files were judged and how many were not — a green with nothing judged proves nothing.".to_string(),
                "UNSURE is green: this reports what fired, never what is verified.".to_string(),
            ],
            evidence_contract: EvidenceContract {
                text: TextContract {
                    required: true,
                    description: Some("The run's summary line.".to_string()),
                },
                assets: vec![AssetContract {
                    kind: "log".to_string(),
                    required: true,
                    description: None,
                }],
            },
        });
    }
    Ok(ResolveResponse {
        protocol_version: 1,
        tasks,
        ignored_checks: vec![],
        notes: vec![
            "uses: jev records a measured verdict, not an attestation. Its green means \"nothing fired\", never \"verified\"."
                .to_string(),
        ],
    })
}

pub fn evidence(req: &EvidenceValidationRequest) -> Result<EvidenceValidationResponse, Error> {
    Ok(EvidenceValidationResponse {
        protocol_version: 1,
        accepted: true,
        satisfies: req.task.satisfies.clone(),
        summary: Some("jev verdicts recorded.".to_string()),
        normalized_assets: vec![],
        missing: vec![],
        message: None,
    })
}
