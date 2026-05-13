//! `musts` CLI entry point.
//!
//! Phase 3 wires the validate orchestrator behind the `validate`
//! subcommand. Empty workspaces (no `MUSTS.yml` anywhere) short-
//! circuit to a clean report before any state is created. Everything
//! else goes through the orchestrator and acquires the cross-process
//! lock per `docs/PLAN.md` §4.5.1.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use musts_core::bootstrap::StateSession;
use musts_core::evidence::{submit, EvidenceSubmissionResult};
use musts_core::extension::runtime::RuntimeOptions;
use musts_core::manifest::discover as discover_manifests;
use musts_core::report::{render_json, render_text, ValidateReport};
use musts_core::validate::{self, ValidateOptions};
use musts_core::workspace;
use musts_core::Error;

#[derive(Debug, Parser)]
#[command(name = "musts", version, about = "Agent-first validation loop", long_about = None)]
struct Cli {
    /// Override the workspace root. Useful inside submodules and CI.
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,

    /// Log filter (`error`, `warn`, `info`, `debug`, or RUST_LOG-style).
    #[arg(long, global = true)]
    log: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report pending validation tasks for the current workspace state.
    Validate {
        /// Emit a machine-readable JSON report (shape frozen at first ship).
        #[arg(long)]
        json: bool,
    },
    /// Record evidence for a task issued by the most recent `musts validate`.
    Evidence {
        /// Task id from the validate report.
        task_id: String,
        /// Freeform summary of the validation result.
        #[arg(long)]
        text: Option<String>,
        /// Asset file path. Repeat for multiple assets.
        #[arg(long = "asset", value_name = "PATH")]
        assets: Vec<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.log.as_deref());

    match run(&cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(70)
        }
    }
}

fn run(cli: &Cli) -> anyhow::Result<ExitCode> {
    match &cli.command {
        Command::Validate { json } => validate_command(cli.workspace.as_deref(), *json),
        Command::Evidence {
            task_id,
            text,
            assets,
        } => evidence_command(cli.workspace.as_deref(), task_id, text.as_deref(), assets),
    }
}

fn validate_command(
    explicit_workspace: Option<&std::path::Path>,
    json: bool,
) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir()?;
    let root = match workspace::resolve(explicit_workspace, &cwd) {
        Ok(root) => root,
        Err(err) => return Ok(report_error(err)),
    };

    // Short-circuit on empty workspace before creating any state.
    let manifests = match discover_manifests(&root) {
        Ok(m) => m,
        Err(err) => return Ok(report_error(err)),
    };
    if manifests.is_empty() {
        let report = ValidateReport {
            workspace_root: root.display().to_string(),
            tasks: vec![],
            ignored_checks: vec![],
            notes: vec![],
        };
        if json {
            println!("{}", serde_json::to_string_pretty(&render_json(&report))?);
        } else {
            println!("Musts validation clean. No MUSTS.yml files found.");
        }
        return Ok(ExitCode::from(0));
    }

    let mut session = match StateSession::acquire(&root) {
        Ok(s) => s,
        Err(err) => return Ok(report_error(err)),
    };
    let options = ValidateOptions {
        workspace_root: root.clone(),
        runtime_options: RuntimeOptions::from_env(root.clone()),
    };
    let report = match validate::run(&mut session, &options) {
        Ok(r) => r,
        Err(err) => return Ok(report_error(err)),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&render_json(&report))?);
    } else {
        print!("{}", render_text(&report));
    }

    // Exit code per PLAN.md §5: 0 when clean, 1 when pending.
    if report.is_clean() {
        Ok(ExitCode::from(0))
    } else {
        Ok(ExitCode::from(1))
    }
}

fn evidence_command(
    explicit_workspace: Option<&std::path::Path>,
    task_id: &str,
    text: Option<&str>,
    assets: &[PathBuf],
) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir()?;
    let root = match workspace::resolve(explicit_workspace, &cwd) {
        Ok(r) => r,
        Err(err) => return Ok(report_error(err)),
    };
    let mut session = match StateSession::acquire(&root) {
        Ok(s) => s,
        Err(err) => return Ok(report_error(err)),
    };
    let runtime_options = RuntimeOptions::from_env(root.clone());
    let asset_refs: Vec<&std::path::Path> = assets.iter().map(|p| p.as_path()).collect();
    let inputs = musts_core::evidence::submit::SubmissionInputs {
        task_id,
        text,
        asset_paths: &asset_refs,
    };
    match submit(&mut session, &root, &runtime_options, &inputs) {
        Ok(result) => {
            print_evidence_result(&result);
            Ok(ExitCode::from(0))
        }
        Err(err) => Ok(report_error(err)),
    }
}

fn print_evidence_result(result: &EvidenceSubmissionResult) {
    println!(
        "Evidence accepted for `{}` (submission {}).",
        result.task_id, result.submission_id
    );
    if !result.satisfied.is_empty() {
        println!("Satisfied:");
        for s in &result.satisfied {
            println!("  - {s}");
        }
    }
    if let Some(summary) = &result.summary {
        println!("Summary: {summary}");
    }
    println!("\nRun `musts validate` again to confirm the report is now clean.");
}

fn report_error(err: Error) -> ExitCode {
    let exit = err.exit_code();
    eprintln!("error: {err}");
    ExitCode::from(exit as u8)
}

fn init_logging(filter: Option<&str>) {
    let env = filter.unwrap_or("warn");
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(env))
        .with_writer(std::io::stderr)
        .try_init();
}
