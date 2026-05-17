# Docs

Pick the entry point that matches what you're doing:

## I want to use `musts` in my repo

- [**Skill guide**](skill.md) — the mental model for the agent: how `validate`
  and `evidence` fit together, how to use `.mustsignore`, when to dispatch
  sub-agents.
- [**Extensions**](extensions.md) — how to write a third-party extension
  in any language using the JSON-over-stdio protocol. Includes a worked
  example in `bash`.

## I want to understand the design

- [**Design spec**](musts-design.md) — the v0.2 specification: every
  decision, the protocol contract, success criteria, and the §15
  end-to-end walkthrough.
- [**Architecture**](architecture.md) — bird's-eye view of the workspace
  crates and the `validate` / `evidence` pipelines.

## I want to contribute

- [**Implementation plan**](PLAN.md) — the source of contract decisions
  (~30 review rounds applied). Read this before proposing protocol
  changes.
- [**CONTRIBUTING**](../CONTRIBUTING.md) — toolchain, build, test, and
  Conventional Commits style for PR titles.
- [**AGENTS.md**](../AGENTS.md) — the rules an AI coding agent must
  follow when opening a PR. `CLAUDE.md` is a symlink to this file.

## Examples

- [`examples/eslint-check/`](examples/eslint-check/) — a complete
  third-party extension written in bash that adds an ESLint capability,
  exercised in the integration tests.
