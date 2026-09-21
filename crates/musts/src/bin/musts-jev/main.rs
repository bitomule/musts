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
                     [--mode shadow|tripwire] [--root <dir>] [--json] \
                     [--sites swift-analytics] [--changed-since <rev>] <file>";

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
    /// Which emission sites to cut the state down to. `None` sends the whole file, which is
    /// what every question before this one wanted.
    sites: Option<String>,
    /// A revision to diff against. Present, only the sites the change touched are judged.
    changed_since: Option<String>,
    file: String,
}

fn parse_args() -> Result<Args, String> {
    let (mut questions, mut ask) = (String::new(), String::new());
    let mut expect = "yes".to_string();
    let mut mode = "shadow".to_string();
    let mut root = ".".to_string();
    let mut inline: Option<String> = None;
    let mut json = false;
    let mut sites: Option<String> = None;
    let mut changed_since: Option<String> = None;
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
            "--sites" => sites = Some(take("--sites")?),
            "--changed-since" => changed_since = Some(take("--changed-since")?),
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
        sites,
        changed_since,
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

/// Question sets that ship with the binary, addressed as `builtin:<name>`.
///
/// A question asked by five repos is one text, not five copies of it. Measured here already:
/// removing one piece of a question took detection from 8/18 to 3/18 — and five editable
/// copies is the cheapest way to lose a piece without anyone noticing which repo lost it.
fn builtin_question_set(name: &str) -> Result<&'static str, String> {
    match name {
        "swift-analytics-privacy" => Ok(include_str!("questions/swift-analytics-privacy.json")),
        other => Err(format!(
            "no built-in question set `{other}`. Known: swift-analytics-privacy."
        )),
    }
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
        Some("sites") => return list_sites(),
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
        None => match args.questions.strip_prefix("builtin:") {
            Some(name) => (
                builtin_question_set(name)?.to_string(),
                args.questions.clone(),
            ),
            None => (
                std::fs::read_to_string(&args.questions)
                    .map_err(|e| format!("cannot read {}: {e}", args.questions))?,
                args.questions.clone(),
            ),
        },
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

    // What gets judged, and what a red is able to point at. Without `--sites` it is the whole
    // file, exactly as before. With it, one request per emission site: a question about a
    // call's arguments, asked about a 2,000-line feature file, is a question about 0.1% of
    // its state, and the verdict that comes back cannot say which line.
    //
    // Either way the state is only what is there — the path, the site, and the lines its
    // values come from. Nothing computed, nothing narrated. Measured: one unverified sentence
    // added to a state moved a probability from 0.06 to 0.74.
    let units: Vec<(String, Value)> = match args.sites.as_deref() {
        None => vec![(
            args.file.clone(),
            json!({ "path": args.file, "source": source }),
        )],
        Some("swift-analytics") => {
            let touched = match &args.changed_since {
                Some(r) => Some(changed_lines(&args.root, &args.file, r)?),
                None => None,
            };
            musts_core::sites::swift_analytics(&args.file, &source)
                .into_iter()
                .filter(|s| match &touched {
                    None => true,
                    Some(lines) => {
                        let span = s.line..=s.line + s.text.matches('\n').count();
                        lines.iter().any(|l| span.contains(l))
                    }
                })
                .map(|s| {
                    (
                        format!("{}:{}", args.file, s.line),
                        json!({ "path": args.file, "line": s.line, "kind": s.kind.as_str(),
                                "site": s.text, "lines_above": s.context }),
                    )
                })
                .collect()
        }
        Some(other) => return Err(format!("unknown --sites selector `{other}`")),
    };

    // Not an error and not a pass. A file can be in a check's `paths:` and contain nothing
    // this question is about, and saying "ok" there is the lie this whole capability is built
    // to avoid.
    if units.is_empty() {
        if !args.json {
            println!(
                "\n0 judged, 0 not evaluated. No emission site in scope; this check proves nothing about this change."
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    let mut judged = 0usize;
    let mut not_evaluated = 0usize;
    let mut fired = false;

    for (label, state) in &units {
        match jevi::ask(&cfg, &prepared, state, &opts) {
            // Infrastructure degrades; a malformed request is our own bug and goes red. Two
            // shapes of failure that a single `ok: false` used to flatten into one, which left
            // a permanently broken check reporting that all was well.
            Err(e @ jevi::Error::NoAnswer { .. }) => {
                not_evaluated += 1;
                if args.json {
                    emit_json(label, "skipped", None, None, Some(e.kind()));
                } else {
                    println!("SKIP   {label}  {}", e.kind());
                }
            }
            Err(e) => {
                if args.json {
                    emit_json(label, "broken", None, None, Some(e.kind()));
                } else {
                    println!("BROKEN {label}  {}: {e}", e.kind());
                }
                return Ok(ExitCode::FAILURE);
            }
            Ok(answered) => {
                let idx = answered.names.iter().position(|n| n == &args.ask);
                let Some(o) = idx.and_then(|i| answered.outcomes.get(i)) else {
                    if args.json {
                        emit_json(label, "broken", None, None, Some("answer missing"));
                    } else {
                        println!("BROKEN {label}  answer for `{}` missing", args.ask);
                    }
                    return Ok(ExitCode::FAILURE);
                };
                judged += 1;
                let verdict = o.verdict.as_str();
                let model = answered.model.clone().unwrap_or_default();
                // Four labels, not three, and the fourth is the one that was missing.
                //
                // Measured on one planted violation run six times: p came back 0.89, 0.89,
                // 0.90, 0.90, 0.91, 0.91 — straddling the abstention band's edge, so the same
                // file read FAIL four times and UNSURE twice. Printed as `UNSURE` it is
                // indistinguishable from the clean twin of that file, which answers 0.13. A
                // report where "almost certainly a violation" and "certainly not" share a word
                // is a report nobody can act on.
                //
                // `WARN` is not a tuned cut and nothing is fitted to produce it: it is which
                // side of even an abstention fell on, and it moves no exit code. `fired` below
                // still takes only a decided verdict, so tripwire behaves exactly as before.
                let leaning_violation = o.number.is_some_and(|p| {
                    if args.expect == "no" {
                        p >= 0.5
                    } else {
                        p < 0.5
                    }
                });
                let decided = match verdict {
                    "unsure" if leaning_violation => "WARN",
                    "unsure" => "UNSURE",
                    v if v == args.expect => "ok",
                    _ => "FAIL",
                };
                if decided == "FAIL" {
                    fired = true;
                }
                if args.json {
                    emit_json(label, decided, o.number, Some(&model), None);
                    continue;
                }
                println!("{decided:6} {label}  p={:?} {model}", o.number);

                if args.mode == "shadow" {
                    let dir = Path::new(&args.root).join(".musts/jev-shadow");
                    let _ = std::fs::create_dir_all(&dir);
                    let row = json!({ "file": label, "question": args.ask,
                                      "verdict": verdict, "p": o.number, "model": model });
                    use std::io::Write;
                    if let Ok(mut h) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(dir.join(format!("{}.jsonl", args.ask)))
                    {
                        let _ = writeln!(h, "{row}");
                    }
                }
            }
        }
    }

    if args.json {
        // A machine reading this has every verdict in the objects above; a second
        // exit-code channel would only let the two disagree.
        return Ok(ExitCode::SUCCESS);
    }

    // The denominator, always. A green with nothing judged is the failure mode this line
    // exists to make impossible to miss.
    println!("\n{judged} judged, {not_evaluated} not evaluated.");

    if args.mode == "shadow" {
        println!("SHADOW: recorded. Nothing granted, nothing blocked.");
        return Ok(ExitCode::SUCCESS);
    }

    // Two states. `unsure` is green. That is honest only because this check's green
    // means "nothing fired", never "verified": measured, it abstains on 14% of the
    // files it judges, and a quiet tripwire proves nothing.
    Ok(if fired {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// `musts-jev sites [--root <dir>] [--changed-since <rev>] <file>...` — what the run would
/// ask about, without asking and without spending anything.
///
/// This is the only way to see the guard's scope separately from its judgment, which is the
/// difference between "it found nothing" and "it looked at nothing". Both print a green, and
/// they are not the same result.
fn list_sites() -> Result<ExitCode, String> {
    let mut root = ".".to_string();
    let mut since: Option<String> = None;
    let mut files: Vec<String> = Vec::new();
    let mut it = std::env::args().skip(2);
    while let Some(arg) = it.next() {
        let mut take = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--root" => root = take("--root")?,
            "--changed-since" => since = Some(take("--changed-since")?),
            o if o.starts_with("--") => return Err(format!("unknown flag {o}")),
            o => files.push(o.to_string()),
        }
    }
    if files.is_empty() {
        return Err(
            "usage: musts-jev sites [--root <dir>] [--changed-since <rev>] <file>...".into(),
        );
    }
    let mut total = 0usize;
    for file in &files {
        let source = std::fs::read_to_string(Path::new(&root).join(file))
            .map_err(|e| format!("cannot read {file}: {e}"))?;
        let touched = match &since {
            Some(r) => Some(changed_lines(&root, file, r)?),
            None => None,
        };
        for s in musts_core::sites::swift_analytics(file, &source) {
            if let Some(lines) = &touched {
                let span = s.line..=s.line + s.text.matches('\n').count();
                if !lines.iter().any(|l| span.contains(l)) {
                    continue;
                }
            }
            total += 1;
            println!(
                "{file}:{}  {}  {}",
                s.line,
                s.kind.as_str(),
                s.text
                    .replace('\n', " ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
    }
    println!("\n{total} site(s) in {} file(s).", files.len());
    Ok(ExitCode::SUCCESS)
}

/// The lines `<file>` gained or changed since `<ref>`, 1-indexed against the working tree.
///
/// Requirement one of this capability: judge the sites a change touched, not every site in
/// every file it happened to be in. A one-line edit to a 214-site file is 214 requests
/// without this and one with it.
///
/// A `git` that fails is a red, not a silent fallback to judging everything: paying for 214
/// requests because a ref was misspelled is exactly the surprise nobody budgets for.
fn changed_lines(root: &str, file: &str, since: &str) -> Result<Vec<usize>, String> {
    let out = std::process::Command::new("git")
        .args(["-C", root, "diff", "--unified=0", since, "--", file])
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git diff {since} -- {file} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut lines = Vec::new();
    for hunk in String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("@@"))
    {
        // `@@ -a,b +c,d @@` — only the `+` side, which is the tree as it is now.
        let Some(plus) = hunk.split('+').nth(1) else {
            continue;
        };
        let plus = plus.split([' ', '@']).next().unwrap_or("");
        let mut parts = plus.split(',');
        let Some(start) = parts.next().and_then(|s| s.parse::<usize>().ok()) else {
            continue;
        };
        let count = parts
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1);
        lines.extend(start..start + count);
    }
    Ok(lines)
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
