# Contributing to musts

Thanks for considering a contribution. `musts` is pre-1.0 — small, focused PRs land fast.

## Toolchain

- Rust toolchain is pinned via [`rust-toolchain.toml`](rust-toolchain.toml) (stable channel + `rustfmt` + `clippy`).
- MSRV is **Rust 1.88** (declared in `Cargo.toml` and verified in CI).

If you have [`rustup`](https://rustup.rs/) installed, the right toolchain will be installed automatically the first time you build.

## Build and test

Everything goes through the `Makefile`:

```bash
make build      # cargo build --workspace
make test       # cargo test --workspace
make e2e        # cargo test --workspace --release --test '*'
make lint       # cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings
make all        # lint + test + e2e — run this before opening a PR
```

## Dogfooding (required)

`musts` validates itself with its own CLI. Before opening a PR:

```bash
cargo build --release

# If you haven't already, symlink the cargo extension into the local workspace
ln -sf "$(pwd)/target/release/cargo-extension" \
       .musts/extensions/cargo/cargo-extension

./target/release/musts validate
echo "exit=$?"   # must be 0
```

If `validate` returns pending tasks, capture the evidence using `musts evidence <task-id>` (see the [self-validation walkthrough in the README](README.md#self-validation)) until the loop converges. A PR that leaves `musts validate` red should not be merged — the project's whole premise is that the loop stays clean.

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/). This is required for the automated changelog and release flow.

Common prefixes:

- `feat:` — new user-facing capability (CLI command, MUSTS.yml field, extension contract)
- `fix:` — bug fix
- `docs:` — documentation only
- `refactor:` — internal change with no behavior diff
- `test:` — adding or fixing tests
- `chore:` — build, tooling, deps
- `perf:` — performance work

Breaking changes: add `!` after the type (`feat!:`) and a `BREAKING CHANGE:` footer explaining the migration.

## Changelog

Add a line under `## [Unreleased]` in [`CHANGELOG.md`](CHANGELOG.md) for any PR that changes user-visible behavior, output format, or the extension protocol. Doc-only or pure-refactor PRs do not need an entry.

## Versioning policy

Pre-1.0: minor versions may break the CLI surface, the extension protocol, or the `MUSTS.yml` format. Patch versions are bug-fix only. SemVer becomes strict from `1.0` onwards.

The four `musts-*` crates and the published extensions may release on independent cadences. Compatibility between `musts-core` and an extension is governed by the version of `musts-protocol` they both speak.

## Adding a capability or extension

See [`docs/extensions.md`](docs/extensions.md) for the full contract. In short:

1. Decide on the `uses: namespace/name` key.
2. Decide what the `with:` keys mean and what counts as valid evidence.
3. Implement the extension binary (it speaks JSON over stdio — see [`docs/architecture.md`](docs/architecture.md) and the existing extensions under `extensions/`).
4. Add tests against the contract.
5. If it's a new capability for an existing extension family, update the relevant `extension.yml`.

If you're using Claude Code or a similar agent, [`docs/skill.md`](docs/skill.md) drops into `.claude/skills/` and gives the agent the right mental model for the validation loop.

## Submitting a PR

- Branch from `main`.
- Keep PRs scoped — one capability, one fix, one refactor.
- CI must be green (`make all` locally is a good proxy).
- `musts validate` must be clean on the changed scope.
- Add a `CHANGELOG.md` entry if applicable.

## License

By contributing, you agree your contributions will be dual-licensed under MIT and Apache-2.0, matching the project itself.
