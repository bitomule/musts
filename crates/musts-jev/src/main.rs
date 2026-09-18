//! `musts-jev` — the judgment runner behind `uses: jev`.
//!
//! It is a thin wrapper over what jev already accepts: the repo writes the question, this
//! sends one file and reads the verdict back. It knows nothing about any domain — no
//! language, no defect, no heuristics. What lives here are the rules that hold for every
//! question, and they are enforced rather than documented, because a rule a format permits
//! breaking will be broken.
//!
//! It exists as a binary so that `musts-core` never talks to the network: the core stays a
//! set of pure functions that declare a command, exactly as `bazel/build` declares
//! `bazel build`.
//!
//! What it deliberately does NOT do: run any program supplied by the repo, compute facts
//! about the code, or look at more than one file per call. The consequence is written down
//! rather than hidden — anything that depends on execution cannot be judged this way, and
//! measured, jev abstains 100% of the time on those. A repo that needs computed facts should
//! produce them with a `bash/check` and leave the result where its question can read it.

use std::path::Path;
use std::process::ExitCode;

use serde_json::{json, Value};

const USAGE: &str = "usage: musts-jev --questions <path> --ask <id> [--expect yes|no] \
                     [--mode shadow|tripwire] [--root <dir>] <file>";

/// jevi's CLI silently truncates a state at 80_000 characters and says nothing about it in
/// its output: 120, 140 and 160 KiB of real source all came back with the same token count,
/// judged on the same prefix, with no field and no warning. The library does not apply that
/// cap — `ask` takes the state it is given — so the value here is a decision and not an
/// omission. Nothing in this codebase comes close: across 1,225 real source files in four
/// apps the largest was 97 KiB (25,018 tokens, 78% of the window) and not one exceeded it.
/// A file over this goes to jev whole and fails loudly, which is the honest outcome.
const MAX_STATE_CHARS: usize = 0; // 0 = never truncate

struct Args {
    questions: String,
    ask: String,
    expect: String,
    mode: String,
    root: String,
    file: String,
}

fn parse_args() -> Result<Args, String> {
    let (mut questions, mut ask) = (String::new(), String::new());
    let mut expect = "yes".to_string();
    let mut mode = "shadow".to_string();
    let mut root = ".".to_string();
    let mut files: Vec<String> = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut take = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--questions" => questions = take("--questions")?,
            "--ask" => ask = take("--ask")?,
            "--expect" => expect = take("--expect")?,
            "--mode" => mode = take("--mode")?,
            "--root" => root = take("--root")?,
            o if o.starts_with("--") => return Err(format!("unknown flag {o}")),
            o => files.push(o.to_string()),
        }
    }
    if questions.is_empty() || ask.is_empty() || files.len() != 1 {
        // One file per call — not because of the context window, which is nowhere near
        // reached, but because these questions are answered by reading one file and a red
        // has to say which one.
        return Err(USAGE.into());
    }
    if expect != "yes" && expect != "no" {
        return Err("--expect takes yes or no".into());
    }
    // There is deliberately no `gate`: a capability whose green would mean "verified" cannot
    // be built on a model that abstains, and an abstention routed to an agent costs more than
    // the agent check it would replace.
    if mode != "shadow" && mode != "tripwire" {
        return Err(format!(
            "--mode takes shadow or tripwire; `{mode}` is not a mode. There is no gate."
        ));
    }
    Ok(Args {
        questions,
        ask,
        expect,
        mode,
        root,
        file: files.remove(0),
    })
}

/// The two rules a repo's own question set could otherwise break without the manifest ever
/// seeing it. Both are refusals, not warnings.
fn vet(raw: &str, ask: &str) -> Result<(), String> {
    let doc: Value =
        serde_json::from_str(raw).map_err(|e| format!("question set is not JSON: {e}"))?;
    let q = doc
        .get("questions")
        .and_then(|q| q.get(ask))
        .ok_or_else(|| format!("question `{ask}` is not in this set"))?;

    // Criteria are mandatory. Measured: dropping them took detection from 8/18 to 3/18 and
    // made every healthy file abstain. jevi leaves them optional on purpose — the tool
    // permits, the policy forbids — so the policy is applied here, where it can be audited.
    let both = q
        .get("criteria")
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
    // enforced. Measured: a cut fitted on 40 rows with zero errors made 0.41 wrong answers
    // per 20 on a holdout, wrong at least once in 39% of splits.
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
    vet(&raw, &args.ask)?;

    let prepared = jevi::QuestionSet::parse(&raw, &args.questions)
        .and_then(|s| s.prepare())
        .map_err(|e| format!("{}: {e}", e.kind()))?;
    let cfg = jevi::Config::load().map_err(|e| format!("{}: {e}", e.kind()))?;
    let opts = jevi::AskOptions {
        provider: None,
        model: None,
        timeout_ms: None,
        state_chars: if MAX_STATE_CHARS == 0 {
            None
        } else {
            Some(MAX_STATE_CHARS)
        },
    };

    let path = Path::new(&args.root).join(&args.file);
    let source = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;

    // The whole state: the file and its path. Nothing computed, nothing narrated. Measured:
    // one unverified sentence added to a state moved a probability from 0.06 to 0.74.
    let state = json!({ "path": args.file, "source": source });

    match jevi::ask(&cfg, &prepared, &state, &opts) {
        // Infrastructure degrades; a malformed request is our own bug and goes red. Two
        // shapes of failure that a single `ok: false` used to flatten into one, which left a
        // permanently broken check reporting that all was well.
        Err(e @ jevi::Error::NoAnswer { .. }) => {
            println!("SKIP   {}  {}", args.file, e.kind());
            println!("\n0 judged, 1 not evaluated. This check proves nothing about this change.");
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            println!("BROKEN {}  {}: {e}", args.file, e.kind());
            Ok(ExitCode::FAILURE)
        }
        Ok(answered) => {
            let idx = answered.names.iter().position(|n| n == &args.ask);
            let Some(o) = idx.and_then(|i| answered.outcomes.get(i)) else {
                println!("BROKEN {}  answer for `{}` missing", args.file, args.ask);
                return Ok(ExitCode::FAILURE);
            };
            let verdict = o.verdict.as_str();
            let model = answered.model.clone().unwrap_or_default();
            println!(
                "{:6} {}  p={:?} {model}",
                match verdict {
                    "unsure" => "UNSURE",
                    v if v == args.expect => "ok",
                    _ => "FAIL",
                },
                args.file,
                o.number
            );

            if args.mode == "shadow" {
                let dir = Path::new(&args.root).join(".musts/jev-shadow");
                let _ = std::fs::create_dir_all(&dir);
                let row = json!({ "file": args.file, "question": args.ask,
                                  "verdict": verdict, "p": o.number, "model": model });
                use std::io::Write;
                if let Ok(mut h) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join(format!("{}.jsonl", args.ask)))
                {
                    let _ = writeln!(h, "{row}");
                }
                println!("SHADOW: recorded. Nothing granted, nothing blocked.");
                return Ok(ExitCode::SUCCESS);
            }

            // Two states. `unsure` is green. That is honest only because this check's green
            // means "nothing fired", never "verified": measured, it abstains on 14% of the
            // files it judges, and a quiet tripwire proves nothing.
            Ok(if verdict != "unsure" && verdict != args.expect {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
    }
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
