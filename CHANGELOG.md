# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **Pre-1.0:** minor versions may include breaking changes to the CLI surface,
> the extension protocol, or the `MUSTS.yml` format. Patch versions are
> bug-fix only.

## [Unreleased]

The first published release will be `0.1.0` and will include everything in this
section.

### Added

- Per-check `paths` filter so a check only triggers when matching files change.
- Built-in `agent` capability for contract-style checklists, with a
  shell-script extension example under `docs/examples/eslint-check/`.
- Portable validated-state ledger lock file (`.musts/ledger.lock.yaml`) that
  carries the team's validated state across clones.
- Self-validation: built-in `cargo` extension + `musts` workspace at the repo
  root, including `cargo/fmt`, `cargo/clippy`, and `cargo/test` capabilities.
- Project logo in `assets/`.
- `LICENSE-MIT` and `LICENSE-APACHE` (dual-licensed at the user's option).
- `CONTRIBUTING.md` covering toolchain, build/test, mandatory dogfooding,
  Conventional Commits, and pre-1.0 versioning policy.
- GitHub Actions CI on every PR and push to `main`: `fmt`, `clippy` (ubuntu +
  macos), `test` (ubuntu + macos), `e2e` (release on ubuntu), `msrv` against
  Rust 1.81, and `dogfood` running `musts validate` on the repo itself.
- Dependabot for `cargo` and `github-actions` with weekly minor+patch grouping.

### Changed

- Renamed the project from `harness` to `musts` across crate directories,
  Cargo identifiers, user-facing strings (`MUSTS.yml`, `.musts/`, `MUSTS_*`),
  fixtures, manifests, on-disk paths, and documentation.
- Missing `with:` block in a check now defaults to `{}` instead of `null`.
- External descriptors shadow built-ins for `resolve`, so a workspace can
  override a bundled capability without forking.
- README: pre-1.0 status, badges, dual-license section replacing "TBD".

### Fixed

- Surfaced extension stderr on every protocol-error path.
- Several review-round blocking issues (Phase 3, Phase 6, Phase 7).
- Stopped tracking fixture runtime state (state.sqlite, .lock, evidence/)
  in version control.

### Internal

- 0–7 implementation phases per `docs/PLAN.md`: workspace skeleton, manifest
  model, extension IPC, `validate` orchestrator, `evidence` command + ledger
  semantics, reference extensions (`bazel/build`, `mav/expect`), agent skill,
  architecture and extension docs, canonical fixture.

## Release notes format

Each release section follows the
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) categories:
**Added**, **Changed**, **Deprecated**, **Removed**, **Fixed**, **Security**.

Internal-only changes (refactors, CI, build tooling) are grouped under
**Internal** and may be omitted from release notes.
