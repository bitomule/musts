//! `harness` CLI entry point.
//!
//! Phase 1 supports just enough to drive the "discovery + state" smoke
//! test: `harness validate` resolves the workspace, walks for manifests,
//! and reports either a clean state (no manifests) or a deliberate
//! Phase-1 placeholder error when manifests do exist (extension loading
//! lands in Phase 2 per `docs/PLAN.md` §9 Phase 1).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use harness_core::manifest::discover;
use harness_core::workspace;

#[derive(Debug, Parser)]
#[command(name = "harness", version, about = "Agent-first validation loop", long_about = None)]
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
        /// Emit a machine-readable JSON report (shape stable from first ship).
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.log.as_deref());

    match run(&cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            // anyhow swallows the source chain into the formatted error;
            // exit 70 is the catch-all for internal errors per PLAN.md §5.
            ExitCode::from(70)
        }
    }
}

fn run(cli: &Cli) -> anyhow::Result<ExitCode> {
    match &cli.command {
        Command::Validate { json } => validate_command(cli.workspace.as_deref(), *json),
    }
}

fn validate_command(
    explicit_workspace: Option<&std::path::Path>,
    json: bool,
) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir()?;
    let root = match workspace::resolve(explicit_workspace, &cwd) {
        Ok(root) => root,
        Err(err) => {
            eprintln!("error: {err}");
            return Ok(ExitCode::from(err.exit_code() as u8));
        }
    };

    let manifests = match discover(&root) {
        Ok(m) => m,
        Err(err) => {
            eprintln!("error: {err}");
            return Ok(ExitCode::from(err.exit_code() as u8));
        }
    };

    // Phase 1: empty workspace = clean; manifests present = Phase-1
    // placeholder error (the orchestrator is wired up in Phase 3).
    if manifests.is_empty() {
        if json {
            print_json_clean(&root);
        } else {
            println!("Harness validation clean. No HARNESS.yml files found.");
        }
        return Ok(ExitCode::from(0));
    }

    let message = format!(
        "Phase 1 only — extension loading lands in Phase 2.\n\
         Discovered {} HARNESS.yml file(s) but cannot resolve checks yet.",
        manifests.len()
    );
    eprintln!("error: {message}");
    Ok(ExitCode::from(2))
}

fn print_json_clean(workspace_root: &std::path::Path) {
    // Stable shape per PLAN.md §5 (--json contract). Clean = empty arrays
    // for tasks/ignored_checks/notes; no synthetic entries.
    let doc = serde_json::json!({
        "protocol_version": 1,
        "status": "clean",
        "workspace_root": workspace_root.display().to_string(),
        "tasks": [],
        "ignored_checks": [],
        "notes": []
    });
    println!("{}", serde_json::to_string_pretty(&doc).unwrap());
}

fn init_logging(filter: Option<&str>) {
    let env = filter.unwrap_or("warn");
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(env))
        .with_writer(std::io::stderr)
        .try_init();
}
