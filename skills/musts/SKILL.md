---
name: musts
description: Use the `musts` CLI to validate that your changes are done. Run `musts validate` to get the validation todo list, dispatch independent tasks to parallel subagents, record evidence with `musts evidence`, then re-run `musts validate` until it is empty. Use after any code change in a repo that has a `MUSTS.yml`. Also covers adding a `.mustsignore` (gitignore-style file) when local artefacts are making the validation loop noisier than it should be.
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
# → prints a task list, exit code 1 if pending, 0 if clean
```

1. Run `musts validate`.
2. For every task in the report, run the validation it asks for, then submit
   evidence. A single report contains at most 5 tasks.
3. Re-run `musts validate`.
4. Repeat until exit code `0`.

## Reading a task

Each task in the report tells you:

- **Task id** — passed back to `musts evidence`.
- **do** — exactly what to run.
- **evidence** — what to attach when recording.
- **submit** — the `musts evidence` command shape for that task.

Do not invent extra steps. Do not skip required evidence.

## Dispatching to subagents

When the report has multiple independent tasks:

- Dispatch each task to its own subagent in a single batch.
- Each subagent runs **one** task's instructions, captures the log, and
  returns the asset path back to you.
- After the batch finishes, **you** record evidence for every task
  sequentially with `musts evidence`.

If two tasks share a single resource the report cannot know about (a
simulator, a database, a port), run them sequentially.

## Recording evidence

```bash
musts evidence <task-id> \
  --text "<one-line summary of the result>" \
  --asset <path-to-log-or-screenshot>
```

- `--asset` repeats. Attach every file the report's `evidence:` line asks for.
- Write asset files **outside the workspace** — for example under `$TMPDIR`
  or `/tmp/musts/<task-id>/`. Logs written inside the workspace mutate the
  scope hash and may invalidate the task you're trying to close.
- Submit one `musts evidence` call per task. If it fails, fix the missing
  evidence and call it again — do not re-run `musts validate` first.

## After recording

- Run `musts validate` again.
- If the report is empty, you are done.
- If new tasks appeared (your evidence-recording moved files, or a parallel
  edit landed), loop again with the new ids.

## Don'ts

- Don't run `musts validate` between evidence submissions for the same
  report — it truncates the task list and the remaining ids will be rejected
  as "no longer applies".
- Don't skip the loop because "the change is trivial". Every task in the
  report is dirty per the ledger; submit evidence or fix the underlying
  issue.
- Don't hand-edit `.musts/ledger.lock.yaml`. `musts evidence` is the only
  writer.

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
