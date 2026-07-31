# Musts Validation Skill

Drop this file into your agent's skill folder. Claude Code reads it from `.claude/skills/`; other agent runners have their own paths. Source: `docs/musts-design.md` §14 + `docs/PLAN.md` Phase 7.

---

## Purpose

Ensure repository-defined musts validation is **clean** before declaring a task done.

The single hard rule of musts:

> The task is not done until `musts validate` is empty.

## Protocol

1. **Run `musts validate`** at the start of every task that touches code and any time you are about to declare work complete.
2. Treat the returned task list as the validation todo list. `validate` emits **every** dirty task (no batching) and is idempotent — re-running it never invalidates the ids it just issued.
3. Close each task by kind:
   - **Deterministic** (`do:` is a plain command — `cargo/*`, `bazel/build`): run `musts run <task-id>`. musts executes the command, checks the real exit code, and records evidence for you — no re-running to satisfy the loop. A non-zero exit prints the output and records nothing; fix and re-run.
   - **Judgment** (`agent`, `mav`): perform the validation yourself and record evidence (step 7).
4. If multiple judgment tasks are independent, use subagents in parallel — but **not** when the underlying tool is single-resource (simulators, local servers, build locks, shared databases). When in doubt, run sequentially.
5. If one task `satisfies` multiple checks, execute it **once**. Do not split.
6. **Do not invent evidence requirements.** Use the `evidence:` and `submit:` lines in the task report. Asset kinds and the `text` requirement are extension-defined.
7. **Record evidence** (judgment checks) with:

   ```bash
   musts evidence <task-id> --text "<one-line summary>" --asset <path>...
   ```

   The `<task-id>` comes from the report. `--asset` may repeat. Assets are validated **in place** — musts no longer archives them; the committed `.musts/ledger.lock.yaml` is the record. Keep logs outside the workspace so edits don't perturb the scope hash.
8. If `musts run`/`musts evidence` exits non-zero, **read the error**:
   - Exit **1** = the command failed (`musts run`) or the extension rejected the evidence (missing kind, zero-byte file, failure markers in the log). Fix and re-run.
   - Exit **2 with "stale"** = files inside this task's scopes changed after the task was issued. Re-run `musts validate` and follow the fresh task list.
   - Exit **2 with "no longer applies"** = that task id isn't in the current report (its check is already green, or a fresh `validate` changed the set). Re-run `validate` and use the current ids.
9. **Re-run `musts validate`.** If new tasks appear, repeat the loop.
10. If `musts validate` reports clean, the work can be reported as complete.

## Hard rules

- **Do not silence the loop.** If a task feels redundant or already-satisfied, that's the extension's call, not yours: every task in the report is dirty per the ledger. Run it, submit evidence, or fix the underlying issue.
- **Snapshot assets outside the workspace** when you can, especially logs you produce while running the task. Writing them inside the workspace mutates the scope hash and can stale the task you're about to submit evidence for.
- **Run the validation loop after your last edit, before you commit.** Any change to any file in a scope — including a comment, whitespace, or a `.gitignore` rule that doesn't actually move files in or out — re-hashes that scope and invalidates the matching entries in `.musts/ledger.lock.yaml`. Order: **edit → validate → submit → commit**. "Submit → edit → commit" looks fine locally (the SQLite ledger still has the old `scope_hash`) but ships a stale lock to every clone.

## The committed ledger lock

`.musts/ledger.lock.yaml` is the portable record of what's been validated. Every accepted `musts evidence` appends a `(check_id, scope_hash)` entry to it; `scope_hash` is a content-hash fingerprint of the files in the check's effective scope. The file lives next to the rest of the workspace (it is **committed**, unlike `state.sqlite` which is per-machine) and `musts validate` consults it alongside the local SQLite ledger when answering "is this check green?". A clone that pulls the lock inherits the team's validated state immediately — the agent only sees tasks for scopes its own changes invalidated.

What this means for your workflow:

- **A scope's hash changes any time any file inside it changes.** Comments, doc tweaks, `.gitignore` edits, reordering imports — none of them change the underlying tool's behaviour, but all of them re-hash the scope and detach it from the prior `(check_id, scope_hash)` entries. musts chooses conservative invalidation over guessing what's "semantic" vs "cosmetic"; it has no way to tell the difference. If you must edit late in the cycle, run the loop again before you commit.
- **The lock is a union, not a snapshot.** Multiple `(check, scope_hash)` entries can accumulate per check as the codebase evolves. That's by design — a clone is green if its current scope hash matches *any* of them. Don't hand-prune the file; musts writes it monotonically and a future cleanup pass will retire dead entries.
- **Sub-workspaces (fixtures, demos, examples) often gitignore their own lock** so the canonical walkthrough starts with nothing validated. If you're working on one of those and `validate` keeps reporting pending tasks despite a clean run, check the project's `.gitignore` before assuming musts is broken.
- **Commit `.musts/.gitattributes`.** musts writes it next to the lock with `ledger.lock.yaml merge=union`, which is what stops two branches that both recorded evidence from conflicting on the lock. It only works once it is committed. If you ever *do* see conflict markers in the lock, keep every entry from both sides — the ledger is append-only, so the union is always the right resolution.
- **A merge whose result nobody validated reopens the checks covering it, and that is correct.** If `main` moved while your branch was open, the tree that lands carries both sets of edits and neither side ever checked that combination. Nothing was lost from the ledger — the tree is genuinely new. To avoid paying for an expensive check twice, bring `main` into the branch and re-close the loop *before* merging, so the branch validates the tree that actually lands. Narrower `paths:` and nested manifests are the other lever: they keep unrelated churn from touching an expensive check's scope.

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
Musts validation pending: 2 tasks.

1. bazel-build-login
   do: Run `bazel build //App/Login:Login`.
   run: musts run bazel-build-login

2. mav-expect-app-login
   do: Validate MAV expectations for App/Login …
   evidence: screenshot + video + mav-report
   submit: musts evidence mav-expect-app-login --text "..." --asset <screenshot> …

Run runnable checks with `musts run <task-id>`; record judgment checks with `musts evidence`. Then rerun `musts validate` until clean.
```

Deterministic checks (`cargo/*`, `bazel/build`) show a `run:` line — `musts run` executes them and records evidence for you. Judgment checks (`agent`, `mav`) show `evidence:` + `submit:`.

When clean:

```text
Musts validation clean.
```

## When you should NOT use this skill

- Tasks that don't touch a musts-validated repo (`.musts/` absent, no `MUSTS.yml`).
- Refactors that have already shipped — musts ratchets evidence forward, it doesn't re-validate the past.
- One-off scripts that bypass the agent loop entirely.

If `musts validate` exits 0 with "No MUSTS.yml files found.", the workspace isn't validated by musts and the loop doesn't apply.
