# harness

An agent-first validation loop for code repositories.

> The task is not done until `harness validate` is empty.

`harness` is a small CLI that tells an agent **what must be validated after a change, how to produce evidence, and when the work is allowed to be called done**. It is not a test runner, not CI, not another `CLAUDE.md` — it is the missing validation loop between agent work and trustworthy completion.

## Status

Pre-MVP. See [`docs/PLAN.md`](docs/PLAN.md) for the implementation plan and [`docs/harness-validation-plan.md`](docs/harness-validation-plan.md) for the full design spec.

## How it works (one paragraph)

You drop `HARNESS.yml` files anywhere in your repo. Each one declares validation *checks* (build this target, validate this user flow with MAV, run this Playwright check…). When the agent finishes a change, it runs `harness validate`. The CLI looks at what changed (using content fingerprints, not git), groups checks by capability, and asks each extension *"given these checks and this dirty scope, what tasks does the agent actually need to do?"*. The extension answers with concrete tasks. The agent runs them, captures evidence (text + assets), and submits it through `harness evidence <task-id>`. The extension decides whether the evidence is good enough. Repeat until `harness validate` is empty.

## Commands (MVP)

```bash
harness validate                                 # report pending validation tasks
harness evidence <task-id> --text "..." \        # record evidence for a task
    --asset path/to/log --asset path/to/screen.png
```

## Build

```bash
cargo build --release
./target/release/harness validate
```

## License

TBD.
