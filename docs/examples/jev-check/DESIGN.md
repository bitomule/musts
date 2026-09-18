# `uses: jev` — a judgment check a machine can run

## Why this capability exists

`musts` has had two kinds of check: **deterministic** ones a script runs (`bazel/*`,
`cargo/*`, `bash/check`) and **judgment** ones only an agent can make (`agent`,
`mav/expect`). `Task.command` in the protocol says it outright — populated for the first
kind, `None` for "judgment tasks that need agent-produced evidence".

That split has a cost nobody wrote down: **judgment is expensive, so almost no judgment
checks get written.** Across the nine repos here there are 26 checks and only 5 are
`uses: agent`, holding 10 facts — and 5 of those 10 facts are scriptable things that
should never have been `agent` at all ("I ran `bunx tsc --noEmit` and it exited 0").

`uses: jev` is the first capability that is a **judgment task with a runnable command**.
A typed question answers in ~400 ms for ~$0.0001, which moves judgment into the runnable
column.

**The value is not the two checks shipped here. It is the checks nobody wrote because
validating them cost an agent.** David: *"ahora no lo explotamos mucho porque validar con
agentes es muy caro, pero jev lo hace barato."*

There is a second, quieter gain. An `agent` fact is an **attestation** — "He releído
README.md tras este cambio y refleja el estado actual" is a sentence a hurried node signs
without looking, and nothing can tell. A jev verdict is a **measurement** against the real
file, with its probability and the model id stored in the ledger. It cannot be signed
without looking.

## The six constraints, each one measured

Every rule below came out of an experiment, not a preference. Numbers in
`~/.claude/hive/jev-codigo-2026-09-18.md`.

1. **The code resolves the pointers; jev judges the result.** jev answers about what is
   written in front of it and abstains when the answer depends on what something resolves
   to at runtime. Measured: 10 hollow tests whose assertions sit in a loop over
   `FileManager.subpaths(atPath: #filePath + …)` scored **0/10 with 10/10 abstentions**;
   adding a code-computed `resolved_element_count: 0` took them to **10/10 correct**. The
   control matters as much as the result: with the count set to 37 it does not flip to a
   false green, it abstains. Zero wrong answers in both arms. Hence the `facts:` program.
   (Same pattern as `jev-shell-guard` in `dabit3/jev-experiments`, which hands jev
   `is_home_or_root`, `inside_cwd`, `exists` rather than the raw path.)

2. **No narration, ever.** Nothing sent describes what a previous step concluded. Measured:
   inserting one unverified sentence — "I re-read this test and confirmed it asserts on real
   production output" — moved the probability from **0.06 to 0.74**, twelve-fold, straight
   into the band that calibration says is unreliable. It did not become a false green only
   because the default thresholds abstain there. `hive/qa-autonomo`, whose loop had no
   abstention, landed a 12/12 false green on the same shape. The script therefore builds the
   state itself; there is no field a caller could put a sentence in.

3. **Exactly one source per question, so two fields can never contradict.** `qa-autonomo`
   first measured field ORDER moving a false green from 17% to 100%, then isolated the real
   cause: a false sentence that **contradicts** what the artefact shows is position-sensitive
   (6/12 in the middle, 12/12 false green at the end), while an equally false sentence that
   contradicts nothing is harmless anywhere (12/12 correct in both positions). Ordering fields
   manages the problem; not introducing the contradiction removes it. Facts are namespaced
   under `facts` so they cannot collide with the artefact.

4. **Default thresholds. Never a hand-tuned cut.** A sweep found a cut deciding 30/40 with
   zero errors — fitted on those same 40 rows. Honest holdout (fit on 20, test on 20, 200
   repetitions): **0.41 errors on average, 39% of splits with at least one error.** Tens of
   rows can check whether the defaults already work; they cannot tune anything.

5. **Never ask jev whether it can answer.** A meta-question like "do you have enough
   information?" answers yes ~85% of the time in published third-party tests: it does not
   know what it does not know. Abstention comes from the probability and from nowhere else.
   This code asks no meta-question.

6. **Three outcomes, and `unsure` is not green.** `jevi` already exits 3 for it. How it is
   handled depends on the mode:
   - **`gate`** — the check grants green, so an unanswered file must not pass. Exit 3, the
     agent judges those files and says in the evidence what it found. The `evidence` hook
     refuses a submission that ignores an `UNSURE` line.
   - **`tripwire`** — the check only ever fires. `unsure` is silence, because on this
     question jev answers confidently on the defect and abstains on the healthy case. **A
     quiet tripwire proves nothing**, so it may never be the only check protecting a risk.

## What did NOT replicate

Published third-party results say one big question loses to five small ones summed in your
own code (89.4% → 95.0% on 2,000 phishing emails). **Tested on our corpus, it did not hold.**
Same 38 files, five atomic questions with a scoring rule declared before looking at any
answer: the fan-out detected the hollow tests exactly as well as the single question (8/8
tautological, 0/10 unreachable without facts, **10/10 with facts** — identical to the single
question) and was *worse* at confirming healthy tests, because requiring four confident
answers abstains more often. Zero wrong answers either way.

Read: **for this kind of question the computed fact is what moves the needle, not the number
of questions.** Effort belongs in the `facts:` program.

The fan-out keeps one real benefit at equal accuracy: it says *which* property fired, which
makes a far better red message than a single yes/no. Worth it for diagnostics, not for
accuracy. Caveat: n=38 and the five questions were not iterated; the third-party result may
still hold for questions less dominated by one resolved fact.

## Shadow mode (designed in, not a later phase)

The honest blocker on everything above is that we have tens of labelled rows and need
hundreds. `mode: shadow` runs the check beside an existing `uses: agent` on the same input,
records both verdicts plus the probability, and grants nothing. Ordinary traffic then becomes
a labelled set for free, and after a few weeks the rows where the two disagree are the only
ones worth reading. That is the only route to validating a threshold that survives a holdout.

## When NOT to use `uses: jev`

- **The check is scriptable.** Then it is `bash/check`. jev never replaces a script, only a
  model. Half the existing `agent` facts here are in this category.
- **The judgment needs to follow a pointer the code cannot resolve either.** Nokoru's
  "each new LEGACY_ALLOWLIST entry is a genuine multi-verb protocol" has to open the named
  file and weigh its design. Out of scope until the `facts:` program can resolve it.
- **The question never changes and labelled examples exist.** A small local model then wins
  on speed and price. jev's edge is an unlabelled domain with a question still moving.
- **The correct answer is the one that resembles the question LEAST.** Check this before
  writing the question and before any positive control: if such a case exists, the question
  is fragile. Both of our 100% positive controls (`: View`, `async`) were the easy family and
  neither of us chose them for that reason — we found out afterwards.

## The abstention rate IS the economics

This is the hole a sibling node found in published routing data and it applies straight to
`gate` mode. An abstention has to go somewhere, and where it goes decides whether any saving
exists: in 237 re-priced real turns, sending abstentions to the safest, most expensive model
saved 11.9%, while sending them to a middle tier saved 59.9%. **An abstention routed to the
expensive option eats the entire benefit.**

For `uses: jev` the expensive option is the agent. A gate whose abstention rate is high costs
*more* than the plain `uses: agent` it replaced, because the repo now pays for jev **and** the
agent. So the abstention rate is a budget, not a detail. Measured on our two questions:

| question | mode | abstains on the healthy case |
|---|---|---|
| "would this test fail if the code broke" | gate | **12 of 20** — unusable as a gate |
| "does this example config hold only placeholders" | gate | 6 of 20 |
| "would this test fail", **with computed facts**, hollow files | tripwire | 0 of 10 |

Rule that falls out: **measure the abstention rate on the HEALTHY case before choosing the
mode.** High abstention does not mean the question is bad — it means it is a tripwire, not a
gate. That is exactly why the hollow-test check ships as a tripwire and the config check as a
gate, and it is a decision no amount of prompt wording fixes.

## A hard guard beats a well-worded question

`jev-ax-pilot` in the same public collection carries a rule the model cannot override: a goal
worded with new/create/make cannot be reported as reached before a real action happened. That
is "can give red, cannot give green" implemented as code rather than trusted to a criteria
block. Worth copying wherever an invariant is known: put it in the `facts` program or in the
scoring, never in the question. A criteria line is a request; a code guard is a fact.

Caveat on every third-party number cited in this document: they are public experiments on a
model that is days old, and none of those repos measures accuracy against real labels at all —
they measure latency, or cost, or nothing. Treat them as hypotheses that shaped our
experiments, never as results. The measurements that govern this design are ours.

## The facts program is the hard part, and it will be wrong before it is right

The weak point of a `uses: jev` check is not the model and not the question. It is the
`facts:` program — and a wrong one does not produce false greens (jev abstains), it produces
**false reds**, which burn the check's credibility in a week.

The Rust facts program in this repo was wrong **four** times. Each error first looked like a
jev mistake, three of them at p≥0.90:

1. **Multi-line signatures.** The first version matched assertions line by line and missed
   every declaration whose signature wrapped. (The Swift version had the same bug: five files
   jev judged correctly were scored as jev errors.)
2. **`.unwrap()` is an assertion.** A test whose only check is
   `validate_with_payload(...).unwrap()` was counted as assertion-free and flagged hollow. In
   Rust a panic ends the test red, so `unwrap` / `expect` / `?` / `panic!` are assertions.
3. **Fluent assertions have no `assert!` in them.** `assert_cmd`'s
   `.assert().failure().code(2).stderr(...)` is the whole CLI suite here and the program saw
   none of it.
4. **Stripping comments ate a string.** `//[^\n]*` removed the rest of any line containing a
   Rust string with a Bazel label in it — `assert_eq!(target_slug("//App:NokoruiOS"), …)` — so
   a healthy three-assertion test was reported as assertion-free and flagged at **p=0.90**.
   That is a **false RED on real, correct code**, which is exactly the failure mode predicted
   above: a wrong facts program does not invent greens, it destroys trust with reds. Blank out
   string literals before touching comments.

And the mirror-image lesson, which is about the corpus rather than the program: once the
facts program was right, **six of eight hand-built "hollow" mutants turned out not to be
hollow** — the mutated test still had an `.unwrap()` that could fail it. Re-derive your labels
with the fixed instrument before you believe any accuracy number.

The rule this leaves for whoever writes the next facts program, for a language not covered
here: **enumerate every way this language can fail a test before you count anything** — macros,
panics, fluent builders, custom harnesses, `matches!`, `should_panic`. Then check your own
labels against the program, not the program against the model's answers.

After the three fixes: 23 files, **zero wrong answers**, 10 abstentions, and 9 of 21 healthy
files abstaining — which is what put this check in `tripwire` rather than `gate`, and then in
`shadow` before either.

## Shadow mode has a target and a date, or it is theatre

This check ships in `shadow`: it records one row per judged file to
`.musts/jev-shadow/<question>.jsonl` — timestamp, file, verdict, probability, model id and the
computed facts — and grants nothing and blocks nothing. Its green means "the rows were
recorded", never "the tests are fine".

It starts there for an honest reason: **on musts' own suite it catches nothing, because there
is nothing to catch** — zero hollow tests across 15 real test files. A check with no prey is
not evidence that it works.

Shadow is not a holding pattern, it is a measurement with an exit condition:

- **Target: 300 recorded rows with at least 30 in the minority class** (verdict `yes`, i.e.
  flagged hollow). Tens of rows cannot tune anything — measured: a threshold with zero errors
  on 40 rows made 0.41 errors per 20 on a holdout, with 39% of splits wrong at least once.
  If the minority class never reaches 30, that is itself the answer: the defect does not occur
  in this repo and the check should be deleted here rather than promoted.
- **How to read it**: take every row whose verdict is not `unsure`, open those files, and
  record what was actually true. That gives labels. Then, and only then, check whether the
  default thresholds hold on a holdout — never fit a cut on all of it.
- **When**: whichever comes first, 300 rows or three months from merge. Put the date in the
  PR. A shadow nobody reads is worse than no shadow, because it looks like coverage.
- **Promotion rule**: it may leave `shadow` for `tripwire` only if the holdout shows zero
  false reds on the labelled rows. It may never become a `gate` on this question — 9 of 21
  healthy files abstain, and a gate that escalates 43% of files to an agent costs more than
  the agent check it replaced.

## `musts run` will not execute this today, and that is correct

`musts run` refuses any task whose capability is not a built-in
(`crates/musts-core/src/run.rs`), with the reason written in the code: *"a descriptor-backed
extension could otherwise have its `command` executed here."* That is a sound safety boundary
— an installed extension must not be able to get arbitrary commands run — and it is not worth
weakening for this.

The consequence is concrete: **an out-of-tree `uses: jev` can emit a `command`, and nothing
will run it.** Two honest ways forward, and they are a decision for whoever owns musts, not
something to route around:

- **Promote `jev` to a built-in** under `crates/musts-core/src/builtin/`, next to `agent` and
  `cargo`. That is what makes it a judgment check a machine can actually run, and it is the
  only version that delivers the point of the whole exercise.
- **Leave it out-of-tree and invoke the script directly** — from a git hook, a scheduled job,
  or by hand. That is enough for `shadow`, which only has to record rows, and it is how the
  shadow in this PR runs today. It is NOT enough for `gate` or `tripwire`, because those have
  to be part of the validation loop to mean anything.

Verified end to end in shadow over every test source in this repo, invoking the script
directly: **64 files, 34 judged healthy, 0 flagged, 30 abstentions (47%)**, rows written to
`.musts/jev-shadow/hollow.jsonl` with the verdict, the probability and the dated model id
(`typesafe/jev-1.13-20260917`). Nothing granted, nothing blocked.

Two things that number settles. **musts' own suite contains no hollow tests** — so this check
has no prey here today, and its shadow is honest precisely because it is empty. And the 47%
abstention rate is a second, independent reason this question can never be a `gate`: escalating
half the files to an agent costs more than the agent check it would replace.

One more thing this run settles, and it is why the check is NOT in `MUSTS.yml`: a pending
judgment task blocks every commit through the pre-commit hook. A `shadow` wired as a check
would therefore block exactly what it promises not to block. It runs from
`.musts/extensions/jev/shadow-run.sh` instead — outside the validation loop, where it can be
honest about granting and blocking nothing.
