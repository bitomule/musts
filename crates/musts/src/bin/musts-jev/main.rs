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

mod jevkey;

use std::path::Path;
use std::process::ExitCode;

use serde_json::{json, Value};

const USAGE: &str = "usage: musts-jev --questions <path> --ask <id> [--expect yes|no] \
                     [--mode shadow|tripwire] [--root <dir>] [--json] <file>";

/// jevi's CLI silently truncates a state at 80_000 characters and says nothing about it in
/// its output: 120, 140 and 160 KiB of real source all came back with the same token count,
/// judged on the same prefix, with no field and no warning. The library does not apply that
/// cap — `ask` takes the state it is given — so the value here is a decision and not an
/// omission. Nothing in this codebase comes close: across 1,225 real source files in four
/// apps the largest was 97 KiB (25,018 tokens, 78% of the window) and not one exceeded it.
/// A file over this goes to jev whole and fails loudly, which is the honest outcome.
const MAX_STATE_CHARS: usize = 0; // 0 = never truncate

struct Args {
    /// Either a path to a question set, or the inline question assembled into one.
    questions: String,
    ask: String,
    expect: String,
    mode: String,
    root: String,
    /// The inline question object, verbatim from the manifest.
    inline: Option<String>,
    /// Emit one JSON object instead of the human line. `musts calibrate`
    /// reads this: parsing `p=Some(0.99)` out of a Debug-formatted
    /// `Option` is the kind of coupling that breaks on a refactor nobody
    /// connects to the breakage.
    json: bool,
    file: String,
}

fn parse_args() -> Result<Args, String> {
    let (mut questions, mut ask) = (String::new(), String::new());
    let mut expect = "yes".to_string();
    let mut mode = "shadow".to_string();
    let mut root = ".".to_string();
    let mut inline: Option<String> = None;
    let mut json = false;
    let mut files: Vec<String> = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut take = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--questions" => questions = take("--questions")?,
            "--question-json" => inline = Some(take("--question-json")?),
            "--ask" => ask = take("--ask")?,
            "--expect" => expect = take("--expect")?,
            "--mode" => mode = take("--mode")?,
            "--root" => root = take("--root")?,
            "--json" => json = true,
            o if o.starts_with("--") => return Err(format!("unknown flag {o}")),
            o => files.push(o.to_string()),
        }
    }
    if inline.is_some() && !questions.is_empty() {
        return Err("give --questions or --question-json, not both".into());
    }
    if (questions.is_empty() && inline.is_none()) || ask.is_empty() || files.len() != 1 {
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
        inline,
        ask,
        expect,
        mode,
        root,
        json,
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
    match std::env::args().nth(1).as_deref() {
        Some("resolve") => return protocol::resolve().map(|_| ExitCode::SUCCESS),
        Some("evidence") => return protocol::evidence().map(|_| ExitCode::SUCCESS),
        Some("set-key") => return jevkey::set_key().map(|_| ExitCode::SUCCESS),
        _ => {}
    }
    let args = parse_args()?;

    if let Some(reason) = unavailable() {
        if args.json {
            emit_json(&args.file, "skipped", None, None, Some(reason));
        } else {
            println!(
                "SKIPPED: {reason}. Nothing was judged; this check proves nothing about this change."
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    // An inline question is assembled into the very same document a file would hold, and
    // handed to jevi's own parser. Nothing about jev's format is reimplemented here, and the
    // two rules below apply to both spellings because they read the same object.
    let (raw, origin) = match &args.inline {
        Some(q) => {
            let question: Value =
                serde_json::from_str(q).map_err(|e| format!("inline question is not JSON: {e}"))?;
            let doc = json!({ "version": 1, "questions": { &args.ask: question } });
            (doc.to_string(), format!("{} (inline)", args.ask))
        }
        None => (
            std::fs::read_to_string(&args.questions)
                .map_err(|e| format!("cannot read {}: {e}", args.questions))?,
            args.questions.clone(),
        ),
    };
    vet(&raw, &args.ask)?;

    let prepared = jevi::QuestionSet::parse(&raw, &origin)
        .and_then(|s| s.prepare())
        .map_err(|e| format!("{}: {e}", e.kind()))?;
    // The key is musts' own, never jevi's: inheriting another tool's credential silently
    // is how "it works on my machine" starts. No key is a red, not a quiet pass.
    let Some((key, source)) = jevkey::resolve() else {
        return Err(jevkey::missing_key_message());
    };
    // SAFETY-ish: jevi reads the provider key from the environment, so hand it ours for this
    // process only. Nothing is written anywhere and no child inherits more than it already
    // would.
    std::env::set_var("OPENROUTER_API_KEY", &key);
    let cfg = jevi::Config::load().map_err(|e| format!("{}: {e}", e.kind()))?;
    eprintln!("musts-jev: key from {}", source.describe());
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
            if args.json {
                emit_json(&args.file, "skipped", None, None, Some(e.kind()));
            } else {
                println!("SKIP   {}  {}", args.file, e.kind());
                println!(
                    "\n0 judged, 1 not evaluated. This check proves nothing about this change."
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            if args.json {
                emit_json(&args.file, "broken", None, None, Some(e.kind()));
            } else {
                println!("BROKEN {}  {}: {e}", args.file, e.kind());
            }
            Ok(ExitCode::FAILURE)
        }
        Ok(answered) => {
            let idx = answered.names.iter().position(|n| n == &args.ask);
            let Some(o) = idx.and_then(|i| answered.outcomes.get(i)) else {
                if args.json {
                    emit_json(&args.file, "broken", None, None, Some("answer missing"));
                } else {
                    println!("BROKEN {}  answer for `{}` missing", args.file, args.ask);
                }
                return Ok(ExitCode::FAILURE);
            };
            let verdict = o.verdict.as_str();
            let model = answered.model.clone().unwrap_or_default();
            let decided = match verdict {
                "unsure" => "UNSURE",
                v if v == args.expect => "ok",
                _ => "FAIL",
            };
            if args.json {
                emit_json(&args.file, decided, o.number, Some(&model), None);
                // A machine reading this has the verdict in the object; a
                // second exit-code channel would only let the two disagree.
                return Ok(ExitCode::SUCCESS);
            }
            println!("{decided:6} {}  p={:?} {model}", args.file, o.number);

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

/// One object per call, for `musts calibrate` and anything else that has to
/// read a verdict rather than show it.
fn emit_json(file: &str, verdict: &str, p: Option<f64>, model: Option<&str>, note: Option<&str>) {
    println!(
        "{}",
        json!({ "file": file, "verdict": verdict, "p": p, "model": model, "note": note })
    );
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

/// The musts extension protocol: one JSON document in on stdin, one out on stdout.
///
/// `musts run` deliberately refuses to execute a descriptor-backed extension's command
/// (crates/musts-core/src/run.rs), so the task this emits is one the agent runs itself and
/// then submits. That is the honest shape until `jev` is registered as a core capability.
mod protocol {
    use serde_json::{json, Value};

    fn read_stdin() -> Result<Value, String> {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("cannot read request: {e}"))?;
        serde_json::from_str(&buf).map_err(|e| format!("request is not JSON: {e}"))
    }

    pub fn resolve() -> Result<(), String> {
        let req = read_stdin()?;
        let empty = vec![];
        let checks = req["checks"].as_array().unwrap_or(&empty);
        let files: Vec<&str> = req["changed_files"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let tasks: Vec<Value> = checks
            .iter()
            .map(|c| {
                let id = c["id"].as_str().unwrap_or("?");
                let w = &c["with"];
                let ask = w["ask"].as_str().unwrap_or("?");
                // Inline or a file — the same document reaches the same parser either way.
                let q = match w.get("question") {
                    Some(inline) => format!("--question-json '{}'", inline),
                    None => format!("--questions {}", w["questions"].as_str().unwrap_or("?")),
                };
                let expect = w["expect"].as_str().unwrap_or("yes");
                let mode = w["mode"].as_str().unwrap_or("shadow");
                let sample = files.first().copied().unwrap_or("<file>");
                json!({
                    "id": format!("jev-{}", id.replace(|ch: char| !ch.is_alphanumeric(), "-")),
                    "extension": req["capability"],
                    "title": format!("Ask jev `{ask}` about {} file(s)", files.len()),
                    "satisfies": [id],
                    "parallelizable": true,
                    "instructions": [
                        format!("Run this once per file in scope: `musts-jev {q} --ask {ask} --expect {expect} --mode {mode} {sample}`"),
                        "Submit its output. The summary line says how many files were judged and how many were not — a green with nothing judged proves nothing.".to_string(),
                        "UNSURE is green: this check reports what fired, never what is verified.".to_string(),
                    ],
                    "evidence_contract": {
                        "text": { "required": true, "description": "The run's summary line." },
                        "assets": [ { "kind": "log", "required": true } ]
                    }
                })
            })
            .collect();

        println!(
            "{}",
            json!({ "protocol_version": 1, "tasks": tasks,
                               "ignored_checks": [], "notes": [] })
        );
        Ok(())
    }

    pub fn evidence() -> Result<(), String> {
        let req = read_stdin()?;
        // A run that judged nothing is not evidence that anything is fine. Read the COUNT
        // out of the log, never the words: the first version looked for "judged" in the
        // submission text and accepted "0 of 0 files judged, nothing fired" — a green
        // recorded over zero files, which is exactly the coverage-that-isn't this capability
        // exists to avoid.
        let root = req["workspace_root"].as_str().unwrap_or(".");
        let judged: u64 = req["submission"]["assets"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|a| a["path"].as_str())
            .and_then(|p| std::fs::read_to_string(std::path::Path::new(root).join(p)).ok())
            .and_then(|log| {
                log.lines().rev().find_map(|l| {
                    let l = l.trim();
                    l.strip_suffix(" files produced a verdict line.")
                        .and_then(|head| head.split(" of ").next())
                        .and_then(|n| n.parse().ok())
                })
            })
            .unwrap_or(0);
        if judged == 0 {
            println!(
                "{}",
                json!({ "protocol_version": 1, "accepted": false,
                "missing": [{ "kind": "log",
                    "message": "The run judged nothing. Say why, or run it where jev is reachable." }],
                "message": "Nothing was judged." })
            );
            return Ok(());
        }
        println!(
            "{}",
            json!({ "protocol_version": 1, "accepted": true,
            "satisfies": req["task"]["satisfies"],
            "summary": "jev verdicts recorded.",
            "normalized_assets": [] })
        );
        Ok(())
    }
}
