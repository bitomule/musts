---
name: musts
description: Use the `musts` CLI to validate that your changes are done. Run `musts validate` to get the validation todo list, dispatch independent tasks to parallel subagents, record evidence with `musts evidence`, then re-run `musts validate` until it is empty. Use after any code change in a repo that has a `MUSTS.yml`.
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
   evidence.
3. Re-run `musts validate`.
4. Repeat until exit code `0`.

## Reading a task

Each task in the report tells you:

- **Task id** — passed back to `musts evidence`.
- **Instructions** — exactly what to run.
- **Parallelizable: yes/no** — whether it is safe to run alongside the others.
- **Evidence required** — what to attach when recording.

Do not invent extra steps. Do not skip required evidence.

## Dispatching to subagents

When the report has multiple tasks marked `Parallelizable: yes`:

- Dispatch each task to its own subagent in a single batch.
- Each subagent runs **one** task's instructions, captures the log, and
  returns the asset path back to you.
- After the batch finishes, **you** record evidence for every task
  sequentially with `musts evidence`.

When a task is `Parallelizable: no`, run it in the current agent before
dispatching the parallel ones (or after them), but never alongside.

If two tasks share a single resource the report cannot know about (a
simulator, a database, a port), run them sequentially even if both say
parallelizable.

## Recording evidence

```bash
musts evidence <task-id> \
  --text "<one-line summary of the result>" \
  --asset <path-to-log-or-screenshot>
```

- `--asset` repeats. Attach every file the "Evidence required" section asks
  for.
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

## Installing the skill

```bash
musts skill install
```

Installs this skill globally for every detected agent runner. Requires
`npx` (Node.js) on `PATH`. Pass `--agent claude-code` (or `cursor`,
`windsurf`, …) to restrict the install to one runner.
