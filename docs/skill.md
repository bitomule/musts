# Musts Validation Skill

Drop this file into your agent's skill folder. Claude Code reads it from `.claude/skills/`; other agent runners have their own paths. Source: `docs/musts-design.md` §14 + `docs/PLAN.md` Phase 7.

---

## Purpose

Ensure repository-defined musts validation is **clean** before declaring a task done.

The single hard rule of musts:

> The task is not done until `musts validate` is empty.

## Protocol

1. **Run `musts validate`** at the start of every task that touches code and any time you are about to declare work complete.
2. Treat the returned task list as the validation todo list.
3. If multiple tasks can be executed independently (`parallelizable: true`), use subagents in parallel — but **not** when the underlying tool is single-resource (simulators, local servers, build locks, shared databases). When in doubt, run sequentially.
4. If one task `satisfies` multiple checks, execute it **once**. Do not split.
5. **Do not invent evidence requirements.** Use the `Evidence required:` section in the task report. Asset kinds, MIME types, and the `text` requirement are extension-defined.
6. For each task, perform the requested validation using the right tool (Bazel, MAV, Playwright, the system under test).
7. **Record evidence** with:

   ```bash
   musts evidence <task-id> --text "<one-line summary>" --asset <path>...
   ```

   The `<task-id>` comes from the report. `--asset` may repeat. Asset paths can point anywhere on disk; musts copies them into `.musts/evidence/<task-id>/submission-NNN/` so workspace edits between evidence calls do not affect them.
8. If `musts evidence` exits non-zero, **read the error**:
   - Exit **1** = the extension rejected the evidence (e.g. missing kind, zero-byte file, non-parseable JSON). Fix and re-submit.
   - Exit **2 with "stale"** = files inside this task's scopes changed after the task was issued. Re-run `musts validate` and follow the new task list.
   - Exit **2 with "no longer applies"** = a subsequent `musts validate` truncated the previous task list. Re-run `validate` and use the new ids.
9. **Re-run `musts validate`.** If new tasks appear, repeat the loop.
10. If `musts validate` reports clean, the work can be reported as complete.

## Hard rules

- **Record evidence for every task from the current `musts validate` output before re-running `musts validate`.** Re-running `validate` replaces the previous task table — un-recorded task ids from the prior run will be rejected with "no longer applies" (PLAN.md §4.2). This is the single most common agent-loop bug.
- **Do not silence the loop.** If a task feels redundant or already-satisfied, that's the extension's call, not yours: every task in the report is dirty per the ledger. Submit evidence or fix the underlying issue.
- **Snapshot assets outside the workspace** when you can, especially logs you produce while running the task. Writing them inside the workspace mutates the scope hash and can stale the task you're about to submit evidence for.
- **Run the validation loop after your last edit, before you commit.** Any change to any file in a scope — including a comment, whitespace, or a `.gitignore` rule that doesn't actually move files in or out — re-hashes that scope and invalidates the matching entries in `.musts/ledger.lock.yaml`. Order: **edit → validate → submit → commit**. "Submit → edit → commit" looks fine locally (the SQLite ledger still has the old `scope_hash`) but ships a stale lock to every clone.

## The committed ledger lock

`.musts/ledger.lock.yaml` is the portable record of what's been validated. Every accepted `musts evidence` appends a `(check_id, scope_hash)` entry to it; `scope_hash` is a content-hash fingerprint of the files in the check's effective scope. The file lives next to the rest of the workspace (it is **committed**, unlike `state.sqlite` which is per-machine) and `musts validate` consults it alongside the local SQLite ledger when answering "is this check green?". A clone that pulls the lock inherits the team's validated state immediately — the agent only sees tasks for scopes its own changes invalidated.

What this means for your workflow:

- **A scope's hash changes any time any file inside it changes.** Comments, doc tweaks, `.gitignore` edits, reordering imports — none of them change the underlying tool's behaviour, but all of them re-hash the scope and detach it from the prior `(check_id, scope_hash)` entries. musts chooses conservative invalidation over guessing what's "semantic" vs "cosmetic"; it has no way to tell the difference. If you must edit late in the cycle, run the loop again before you commit.
- **The lock is a union, not a snapshot.** Multiple `(check, scope_hash)` entries can accumulate per check as the codebase evolves. That's by design — a clone is green if its current scope hash matches *any* of them. Don't hand-prune the file; musts writes it monotonically and a future cleanup pass will retire dead entries.
- **Sub-workspaces (fixtures, demos, examples) often gitignore their own lock** so the canonical walkthrough starts with nothing validated. If you're working on one of those and `validate` keeps reporting pending tasks despite a clean run, check the project's `.gitignore` before assuming musts is broken.

## Capabilities at a glance

- **`agent`** is built into the musts binary. Manifests using `uses: agent` need no installed extension; the task tells you which facts to verify and asks for a text summary plus whatever assets you captured.
- **`bazel/build`, `mav/expect`, and any third-party `uses: ...`** are installed as extensions under `<workspace>/.musts/extensions/<name>/`. They can be Rust binaries, bash scripts, Python — anything that speaks the JSON protocol.

## Quick reference

```bash
# What does musts want from me right now?
musts validate

# Submit evidence for one task.
musts evidence bazel-build-login \
  --text "bazel build //App/Login:Login succeeded" \
  --asset /tmp/login-build.log

# Submit evidence with multiple assets.
musts evidence mav-login-flow \
  --text "Validated valid + invalid email flows" \
  --asset /tmp/login-success.png \
  --asset /tmp/login-run.mp4 \
  --asset /tmp/mav-report.json

# Confirm everything closed.
musts validate
```

## What the report looks like

```text
Musts validation pending.

Task: bazel-build-login
Title: Build //App/Login:Login
Extension: bazel/build
Satisfies:
  - App/Login/login-build
Parallelizable: yes
Instructions:
  1. Run `bazel build //App/Login:Login`.
  2. Capture stdout/stderr as a log asset.
  3. Record the result with `musts evidence <task-id> --text "…" --asset <log>`.
Evidence required:
  - text (required): State the command that was run and whether it succeeded.
  - log (required): Bazel stdout/stderr log.

Task: mav-expect-app-login
Title: Validate MAV expectations for App/Login
…

Completion rule:
  Repeat `musts validate` after recording evidence.
  The task is not done until this report is empty.
```

When clean:

```text
Musts validation clean.
No pending validation tasks for the current workspace snapshot.
```

## When you should NOT use this skill

- Tasks that don't touch a musts-validated repo (`.musts/` absent, no `MUSTS.yml`).
- Refactors that have already shipped — musts ratchets evidence forward, it doesn't re-validate the past.
- One-off scripts that bypass the agent loop entirely.

If `musts validate` exits 0 with "No MUSTS.yml files found.", the workspace isn't validated by musts and the loop doesn't apply.
