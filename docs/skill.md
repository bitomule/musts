# Harness Validation Skill

Drop this file into your agent's skill folder. Claude Code reads it from `.claude/skills/`; other agent harnesses have their own paths. Source: `docs/harness-validation-plan.md` §14 + `docs/PLAN.md` Phase 7.

---

## Purpose

Ensure repository-defined harness validation is **clean** before declaring a task done.

The single hard rule of the harness:

> The task is not done until `harness validate` is empty.

## Protocol

1. **Run `harness validate`** at the start of every task that touches code and any time you are about to declare work complete.
2. Treat the returned task list as the validation todo list.
3. If multiple tasks can be executed independently (`parallelizable: true`), use subagents in parallel — but **not** when the underlying tool is single-resource (simulators, local servers, build locks, shared databases). When in doubt, run sequentially.
4. If one task `satisfies` multiple checks, execute it **once**. Do not split.
5. **Do not invent evidence requirements.** Use the `Evidence required:` section in the task report. Asset kinds, MIME types, and the `text` requirement are extension-defined.
6. For each task, perform the requested validation using the right tool (Bazel, MAV, Playwright, the system under test).
7. **Record evidence** with:

   ```bash
   harness evidence <task-id> --text "<one-line summary>" --asset <path>...
   ```

   The `<task-id>` comes from the report. `--asset` may repeat. Asset paths can point anywhere on disk; harness copies them into `.harness/evidence/<task-id>/submission-NNN/` so workspace edits between evidence calls do not affect them.
8. If `harness evidence` exits non-zero, **read the error**:
   - Exit **1** = the extension rejected the evidence (e.g. missing kind, zero-byte file, non-parseable JSON). Fix and re-submit.
   - Exit **2 with "stale"** = files inside this task's scopes changed after the task was issued. Re-run `harness validate` and follow the new task list.
   - Exit **2 with "no longer applies"** = a subsequent `harness validate` truncated the previous task list. Re-run `validate` and use the new ids.
9. **Re-run `harness validate`.** If new tasks appear, repeat the loop.
10. If `harness validate` reports clean, the work can be reported as complete.

## Hard rules

- **Record evidence for every task from the current `harness validate` output before re-running `harness validate`.** Re-running `validate` replaces the previous task table — un-recorded task ids from the prior run will be rejected with "no longer applies" (PLAN.md §4.2). This is the single most common agent-loop bug.
- **Do not silence the loop.** If a task feels redundant or already-satisfied, that's the extension's call, not yours: every task in the report is dirty per the ledger. Submit evidence or fix the underlying issue.
- **Snapshot assets outside the workspace** when you can, especially logs you produce while running the task. Writing them inside the workspace mutates the scope hash and can stale the task you're about to submit evidence for.

## Capabilities at a glance

- **`agent`** is built into the harness binary. Manifests using `uses: agent` need no installed extension; the task tells you which facts to verify and asks for a text summary plus whatever assets you captured.
- **`bazel/build`, `mav/expect`, and any third-party `uses: ...`** are installed as extensions under `<workspace>/.harness/extensions/<name>/`. They can be Rust binaries, bash scripts, Python — anything that speaks the JSON protocol.

## Quick reference

```bash
# What does the harness want from me right now?
harness validate

# Submit evidence for one task.
harness evidence bazel-build-login \
  --text "bazel build //App/Login:Login succeeded" \
  --asset /tmp/login-build.log

# Submit evidence with multiple assets.
harness evidence mav-login-flow \
  --text "Validated valid + invalid email flows" \
  --asset /tmp/login-success.png \
  --asset /tmp/login-run.mp4 \
  --asset /tmp/mav-report.json

# Confirm everything closed.
harness validate
```

## What the report looks like

```text
Harness validation pending.

Task: bazel-build-login
Title: Build //App/Login:Login
Extension: bazel/build
Satisfies:
  - App/Login/login-build
Parallelizable: yes
Instructions:
  1. Run `bazel build //App/Login:Login`.
  2. Capture stdout/stderr as a log asset.
  3. Record the result with `harness evidence <task-id> --text "…" --asset <log>`.
Evidence required:
  - text (required): State the command that was run and whether it succeeded.
  - log (required): Bazel stdout/stderr log.

Task: mav-expect-app-login
Title: Validate MAV expectations for App/Login
…

Completion rule:
  Repeat `harness validate` after recording evidence.
  The task is not done until this report is empty.
```

When clean:

```text
Harness validation clean.
No pending validation tasks for the current workspace snapshot.
```

## When you should NOT use this skill

- Tasks that don't touch a harness-validated repo (`.harness/` absent, no `HARNESS.yml`).
- Refactors that have already shipped — the harness ratchets evidence forward, it doesn't re-validate the past.
- One-off scripts that bypass the agent loop entirely.

If `harness validate` exits 0 with "No HARNESS.yml files found.", the workspace isn't validated by harness and the loop doesn't apply.
