---
name: musts
description: Use the `musts` CLI to validate that your changes are done. Run `musts validate` to get the validation todo list; for deterministic checks (cargo/*, bazel/build) run `musts run <task-id>` to let musts execute and record them for you; for judgment checks (agent, mav) do the work and record evidence with `musts evidence`; then re-run `musts validate` until it is empty. Use after any code change in a repo that has a `MUSTS.yml`. Also covers adding a `.mustsignore` (gitignore-style file) when local artefacts are making the validation loop noisier than it should be.
---

# Musts

`musts` is the validation loop. The task is not done until `musts validate`
exits clean.

## When to use this skill

- After making code changes in a repo with a `MUSTS.yml` at the root.
- Before declaring work complete, opening a PR, or asking for review.
- Whenever the user asks "is it ready?" / "is this validated?".

If `musts validate` prints "No MUSTS.yml files found.", this skill does not
apply.

## The loop

```bash
musts validate
# → prints every pending task, exit code 1 if pending, 0 if clean
```

1. Run `musts validate`. It emits **every** dirty task (no batching), so it
   is idempotent — re-running it never invalidates the ids it just issued.
2. Close each task:
   - **Runnable checks** (`do:` is a plain command — `cargo/*`,
     `bazel/build`): just `musts run <task-id>`. musts executes the
     command itself, checks the real exit code, and records the evidence
     for you. You never re-run the build to satisfy the loop.
   - **Judgment checks** (`agent`, `mav` — verify facts, drive a UI): do
     the work yourself, then `musts evidence <task-id>` (see below).
3. Re-run `musts validate`.
4. Repeat until exit code `0`.

A task shown as `(unchanged since last validate …)` is one you already saw —
its full body isn't reprinted; act on it the same way.

## Reading a task

Each task in the report tells you:

- **Task id** — passed to `musts run` or `musts evidence`.
- **do** — exactly what to run.
- **evidence** — what to attach when recording (judgment checks only).
- **submit** — the `musts evidence` command shape for that task.

Do not invent extra steps. Do not skip required evidence.

## `musts run` — the fast path for deterministic checks

```bash
musts run cargo-test-root
```

- Executes the task's command from the workspace root, captures the log
  outside the workspace, and — on exit 0 — records evidence automatically.
- On a non-zero exit it prints the output and records nothing: fix the
  failure and re-run.
- Refuses judgment tasks (`agent`, `mav`) and any check provided by an
  installed extension — use `musts evidence` for those.

## Dispatching to subagents

When the report has multiple independent judgment tasks:

- Dispatch each to its own subagent in a single batch.
- Each subagent runs **one** task's work, captures any asset, and returns
  the asset path back to you.
- After the batch finishes, **you** record evidence for every task with
  `musts evidence`.

If two tasks share a single resource the report cannot know about (a
simulator, a database, a port), run them sequentially. Deterministic checks
are simpler: just `musts run` them (in parallel is fine unless they contend
on a build lock).

## Recording evidence (judgment checks)

```bash
musts evidence <task-id> \
  --text "<one-line summary of the result>" \
  --asset <path-to-log-or-screenshot>
```

- `--asset` repeats. Attach every file the report's `evidence:` line asks for.
- The asset is validated **in place** and not copied anywhere — musts no
  longer archives evidence under `.musts/evidence/`. The ledger
  (`.musts/ledger.lock.yaml`) is the record.
- Prefer asset files **outside the workspace** (e.g. `$TMPDIR`): a log
  written inside the workspace mutates the scope hash and can invalidate
  the task you're closing.
- Submit one `musts evidence` call per task. If it fails, fix the missing
  evidence and call it again.

## After recording

- Run `musts validate` again.
- If the report is empty, you are done.
- If a task comes back "stale", files it covers changed after it was
  issued — re-run `musts validate` and act on the fresh id.

## Don'ts

- Don't skip the loop because "the change is trivial". Every task in the
  report is dirty per the ledger; run it, submit evidence, or fix the
  underlying issue.
- Don't hand-edit `.musts/ledger.lock.yaml`. `musts run` / `musts evidence`
  are the only writers.

## Adding a `.mustsignore`

`.mustsignore` is `.gitignore` for musts: files it matches are excluded
from the walker that builds each check's scope hash, so editing them
never re-invalidates the ledger.

Reach for it when a file is making the loop noisier than it should:

- canonical fixtures the user wants committed but doesn't want gating
  validation;
- local artefacts that aren't gitignored (logs in a vendored sub-repo,
  IDE state outside the standard ignore list);
- generated files that change far more often than the underlying source.

Don't reach for it to silence a failing check — that's the extension's
call, not yours.

```bash
# at the workspace root (or any subdirectory — applies to that subtree,
# same as nested .gitignore):
cat > .mustsignore <<'EOF'
*.log
scratch/
!scratch/keep-this.log    # negation works on file patterns
EOF
git add .mustsignore
```

**Commit `.mustsignore`.** If it isn't committed, two clones produce
different scope hashes for the same code and the lock stops
reproducing. After adding patterns, run `musts validate` once: any
check whose scope contents change re-opens, so submit evidence for
those tasks, then commit the refreshed `.musts/ledger.lock.yaml`.

Gotcha: standard gitignore rule. If you ignore a directory (`fixtures/`),
you cannot re-include children with `!fixtures/keep`. Use a file-pattern
(`*.junk` / `!keep.junk`) or ignore the directory's *contents*
(`fixtures/*` / `!fixtures/keep`).

## Installing the skill

```bash
musts skill install
```

Installs this skill globally for every detected agent runner. Requires
`npx` (Node.js) on `PATH`. Pass `--agent claude-code` (or `cursor`,
`windsurf`, …) to restrict the install to one runner.
