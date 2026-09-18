//! `musts-jev` — the judgment runner behind `uses: jev`.
//!
//! It exists so that `musts-core` never talks to the network: the core stays a set of pure
//! functions that declare a command, exactly as `bazel/build` declares `bazel build`. This
//! binary is what that command points at, and it is the only part of musts that calls jev.
//!
//! Two states, and the rules that make them honest, are in
//! `docs/examples/jev-check/DESIGN.md`. Each one is enforced here rather than documented,
//! because a rule a format permits breaking will be broken.

mod facts;

use std::path::Path;
use std::process::ExitCode;

use serde_json::{json, Value};

const USAGE: &str = "usage: musts-jev --questions <path> --ask <id> [--expect yes|no] \
                     [--mode shadow|tripwire] [--root <dir>] <file>...";

struct Args {
    questions: String,
    ask: String,
    expect: String,
    mode: String,
    root: String,
    files: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        questions: String::new(),
        ask: String::new(),
        expect: "yes".into(),
        mode: "shadow".into(),
        root: ".".into(),
        files: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut take = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--questions" => a.questions = take("--questions")?,
            "--ask" => a.ask = take("--ask")?,
            "--expect" => a.expect = take("--expect")?,
            "--mode" => a.mode = take("--mode")?,
            "--root" => a.root = take("--root")?,
            other if other.starts_with("--") => return Err(format!("unknown flag {other}")),
            other => a.files.push(other.to_string()),
        }
    }
    if a.questions.is_empty() || a.ask.is_empty() {
        return Err(USAGE.into());
    }
    if a.expect != "yes" && a.expect != "no" {
        return Err("--expect takes yes or no".into());
    }
    // There is deliberately no `gate`: a capability whose green would mean "verified" cannot
    // be built on a model that abstains, and an abstention routed to an agent costs more than
    // the agent check it replaces.
    if a.mode != "shadow" && a.mode != "tripwire" {
        return Err(format!(
            "--mode takes shadow or tripwire; `{}` is not a mode. There is no gate.",
            a.mode
        ));
    }
    Ok(a)
}

/// The two rules a question set could otherwise break without the manifest ever seeing it.
fn vet_question_set(raw: &str, ask: &str) -> Result<(), String> {
    let doc: Value =
        serde_json::from_str(raw).map_err(|e| format!("question set is not JSON: {e}"))?;
    let q = doc
        .get("questions")
        .and_then(|q| q.get(ask))
        .ok_or_else(|| format!("question `{ask}` is not in this set"))?;

    // Criteria are mandatory. Measured: dropping them took detection from 8/18 to 3/18 and
    // made every healthy file abstain. jevi leaves them optional on purpose — the tool
    // permits, the policy forbids — so the policy is applied here, where it can be audited.
    let c = q.get("criteria");
    let both = c
        .map(|c| c.get("true").is_some() && c.get("false").is_some())
        .unwrap_or(false);
    if !both {
        return Err(format!(
            "question `{ask}` has no criteria block with both branches. uses: jev refuses to run without one."
        ));
    }

    // No numeric threshold. The `with` schema has no probability field, but that alone is not
    // enforcement: the question set carries its own `decide` block and jevi honours it, which
    // is a back door the schema never sees. A rule only one of two files can express is not
    // enforced. Measured: a cut fitted on 40 rows with zero errors made 0.41 wrong answers per
    // 20 on a holdout, wrong at least once in 39% of splits.
    if q.get("decide").is_some() {
        return Err(format!(
            "question `{ask}` carries a `decide` block. uses: jev runs on default thresholds; a cut tuned on tens of rows does not survive a holdout."
        ));
    }
    Ok(())
}

/// Not for CI, and never load-bearing.
fn unavailable() -> Option<&'static str> {
    if std::env::var_os("CI").is_some() {
        return Some("CI is set");
    }
    if std::env::var_os("JEVI_DISABLE").is_some() {
        return Some("JEVI_DISABLE is set");
    }
    None
}

fn run() -> Result<ExitCode, String> {
    let args = parse_args()?;

    if let Some(reason) = unavailable() {
        println!(
            "SKIPPED: {reason}. Nothing was judged; this check proves nothing about this change."
        );
        return Ok(ExitCode::SUCCESS);
    }

    let raw = std::fs::read_to_string(&args.questions)
        .map_err(|e| format!("cannot read {}: {e}", args.questions))?;
    vet_question_set(&raw, &args.ask)?;

    let prepared = jevi::QuestionSet::parse(&raw, &args.questions)
        .and_then(|s| s.prepare())
        .map_err(|e| format!("{}: {e}", e.kind()))?;
    let cfg = jevi::Config::load().map_err(|e| format!("{}: {e}", e.kind()))?;
    let opts = jevi::AskOptions {
        provider: None,
        model: None,
        timeout_ms: None,
        state_chars: None,
    };

    let (mut ok, mut red, mut unsure, mut skipped, mut na) = (0, 0, 0, 0, 0);
    let mut rows: Vec<String> = Vec::new();

    for f in &args.files {
        let path = Path::new(&args.root).join(f);
        let Ok(source) = std::fs::read_to_string(&path) else {
            println!("SKIP   {f}  unreadable");
            skipped += 1;
            continue;
        };
        let computed = facts::rust_tests(&source);

        // The filter runs BEFORE the request, not after.
        if computed.get("applicable") == Some(&Value::Bool(false)) {
            na += 1;
            continue;
        }

        let state = json!({ "path": f, "source": source, "facts": computed });
        match jevi::ask(&cfg, &prepared, &state, &opts) {
            // Infrastructure degrades; a malformed request is our own bug and goes red. Two
            // shapes of failure that `ok: false` used to flatten into one, which left a
            // permanently broken check reporting that all was well.
            Err(e @ jevi::Error::NoAnswer { .. }) => {
                println!("SKIP   {f}  {}", e.kind());
                skipped += 1;
            }
            Err(e) => {
                println!("BROKEN {f}  {}: {e}", e.kind());
                red += 1;
            }
            Ok(answered) => {
                let idx = answered.names.iter().position(|n| n == &args.ask);
                let Some(o) = idx.and_then(|i| answered.outcomes.get(i)) else {
                    println!("BROKEN {f}  answer for `{}` missing", args.ask);
                    red += 1;
                    continue;
                };
                let v = o.verdict.as_str();
                let model = answered.model.clone().unwrap_or_default();
                if args.mode == "shadow" {
                    rows.push(
                        json!({ "file": f, "question": args.ask, "verdict": v,
                                "p": o.number, "model": model, "facts": state["facts"] })
                        .to_string(),
                    );
                }
                match v {
                    "unsure" => {
                        unsure += 1;
                        println!("UNSURE {f}  p={:?} {model}", o.number);
                    }
                    x if x == args.expect => {
                        ok += 1;
                        println!("ok     {f}  p={:?} {model}", o.number);
                    }
                    _ => {
                        red += 1;
                        println!("FAIL   {f}  p={:?} {model}", o.number);
                    }
                }
            }
        }
    }

    println!(
        "\n{ok} ok, {red} failed, {unsure} unsure, {skipped} not evaluated, {na} not applicable"
    );

    if args.mode == "shadow" {
        let dir = Path::new(&args.root).join(".musts/jev-shadow");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join(format!("{}.jsonl", args.ask));
        if !rows.is_empty() {
            use std::io::Write;
            if let Ok(mut h) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file)
            {
                for r in &rows {
                    let _ = writeln!(h, "{r}");
                }
            }
        }
        // Shadow promises not to block on a VERDICT. It does not promise to hide its own
        // breakage: a misconfigured shadow that records nothing forever is the failure mode
        // of every channel that only ever carries "all clear".
        if red > 0 {
            println!(
                "SHADOW IS BROKEN: {red} files failed for a configuration reason, not a verdict."
            );
            return Ok(ExitCode::FAILURE);
        }
        println!(
            "SHADOW: recorded {} rows. Nothing granted, nothing blocked.",
            rows.len()
        );
        return Ok(ExitCode::SUCCESS);
    }

    if ok + red + unsure == 0 {
        println!("Nothing was judged. This check proves nothing about this change.");
        return Ok(ExitCode::SUCCESS);
    }
    // Two states. `unsure` is green, and so is a file nothing could be judged on. That is
    // honest only because this check's green means "nothing fired", never "verified".
    Ok(if red > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("BROKEN: {msg}");
            ExitCode::FAILURE
        }
    }
}
