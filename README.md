<p align="left">
  <img src="assets/logo.png" alt="musts logo" width="240">
</p>

# musts

[![CI](https://github.com/bitomule/musts/actions/workflows/ci.yml/badge.svg)](https://github.com/bitomule/musts/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/musts.svg)](https://crates.io/crates/musts)
[![MSRV](https://img.shields.io/badge/MSRV-1.88-blue.svg)](rust-toolchain.toml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

AI agents are fast at editing code. They are less reliable at knowing when
verification is actually finished.

`musts` gives your repository a local, enforceable definition of done:

> The task is not done until `musts validate` is empty.

<p align="center">
  <img src="assets/hero.png" alt="A musts validate output showing two pending tasks with the exact cargo commands needed to resolve them" width="820">
</p>

Instead of hoping the agent remembers every build, test, UI check, and
architecture rule, you declare those checks next to the code they protect.
When files change, `musts validate` reports the exact validation tasks still
pending. The agent runs them, records evidence, and repeats until the report
is clean.

## Get Started

### 1. Install the CLI

```bash
brew install bitomule/tap/musts
```

Homebrew is the supported channel. It installs both `musts` and `musts-jev`, the
runner behind `uses: jev`.

If you already have it, upgrade rather than assume: a stale local install is the
most common reason a capability appears to be missing.

```bash
brew update && brew upgrade musts
musts --version
```

Other channels exist (`cargo install musts --locked`, `cargo binstall musts`, or the
binaries on GitHub Releases) but are not the one tested here. Installing through more
than one will leave two binaries on your PATH, and whichever comes first wins — which
is confusing precisely when you are trying to work out why a new feature is absent.

### 2. Create your first `MUSTS.yml`

Put a `MUSTS.yml` at the root of your repo:

```yaml
checks:
  test:
    uses: cargo/test
```

Now ask what still needs to be validated:

```bash
musts validate
```

If code covered by that manifest has changed, `musts` returns a concrete task
for the agent to complete. After running the requested command, the agent
records evidence:

```bash
cargo test --workspace 2>&1 | tee /tmp/musts-cargo-test.log

musts evidence cargo-test-root \
  --text "cargo test --workspace passed" \
  --asset /tmp/musts-cargo-test.log

musts validate
```

When `musts validate` is empty, the repo has fresh evidence for the current
workspace state.

### 3. Tell your agent to obey the loop

For Claude Code, install the plugin. It bundles the `musts` skill and a
`Stop` hook that runs `musts validate` whenever Claude tries to finish a turn:

```text
/plugin marketplace add bitomule/musts
/plugin install musts@musts
```

See [`docs/claude-code-plugin.md`](docs/claude-code-plugin.md) for install,
update, uninstall, and private-fork details.

For other agents, add the rule to your `AGENTS.md`, `CLAUDE.md`, or equivalent
repo instructions:

```md
Before declaring a code change done, run `musts validate`.
Treat every reported task as required. Run the task, capture evidence outside
the workspace, submit it with `musts evidence`, and repeat until
`musts validate` is empty.
```

The CLI is agent-agnostic. Anything that can run shell commands can participate
in the loop.

## What You Can Encode

`musts` is not limited to "run the test suite". A check can represent any
validation rule your repo needs before an agent is allowed to stop.

### Build and test checks

```yaml
checks:
  fmt:
    uses: cargo/fmt
  clippy:
    uses: cargo/clippy
  test:
    uses: cargo/test
```

### Targeted build checks

```yaml
checks:
  app-build:
    uses: bazel/build
    with:
      target: //App:App
```

Use `exclude_paths` to carve files out of a check's scope so editing them
doesn't re-open it — for example a version file bumped by release automation:

```yaml
checks:
  app-build:
    uses: bazel/build
    exclude_paths:
      - "tools/config.bzl"   # release automation bumps build_number here
    with:
      target: //App:App
```

`exclude_paths` applies after `paths`. Note that musts does **not** support
gitignore-style `!` negation inside `paths:` (it would silently match
nothing) — a leading `!` is now rejected with a manifest error pointing you
at `exclude_paths`.

### Product or architecture contracts

Use the built-in `agent` capability when the validation is a judgement call
that needs a human-readable answer rather than a command exit code:

```yaml
checks:
  usecase-shape:
    uses: agent
    paths:
      - "Sources/App/UseCases/**"
    with:
      facts:
        - "Every use case has exactly one public entry point."
        - "The entry point name describes the user action, not implementation detail."
        - "No use case reaches across module boundaries except through declared ports."
```

When a matching use case changes, `musts validate` asks the agent to verify
those facts and submit a text explanation. That makes repo-specific rules
visible, repeatable, and hard to forget.

### UI and device checks

`musts` can also gate flows that need screenshots, videos, JSON reports, or
other assets. Built-in and third-party capabilities decide what evidence they
need; the agent should follow the `evidence:` and `submit:` lines in the
`musts validate` report.

## How The Loop Works

<p align="center">
  <img src="assets/loop.png" alt="The musts loop: agent edits code, musts validate, run tasks and capture evidence, submit evidence, repeat until empty" width="720">
</p>

1. You place `MUSTS.yml` files next to the code they protect.
2. The agent edits code.
3. `musts validate` fingerprints the relevant files and finds checks whose
   current scope is no longer covered by accepted evidence.
4. Each capability turns dirty checks into concrete tasks.
5. The agent runs those tasks and submits evidence with `musts evidence`.
6. The loop repeats until `musts validate` is empty.

The ledger is content-based, not git-based. Comments, generated fixtures,
architecture docs, and source files all count if they are inside a check's
scope. That conservative model is intentional: `musts` does not guess whether
a change was "semantic enough" to need validation.

## Why `musts`?

### Why not just run `cargo test` or a Makefile?

Because the agent has to remember to do it. `make all` is a suggestion;
`musts validate` is a contract the turn cannot close around. The task list is
generated from what actually changed, so the repo does not need one giant
script for every possible validation path.

### Why not a pre-commit hook or CI-only check?

Pre-commit hooks can be skipped. CI runs after the agent has already stopped,
you have already moved on, and the context needed to fix the issue may be
gone. `musts` runs in the gap between "the agent says done" and "you believe
it".

### Why not just trust the agent?

Agents are good at finishing turns. They are not always good at finishing
work. `musts` makes a false "done" visible by moving verification into an
external, repo-owned loop.

<p align="center">
  <img src="assets/before-after.png" alt="A comparison of two terminal sessions: on the left, an agent says 'done' without running any checks. On the right, the same agent runs musts validate, sees a cargo test task is still pending, runs it, and only then closes the turn." width="820">
</p>

## Built-In Capabilities

The reference capabilities are built into the `musts` binary:

| Capability | Use it for |
| --- | --- |
| `agent` | Text-backed contracts, architectural checks, manual reasoning tasks |
| `cargo/fmt` | `cargo fmt --check` |
| `cargo/clippy` | `cargo clippy --workspace --all-targets -- -D warnings` |
| `cargo/test` | `cargo test --workspace` |
| `bazel/build` | Bazel target builds |
| `bazel/test` | Bazel test targets, grouped into one run per scope |
| `mav/expect` | Mobile Agent Verifier flows and device evidence |

Third-party extensions can add new capabilities in any language that speaks
the JSON-over-stdio protocol. See [`docs/extensions.md`](docs/extensions.md)
and the worked example in [`docs/examples/eslint-check/`](docs/examples/eslint-check/).

## Example: This Repo

`musts` validates itself on every PR. Its root manifest gates formatting,
linting, and tests; the protocol crate also carries an `agent` contract for
facts that should remain true across changes.

That dogfood loop is intentionally the same loop users run:

```bash
cargo build --release
./target/release/musts validate
# run the reported tasks
./target/release/musts evidence <task-id> --text "..." --asset /tmp/log
./target/release/musts validate
```

For contributor commands, release rules, and the required pre-PR validation
sequence, see [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Used At

`musts` runs in production on:

- [Undolly](https://undolly.app) - finding duplicate photos
- [Boxy](https://boxy-app.com/) - organising physical items
- [HiddenFace](https://hiddenface.app) - privacy-first face blur

## Commands

```bash
musts validate                                 # report pending validation tasks
musts validate --json                          # machine-readable report
musts run <task-id>                            # execute a deterministic task and record it
musts evidence <task-id> --text "..." \        # record evidence for a judgment task
    --asset path/to/log --asset path/to/screen.png
musts lint                                     # authoring checks on every MUSTS.yml
musts stats                                    # what each check has cost, and caught
musts calibrate                                # does each `uses: jev` question decide anything?
musts calibrate --no-record                    # same, without updating .musts/calibration.json
```

Exit codes:

- `validate`: 0 clean, 1 pending tasks, 2 configuration / stale / lock error, 70 internal error.
- `evidence`: 0 accepted, 1 rejected by extension, 2 unknown task / stale snapshot / over-claim, 70 internal error.
- `lint`: 0 clean or advice only, 1 an error-level finding (the manifest does not do what it says).
- `stats`: always 0 — it reports, it does not judge.
- `calibrate`: 0 when it ran, 70 when it could not judge a control (it then writes nothing).

`lint`, `stats` and `calibrate` are read-only with respect to validation
state and take no workspace lock, so none of them blocks on (or blocks) a
running `validate`.

### Calibrating a judgment check

A `uses: jev` question is prose, and prose can be wrong in a way nothing
notices: a question that can never fire looks exactly like a question on
a clean repo. Both answer "no" to everything.

`musts calibrate` separates them. It runs each question against real file
states from the repo's own history **and** against two files the check
declares: one that really breaks the rule, and a near-miss that does not.

```yaml
  self-comparing-assertions:
    uses: jev
    with:
      ask: self_comparing
      expect: "no"
      mode: shadow
      control:
        violating: .musts/controls/self-comparing/violating.rs
        clean: .musts/controls/self-comparing/clean.rs
```

Six verdicts, and two of them are the point:

| verdict | what it means |
|---|---|
| `broken` | a control answered the wrong way — the question does not work |
| `uncontrolled` | no control declared, so nothing below can be read |
| `thin` | too little history to judge; the controls stand alone |
| `noisy` | fires on most of real history, so it decides nothing |
| `mute` | **nothing to find here yet** — the question works, the repo is clean |
| `decisive` | fires on a minority, and its controls hold |

Three things it deliberately does:

- **It never edits `MUSTS.yml`.** For `broken`, `noisy` and
  `uncontrolled` it prints the exact edit and stops. A check disabled
  silently is the same quiet green the loop exists to remove.
- **It samples only commits older than the one the question first
  appeared in**, so a rule is never scored against changes it already
  approved. When that leaves fewer samples than asked for, it says so
  rather than reaching for newer history.
- **It records `.musts/calibration.json`, committed**, with the boundary
  commit and every probability, so a reviewer can see what a judgment
  check was calibrated against without running anything.

It needs a jev key and does not run under `CI` or `JEVI_DISABLE`; there
it fails loudly and records nothing.

`control:` is a new `with` field, so a `musts` older than the one that
ships `calibrate` will reject a manifest that declares it — upgrade
before adding the block. This repo's own manifest therefore picks it up
in a follow-up, once a release carrying `calibrate` is installed.

### Asking about one place instead of one file

A `uses: jev` question is answered about the whole file by default, which is
right for a question about a file. For a question about what one call
*sends* it is not: in a 2,000-line feature file the call is 0.1% of the
state, and a verdict per file cannot say which line.

```yaml
  analytics-privacy:
    uses: jev
    paths:
      - "Boxy/**/*.swift"
    with:
      questions: builtin:swift-analytics-privacy
      ask: analytics_user_data
      expect: "no"
      mode: shadow
      sites: swift-analytics
      changed_since: origin/main
```

- **`sites:`** cuts the state down to one emission site per request, and the
  report then names `path:line`. `swift-analytics` finds calls on a receiver
  named for tracking (`tracking.execute(...)`, `trackingClient.track(...)`)
  and the `case` declarations of the app's own event enum. Measured against
  `grep` on two real apps: 109 of 109 sites in one, 41 of 41 in the other,
  none extra.
- **`changed_since:`** keeps only the sites the change touched. Without it a
  one-line edit to a 109-site app asks about every site in the file it
  touched, and pays for each.
- **`questions: builtin:<name>`** addresses a question set that ships inside
  the binary, so repos asking the same question ask the same text instead of
  each holding a copy that can drift.

`musts-jev sites [--changed-since <rev>] <file>...` prints what a run would
ask about without asking. It is the only way to separate "it found nothing"
from "it looked at nothing", and both print a green.

### `WARN`: an abstention that leans the wrong way

`UNSURE` is green, because a tripwire that fires on abstention is noise. But
on one planted violation run six times, the probability came back 0.87 to
0.93 — straddling the edge of the abstention band, so the same unchanged file
read `FAIL` four times and `UNSURE` twice, and the `UNSURE` was printed
exactly like the clean twin of that file, which answers 0.11.

`WARN` is an abstention that fell on the violating side of even. Nothing is
fitted to produce it and it moves no exit code: a tripwire still fails only
on a decided verdict. It exists so a report distinguishes "almost certainly"
from "certainly not", and `calibrate` counts it as fired so a control does
not hold or not hold depending on the run.

## Stability

`musts` is pre-1.0. The CLI surface, extension protocol, and `MUSTS.yml`
schema may change between minor versions until `1.0`. The validation loop is
already used by this repository and by production apps, but you should expect
some API movement while the format settles.

## Docs

Start at [`docs/README.md`](docs/README.md) for the documentation index.

- [`docs/claude-code-plugin.md`](docs/claude-code-plugin.md) - Claude Code plugin and pre-commit validation hook.
- [`docs/skill.md`](docs/skill.md) - copyable agent instructions for the validation loop.
- [`docs/extensions.md`](docs/extensions.md) - how to write a third-party extension.
- [`docs/architecture.md`](docs/architecture.md) - bird's-eye view of the crates.
- [`docs/musts-design.md`](docs/musts-design.md) - design spec and protocol decisions.
- [`docs/PLAN.md`](docs/PLAN.md) - implementation plan and historical contract notes.

Advanced topics:

- [`.mustsignore`](docs/musts-design.md#55-workspace-mustsignore) - exclude committed generated files or canonical fixtures from scope hashes.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) - build, test, release, and PR-title rules.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
