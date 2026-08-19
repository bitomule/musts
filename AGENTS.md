# AGENTS.md

Project context for AI coding agents working on `musts`. This file follows
the [agents.md](https://agents.md) convention; `CLAUDE.md` is a symlink to
this file, so Claude Code reads the same rules.

## What this project is

`musts` is the agent-first validation loop — a small Rust CLI that tells an
agent what must be validated after a change, how to produce evidence, and
when the work is allowed to be called done. The repo validates itself with
its own CLI; the `musts validate` loop must stay clean on every PR.

Architecture, contract details, and the protocol spec live in
[`docs/`](docs/). [`CONTRIBUTING.md`](CONTRIBUTING.md) holds the human
contributor guide (build, test, license, etc.). This file is the rules
you must follow when opening a PR here.

## PR title is the contract

PRs are squash-merged. The squash commit on `main` inherits its message
from the **PR title**, not the branch name and not from intermediate
commits. `release-plz` parses those squash commits to grow the rolling
release PR and the next changelog entry.

PR titles must follow Conventional Commits:

- `feat:` — new user-facing capability (CLI command, `MUSTS.yml` field,
  extension contract)
- `fix:` — bug fix
- `docs:` — documentation only
- `refactor:` — internal change with no behaviour diff
- `test:` — tests only
- `chore:` — build, tooling, deps
- `perf:` — performance
- `ci:` — workflows
- Scope is optional: `fix(core): ...`, `chore(deps): ...`
- Breaking: `feat!:` plus `BREAKING CHANGE:` footer in the PR body

Branch names are free. Intermediate commits inside the branch are free.
Only the PR title matters.

## Before opening a PR

```bash
make all                                # fmt + clippy + test + e2e
cargo build --release --locked
./target/release/musts validate         # must exit 0
```

The reference capabilities (`agent`, `cargo/{fmt,clippy,test}`, `bazel/{build,test}`, `mav/expect`) are built into the `musts` binary — no `.musts/extensions/` wiring is needed for self-validation.

If `musts validate` reports pending tasks, run the listed commands, capture
logs **outside** the workspace (e.g. `$TMPDIR`), and submit evidence with
`./target/release/musts evidence <task-id> --text "..." --asset <log>` until
the loop is empty. Commit the refreshed `.musts/ledger.lock.yaml`.

The CI `musts validate (self)` job is a required check and runs the same
loop.

## Don't

- Edit `CHANGELOG.md`. `release-plz` regenerates it from Conventional
  Commits in the release PR; manual edits get overwritten.
- Add `SECURITY.md` or a code-of-conduct unless I ask — solo-maintainer
  project, minimal ceremony.
- Touch branch protection on `main`.
- Bypass the dogfood loop. The whole project's premise is that the loop
  stays clean; landing red on `musts validate` defeats the point.

## Versioning

Pre-1.0: minor may break, patch is bug-fix only, `feat!:` ships as minor
(no auto-bump to 1.0). Strict SemVer from 1.0 onwards.

## Workspace map

- `crates/musts-protocol` — JSON-over-stdio wire types shared with
  third-party extensions
- `crates/musts-extension-util` — Rust helpers for extension authors
  (third-party use; the in-tree built-ins don't go through this)
- `crates/musts-core` — orchestrator: manifests, snapshots, scope hashes,
  ledger; also home of the built-in capabilities under
  [`src/builtin/`](crates/musts-core/src/builtin/) (`agent`,
  `cargo/{fmt,clippy,test}`, `bazel/{build,test}`, `mav/expect`)
- `crates/musts` — the `musts` CLI binary
- `tests/fixtures/stub_extension` — protocol test stub, `publish = false`
- `.musts/ledger.lock.yaml` — committed validated-state lock; OS-portable
  (path hashes always lowercased — see
  `crates/musts-core/src/snapshot/paths.rs`)

---

> This file is the canonical source. `CLAUDE.md` is a symlink to it, so
> tools that look for either name read identical rules.
