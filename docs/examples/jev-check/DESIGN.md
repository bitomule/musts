# `uses: jev` — a judgment check a machine can run

Read this before changing anything here. Every rule below has a number behind it, and each one
was learned by getting it wrong first.

## Why it exists

`musts` has had deterministic checks a script runs and judgment checks only an agent can make;
`Task.command` says so — populated for the first, `None` for the second. Judgment is therefore
expensive, so almost nobody writes a judgment check: across nine repos there are 26 checks, 5
are `uses: agent`, and half of their facts are scriptable things that should never have been
agent checks. A typed jev question answers in ~400 ms for ~$0.0001.

It also turns an **attestation** into a **measurement**. "I re-read README.md and it reflects
the current state" is a sentence a hurried agent signs without looking, and nothing can tell. A
jev verdict is judged against the real file, with its probability and model id recorded.

**The value is not the check shipped here. It is the checks nobody wrote because validating
them cost an agent.**

## The rules, and the number behind each one

Corpus for every figure below: 20 healthy test functions and 18 hollow mutants, labels
re-derived after each instrument fix. **Zero wrong answers in every arm ever measured.**

**1. Criteria are mandatory.** The schema requires them and the run refuses a question set
whose asked question lacks either branch. Removing them took detection from **8/18 to 3/18**
and made **every** healthy file abstain. A format that permits omitting them will see them
omitted.

**2. No numeric threshold, anywhere. The `verdict` decides.** A cut fitted on 40 rows with zero
errors made **0.41 wrong answers per 20 on a holdout, with 39% of splits wrong at least once**.
Tens of rows can check whether the defaults work; they cannot tune anything. The schema has no
probability field so one cannot be added for convenience.

**3. `unsure` is green, and that is not a concession.** Under two states an abstaining file and
a healthy file reach the same outcome, so an abstention rate of 47%, 60% or 95% costs exactly
the same: nothing. Abstention only costs money in a gate, where it escalates to an agent — and
**there is no `gate` in the schema**. So the check is chosen by what it CATCHES, and it ships
the single question with resolved facts (**17/18**), which is also the variant that abstains
most (95% on healthy files). Being binary is what frees that choice.

This is honest only because of what the check claims: its green means **"nothing fired"**,
never "verified". A quiet tripwire proves nothing and may never be the only check on a risk.

**4. Facts are resolved in code and handed over resolved.** jev judges what is written in front
of it and abstains when the answer depends on what something resolves to at runtime: the same
10 hollow tests scored **0/10 with 10/10 abstentions** on raw source and **10/10** once the code
supplied `resolved_element_count: 0`. The control matters as much: with the count set to 37 it
does not flip to a false green, it abstains. **A 10/10 with no inverted control only shows the
field pushes, not that it informs.**

**5. The filter runs before the request.** 81% of the original abstentions were files with no
`#[test]` in them — not judgment, a question that did not apply, one wasted request each. The
facts program prints `"applicable": false` and the check drops them before calling anything.
Over this repo: **64 files → 27 dropped, 37 judged, 7 abstentions (19%)**, 27 requests never
made.

**6. An input failure is red; only infrastructure degrades.** jevi's error type splits
`NoAnswer` (no key, DNS, timeout, 5xx) from `Invalid` (malformed question set, empty state,
400). Treating both as "not evaluated" leaves a permanently broken check reporting that all is
well. The skip list is explicit; everything else is a red marked `BROKEN`.

**7. Not for CI, and it degrades on its own.** With `CI` set, `JEVI_DISABLE` set or `jevi`
absent, the run skips and exits 0 having judged nothing — and says so, because silence from
this check must never read as evidence. Never read the exit code as an answer: jevi exits 1 on
a `no` and 3 on an `unsure`. Read the JSON `ok` field.

**8. Shadow does not hide its own breakage.** Shadow promises not to block on a *verdict*. A
misconfigured shadow that records nothing forever is the failure mode of every channel that
only ever carries "all clear", so a run that failed for a configuration reason exits 1 saying
`SHADOW IS BROKEN`.

## The facts program is the hard part, and it will be wrong before it is right

The weak point is not the model and not the question. A wrong facts program does not produce
false greens — jev abstains — it produces **false reds**, which burn the check's credibility in
a week.

The Rust facts program here was wrong four times, and every time it looked like a jev mistake,
three of them at p ≥ 0.90:

1. **Multi-line signatures** — assertions matched line by line, wrapped declarations missed.
2. **`.unwrap()` is an assertion** — a test whose only check was `f(...).unwrap()` was counted
   assertion-free. In Rust a panic ends the test red.
3. **Fluent assertions carry no `assert!`** — `assert_cmd`'s `.assert().failure().code(2)` is
   the whole CLI suite here and the program saw none of it.
4. **Stripping comments ate a string** — `//[^\n]*` removed the rest of any line holding a
   Bazel label, so `assert_eq!(target_slug("//App:Target"), …)` read as assertion-free and a
   healthy test was flagged at p=0.90. Blank out string literals before touching comments.

And the mirror image: once the program was right, **six of eight hand-built "hollow" mutants
turned out not to be hollow**. Re-derive your labels with the fixed instrument before believing
any accuracy number.

**And one bug that made the filter silently do nothing**: it first read
`jq -r '.applicable // true'`. **jq's `//` treats `false` as absent**, so `false // true` is
`true`, every file the filter meant to drop sailed through, and the run reported
`0 not applicable` and looked fine. Use `if .applicable == false`.

Writing one for a language not covered here: **enumerate every way that language can fail a
test before counting anything** — macros, panics, fluent builders, `should_panic`, custom
harnesses. Then check your labels against the program, not the program against jev's answers.

## Shadow has a target and a date, or it is theatre

This check ships in `shadow`: it records one row per judged file to
`.musts/jev-shadow/<question>.jsonl` and grants and blocks nothing. Its green means "the rows
were recorded". It starts there for an honest reason — **on this repo's suite it catches
nothing, because there is nothing to catch**: zero hollow tests across every test source.

- **Target: 300 rows with at least 30 flagged.**
- **Read it by** 300 rows or three months from merge, whichever is first. A shadow nobody reads
  is worse than none, because it looks like coverage.
- **Promotion**: shadow → tripwire only if a holdout shows zero false reds on labelled rows.
- **If the flagged count never reaches 30, delete the check here** rather than promote it: the
  defect does not occur in this repo.

## When NOT to use `uses: jev`

- **The check is scriptable.** Then it is `bash/check`. jev never replaces a script, only a
  model.
- **A good enough facts program removes the question.** The tell fires while you are still
  writing it: **if the facts program has to LOCATE the defect in order to describe it to jev,
  it has already found it and the question only repeats the finding.** Contrast the check here,
  whose facts program resolves how many elements a loop yields — a neutral fact that says
  nothing about whether the test is hollow. Resolving a fact is the good case.
- **The judgment must follow a pointer the code cannot resolve either.**
- **The question never changes and labelled examples exist.** A small local model then wins.
- **The correct answer is the one that resembles the question LEAST.** Check this before
  writing the question and before any positive control. Both of the 100% positive controls that
  validated this approach (`: View`, `async`) were the easy family, and nobody chose them for
  that reason.

## Why it is not in `MUSTS.yml`, and why `musts run` will not execute it

`musts run` refuses any task whose capability is not a built-in
(`crates/musts-core/src/run.rs`): *"a descriptor-backed extension could otherwise have its
`command` executed here."* Sound boundary, not worth weakening. And wiring the check into
`MUSTS.yml` made it a pending judgment task, which blocks every commit through the pre-commit
hook — a shadow blocking exactly what it promises not to block.

So it runs from `.musts/extensions/jev/shadow-run.sh`, outside the validation loop. The agreed
end state is `jev` as a capability inside the tree, using `jevi` as a library dependency from a
binary of the `musts` package — never from the core, which stays declare-only and network-free.
