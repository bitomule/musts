# CLAUDE.md

Repo-local guidance for Claude Code (and any other AI agent) working in `musts`.
Humans should read [`CONTRIBUTING.md`](CONTRIBUTING.md) for the full contributor
guide; this file is the terse version for agents.

## PR titles are the contract

The release flow squash-merges every PR into `main`. The squash commit on
`main` inherits its message **from the PR title**, not from the branch name
and not from intermediate commits. That squash commit is what
[release-plz](https://release-plz.dev/) parses to grow the rolling release PR
and the next changelog entry.

Therefore PR titles MUST follow Conventional Commits. Branch names are free —
they never reach `main`.

Allowed prefixes:

- `feat:` — new user-facing capability (CLI command, `MUSTS.yml` field, extension contract)
- `fix:` — bug fix
- `docs:` — documentation only
- `refactor:` — internal change with no behaviour diff
- `test:` — tests only
- `chore:` — build, tooling, deps, CI
- `perf:` — performance work
- `ci:` — workflow / CI changes

Scope is optional: `fix(core): ...`, `chore(deps): ...`.

Breaking changes: append `!` (e.g. `feat!: ...`) and add a `BREAKING CHANGE:`
footer in the PR body. In 0.x this still goes out as a minor bump — see
versioning below.

Intermediate commits inside a feature branch can say whatever they want;
they vanish on squash-merge.

## Build, test, dogfood — always before opening a PR

```bash
make all                                # fmt + clippy + test + e2e
cargo build --release --locked          # required for the dogfood step
ln -sf "$(pwd)/target/release/cargo-extension" \
       .musts/extensions/cargo/cargo-extension
./target/release/musts validate         # must exit 0
```

If `musts validate` reports pending tasks, run the listed commands, capture
logs **outside** the workspace (e.g. `$TMPDIR`), and submit evidence with
`./target/release/musts evidence <task-id> --text "..." --asset <log>` until
the validation loop is empty. Commit the refreshed `.musts/ledger.lock.yaml`.
The CI `musts validate (self)` job is a required check and runs the same
loop.

## Changelog — do not edit by hand

`release-plz` regenerates `CHANGELOG.md` from Conventional Commits in the
release PR it maintains on `main`. Feature PRs should not touch
`CHANGELOG.md` — any manual edit gets overwritten when the release PR is
cut.

## Versioning

Pre-1.0:

- minor bump may break the CLI surface, the extension protocol, or the
  `MUSTS.yml` format
- patch bump is bug-fix only
- breaking changes (`feat!:`) still ship as minor; `release-plz` does not
  auto-bump majors below 1.0

Strict SemVer from 1.0 onwards.

## Workspace layout (so you don't have to grep)

- `crates/musts-protocol` — JSON-over-stdio wire types shared with extensions
- `crates/musts-extension-util` — Rust helpers for extension authors
- `crates/musts-core` — orchestrator: manifests, snapshots, scope hashes, ledger
- `crates/musts` — the `musts` CLI binary
- `extensions/{cargo,bazel-build,mav-expect}` — reference extensions
  (`publish = false`, ship inside the binary release, not on crates.io)
- `tests/fixtures/stub_extension` — protocol test stub (`publish = false`)
- `.musts/extensions/cargo/` — runtime extension descriptor + symlinked binary
  for self-validation
- `.musts/ledger.lock.yaml` — committed validated-state lock; OS-portable
  (path hashes are always lowercased — see
  `crates/musts-core/src/snapshot/paths.rs`)

## Don't open PRs that

- Edit `CHANGELOG.md` directly (let release-plz do it).
- Add `SECURITY.md`, code-of-conduct, or issue/PR templates unless asked —
  this is a solo-maintainer project that wants minimal ceremony.
- Touch branch protection on `main` (no reviewer required, but `ci.yml`
  checks must stay green for merge).
- Bypass the dogfood loop. `musts` is the very project being built; the
  loop must stay clean on every PR.
