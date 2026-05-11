# Architecture

`harness` is a single CLI binary that drives a validation loop between agents and extensions. The authoritative source is [`PLAN.md`](PLAN.md) §4; this document gives the bird's-eye view a contributor needs to navigate the code.

## Crates

| Crate | Role |
|---|---|
| `crates/harness-protocol` | Pure serde types for the JSON-over-stdio extension protocol. Zero behaviour. |
| `crates/harness-extension-util` | Helpers for Rust extension authors: stdio framing (`ipc_main`, `read_request`, `write_response`), MIME-based asset classification (`asset_kind::*`). |
| `crates/harness-core` | All domain logic — manifests, snapshots, state, extension runtime, validate orchestrator, evidence pipeline. |
| `crates/harness` | The CLI binary. Argument parsing (`clap`), error rendering, exit codes. |
| `extensions/bazel-build` | Reference `bazel/build` extension. Deepest-target policy. |
| `extensions/mav-expect` | Reference `mav/expect` extension. Per-scope grouping + MIME-driven evidence validation with JSON-parse on `mav-report` / `accessibility-tree` assets. |
| `tests/fixtures/stub_extension` | Configurable test stub used by the integration suite. Behaviour driven by `HARNESS_STUB_*` env vars (PLAN.md §7.2.1). |

The protocol crate is the only dependency boundary between core and extensions. Third-party extensions ignore the Rust crates entirely; the JSON wire shape is the contract.

## The two pipelines

### `harness validate`

Implements `PLAN.md` §4.1.

```text
workspace::resolve → bootstrap (lock + state.sqlite) →
gc_orphan_submissions →
discover manifests + parse + with_validation →
discover extensions + ext_descriptor_hash →
for each check: compute effective scope (carve out same-capability deeper manifests) → scope_hash →
  is_green(check, scope_hash)? → dirty
group dirty checks by capability →
for each capability: ResolveRequest → extension.resolve → ResolveResponse →
  ingest tasks + ignored_checks + notes →
truncate-and-insert tasks (one transaction) →
render text or --json →
exit 0 if clean, 1 if pending, 2 on configuration error.
```

Idempotent: a re-run on an unchanged workspace yields the same task list, the same `task_snapshot_hash` values, and the same JSON output.

### `harness evidence`

Implements `PLAN.md` §4.2.

```text
workspace::resolve → bootstrap →
fetch_task(task_id) → TaskNotFound (exit 2) if missing →
compute_current_scope_hashes → recompute task_snapshot_hash →
  EvidenceStale (exit 2) if drifted →
EvidenceStore::allocate(submission-NNN) →
copy assets, MIME-detect, build EvidenceSubmission →
extension.evidence → EvidenceValidationResponse →
  if !accepted: EvidenceRejected (exit 1, message + missing list) →
  reject over-claims (EvidenceOverclaim, exit 2) →
insert atomic ledger rows (one per accepted-now check, keyed by declaring-manifest scope_hash) →
write evidence.json LAST →
exit 0.
```

Atomic: every accept is one SQLite transaction. The `evidence.json` marker file is written *after* the commit so an interrupted submission leaves an identifiable orphan that the next `validate`'s GC reclaims.

## Snapshots

`PLAN.md` §4.5 documents the per-check effective scope and the scope hash:

- A check's effective scope is the files under its declaring manifest's folder **minus** files under any deeper manifest that declares a check of the **same capability**. The carve-out keeps a check stable when sibling capabilities churn.
- Scope hash = `blake3(sorted_files ++ manifest_hash ++ ext_descriptor_hash ++ sorted_descendant_manifest_paths)`. Including descendant paths means adding or removing a child manifest invalidates the parent.
- Paths are NFC-normalised before hashing; on case-insensitive filesystems we lowercase them too. The check's `state.sqlite` row is therefore stable across NFC ↔ NFD checkouts and case variations.

## Cross-process locking

Every state-writing command acquires an advisory file lock on `.harness/.lock` (`fs2::FileExt::try_lock_exclusive`). On contention we exit 2 with `"another harness process is running"`; we never block. The lock is held through the SQLite transaction that mutates `tasks` (validate) or `evidence_records` (evidence). Read-only paths (`--help`, `--version`) skip bootstrap entirely.

## Convergence model

Detailed in `PLAN.md` §4.5. The short version: a check goes green only when an extension lists it in an evidence-accept's `satisfies` array. The reference `bazel/build` extension subsumes ancestor checks into a deeper task's `satisfies` so one evidence submission closes the loop on both. Extensions that prefer not to subsume can use `ignored_checks` alone — the loop still terminates because completion is "no pending tasks", not "every check has a ledger row".

## Where to look

- Manifest parsing edge cases → `crates/harness-core/src/manifest/parser.rs`.
- Why a check is or isn't dirty → `crates/harness-core/src/validate.rs::run` + `evidence::ledger::is_green`.
- Why an extension returned what it did → run the extension manually with the captured request JSON on stdin (the IPC contract is documented in `PLAN.md` §4.6).
- Why a workspace path didn't resolve → `crates/harness-core/src/workspace.rs`.
- What the JSON report looks like → `crates/harness-core/src/report.rs` + the §5 example in `PLAN.md`.
