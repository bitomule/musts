# harness

An agent-first validation loop for code repositories.

> The task is not done until `harness validate` is empty.

`harness` is a small CLI that tells an agent **what must be validated after a change, how to produce evidence, and when the work is allowed to be called done**. It is not a test runner, not CI, not another `CLAUDE.md` — it is the missing validation loop between agent work and trustworthy completion.

## Status

MVP. Implements every phase in [`docs/PLAN.md`](docs/PLAN.md). The §19 success criterion runs end-to-end on [`fixtures/login-app/`](fixtures/login-app/) and is checked in as `phase6_e2e::full_section_15_worked_example`.

## How it works (one paragraph)

You drop `HARNESS.yml` files anywhere in your repo. Each one declares validation *checks* (build this target, validate this user flow with MAV, run this Playwright check…). When the agent finishes a change, it runs `harness validate`. The CLI looks at what changed (using content fingerprints, not git), groups checks by capability, and asks each extension *"given these checks and this dirty scope, what tasks does the agent actually need to do?"*. The extension answers with concrete tasks. The agent runs them, captures evidence (text + assets), and submits it through `harness evidence <task-id>`. The extension decides whether the evidence is good enough. Repeat until `harness validate` is empty.

## Commands

```bash
harness validate                                 # report pending validation tasks
harness validate --json                          # machine-readable report
harness evidence <task-id> --text "..." \        # record evidence for a task
    --asset path/to/log --asset path/to/screen.png
```

Exit codes:
- `validate`: 0 clean, 1 pending tasks, 2 configuration / stale / lock error, 70 internal error.
- `evidence`: 0 accepted, 1 rejected by extension, 2 unknown task / stale snapshot / over-claim, 70 internal error.

## Build

```bash
cargo build --release
./target/release/harness validate
```

Tests:

```bash
make test       # cargo test --workspace
make e2e        # cargo test --workspace --release --test '*e2e*'
make lint       # cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings
make all        # lint + test + e2e
```

## Self-validation

The repository validates itself with its own CLI. Three `cargo` capabilities (`cargo/fmt`, `cargo/clippy`, `cargo/test`) live in [`extensions/cargo`](extensions/cargo/) and validate evidence the agent collects; two scopes (`crates/harness-protocol/` and `extensions/cargo/`) carry `uses: agent` contracts that pin their responsibility as a checklist of facts.

Walk the loop end-to-end:

```bash
cargo build --release
ln -sf "$(pwd)/target/release/cargo-extension" \
       .harness/extensions/cargo/cargo-extension

# Touch something to dirty a scope
echo "// touch" >> crates/harness-protocol/src/lib.rs

# Pending: 3 cargo-* tasks + 1 agent-* contract task
./target/release/harness validate

# Capture real cargo output
mkdir -p /tmp/harness-self-evidence
{ echo "+ cargo fmt --check"; cargo fmt --check 2>&1; echo "exit=$?"; } \
  > /tmp/harness-self-evidence/fmt.log
{ echo "+ cargo clippy --workspace --all-targets -- -D warnings"; \
  cargo clippy --workspace --all-targets -- -D warnings 2>&1; \
  echo "exit=$?"; } > /tmp/harness-self-evidence/clippy.log
cargo test --workspace 2>&1 | tee /tmp/harness-self-evidence/test.log >/dev/null

# Submit evidence
./target/release/harness evidence cargo-fmt-root \
  --text "cargo fmt --check exited 0 with no diffs" \
  --asset /tmp/harness-self-evidence/fmt.log
./target/release/harness evidence cargo-clippy-root \
  --text "cargo clippy clean under -D warnings" \
  --asset /tmp/harness-self-evidence/clippy.log
./target/release/harness evidence cargo-test-root \
  --text "cargo test --workspace all green" \
  --asset /tmp/harness-self-evidence/test.log

# Agent contract: answer each fact in your --text
./target/release/harness evidence agent-crates-harness-protocol \
  --text "Fact 1: …  Fact 2: …  Fact 3: …  Fact 4: …"

# Converged
./target/release/harness validate ; echo "exit=$?"   # → 0
```

The contract task lists its facts under `Instructions:` in the `validate` output — your evidence text should address each one. Empty text is rejected (`agent_builtin_e2e::text_required`).

## Docs

- [`docs/harness-validation-plan.md`](docs/harness-validation-plan.md) — the v0.2 design spec.
- [`docs/PLAN.md`](docs/PLAN.md) — the implementation plan, ~30 review rounds applied; the source of contract decisions.
- [`docs/skill.md`](docs/skill.md) — the agent skill (drop into `.claude/skills/`).
- [`docs/architecture.md`](docs/architecture.md) — bird's-eye view of the crates.
- [`docs/extensions.md`](docs/extensions.md) — how to write a third-party extension.

## License

TBD.
