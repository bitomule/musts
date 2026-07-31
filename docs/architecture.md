# Architecture

`musts` is a single CLI binary that drives a validation loop between agents and extensions. The authoritative source is [`PLAN.md`](PLAN.md) §4; this document gives the bird's-eye view a contributor needs to navigate the code.

## Crates

| Crate | Role |
|---|---|
| `crates/musts-protocol` | Pure serde types for the JSON-over-stdio extension protocol. Zero behaviour. |
| `crates/musts-extension-util` | Helpers for Rust extension authors: stdio framing (`ipc_main`, `read_request`, `write_response`), MIME-based asset classification (`asset_kind::*`). |
| `crates/musts-core` | All domain logic — manifests, snapshots, state, extension runtime, validate orchestrator, evidence pipeline. Also hosts the **built-in capabilities** under [`src/builtin/`](../crates/musts-core/src/builtin/): `agent`, `cargo/{fmt,clippy,test}`, `bazel/build`, `mav/expect`. |
| `crates/musts` | The CLI binary. Argument parsing (`clap`), error rendering, exit codes. |
| `tests/fixtures/stub_extension` | Configurable test stub used by the integration suite. Behaviour driven by `MUSTS_STUB_*` env vars (PLAN.md §7.2.1). |

The protocol crate is the only dependency boundary between core and external extensions. Third-party extensions ignore the Rust crates entirely; the JSON wire shape is the contract. The built-in capabilities live inside `musts-core` and skip the IPC hop — at lookup time, an external descriptor wins over the built-in registry so a workspace can override or replace a built-in by shipping its own `.musts/extensions/<name>/extension.yml`.

## The two pipelines

### `musts validate`

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

### `musts evidence`

Implements `PLAN.md` §4.2.

```text
workspace::resolve → bootstrap →
fetch_task(task_id) → TaskNotFound (exit 2) if missing →
compute_current_scope_hashes → recompute task_snapshot_hash →
  EvidenceStale (exit 2) if drifted →
describe_asset(path) in place (MIME-detect, size; no copy) → build EvidenceSubmission →
extension.evidence → EvidenceValidationResponse →
  if !accepted: EvidenceRejected (exit 1, message + missing list) →
  reject over-claims (EvidenceOverclaim, exit 2) →
insert atomic ledger rows (one per accepted-now check, keyed by declaring-manifest scope_hash) →
exit 0.
```

Atomic: every accept is one SQLite transaction. Evidence is **not** archived — assets are validated where they live and the committed `.musts/ledger.lock.yaml` (plus the per-machine `evidence_records` table) is the durable record. `musts run <task-id>` wraps this: it executes a deterministic built-in's command, captures the log outside the workspace, and drives the same pipeline from the real exit code.

## Snapshots

`PLAN.md` §4.5 documents the per-check effective scope and the scope hash:

- A check's effective scope is the files under its declaring manifest's folder **minus** files under any deeper manifest that declares a check of the **same capability**. The carve-out keeps a check stable when sibling capabilities churn.
- Scope hash = `blake3(sorted_files ++ manifest_hash ++ ext_descriptor_hash ++ sorted_descendant_manifest_paths)`. Including descendant paths means adding or removing a child manifest invalidates the parent.
- Paths are NFC-normalised before hashing; on case-insensitive filesystems we lowercase them too. The check's `state.sqlite` row is therefore stable across NFC ↔ NFD checkouts and case variations.

## Portable validated state — `ledger.lock.yaml`

`.musts/state.sqlite` is machine-local: a fresh `git clone` starts with no `evidence_records` rows, so without help every check would be re-emitted as dirty. The lock file at `.musts/ledger.lock.yaml` is the portable companion: a deterministic, sorted YAML list of `(check_id, scope_hash)` tuples that have been accepted somewhere. `validate`'s "is this green?" lookup unions the local SQLite query with the lock's `contains` check; `evidence`'s accept path appends the new tuple and rewrites the file.

What this buys: the team commits the lock alongside the manifests; a clone runs `musts validate` and only sees tasks for scopes its own changes have invalidated (the blake3 scope hashes match the lock for everything else). `state.sqlite` and `evidence/` stay gitignored — they are a perf cache and an asset payload, not the source of truth.

### Merging the lock

`satisfied` is append-only: two branches that each record evidence can add entries, never contradict them, so **the union of both sides is always the correct merge**. Two things make git do that on its own:

- **One line per entry**, written as a YAML flow mapping (`- {check: "root/build-ios", scope_hash: "833bc590…"}`). The default block style splits an entry over two lines, and every entry for the same check then shares an identical `- check: …` first line; a line-based merge aligns on those and splices two entries into one record with a duplicate `scope_hash` key — an unparseable ledger. This is a formatting change only: a flow mapping is an ordinary mapping, the file stays `version: 1`, and older musts releases keep reading it (`flow_style_output_is_still_read_by_a_plain_serde_derive` pins that down).
- **`.musts/.gitattributes`**, written next to the lock, setting `ledger.lock.yaml merge=union`. `union` is a built-in git driver, so no per-clone `git config` is needed — but the file has to be **committed** to take effect. An existing `.gitattributes` that already mentions the lock is left untouched.

Before this, both branches appending entries produced a conflict on every merge, and resolving it by taking one side silently discarded proven-green entries. That loss shows up later as "the merge invalidated the ledger". A lock that still contains conflict markers is now rejected with a message saying to keep both sides, rather than being half-parsed.

What this does *not* buy — and cannot — is inheriting green state across a merge whose result nobody validated. If `main` moved while a branch was open, the tree that lands is a combination neither side ever checked, so the checks covering it reopen. That is the correct answer, not a bug: two individually-green trees do not make their merge green. The levers are to keep branches up to date before merging (so the branch validates the tree that actually lands) and to scope checks narrowly with `paths:`/nested manifests, so unrelated churn does not invalidate an expensive check. `crates/musts/tests/git_merge_ledger_e2e.rs` pins down both halves.

## Cross-process locking

Every state-writing command acquires an advisory file lock on `.musts/.lock` (`fs2::FileExt::try_lock_exclusive`). On contention we exit 2 with `"another musts process is running"`; we never block. The lock is held through the SQLite transaction that mutates `tasks` (validate) or `evidence_records` (evidence). Read-only paths (`--help`, `--version`) skip bootstrap entirely.

## Convergence model

Detailed in `PLAN.md` §4.5. The short version: a check goes green only when an extension lists it in an evidence-accept's `satisfies` array. The reference `bazel/build` extension subsumes ancestor checks into a deeper task's `satisfies` so one evidence submission closes the loop on both. Extensions that prefer not to subsume can use `ignored_checks` alone — the loop still terminates because completion is "no pending tasks", not "every check has a ledger row".

## Where to look

- Manifest parsing edge cases → `crates/musts-core/src/manifest/parser.rs`.
- Why a check is or isn't dirty → `crates/musts-core/src/validate.rs::run` + `evidence::ledger::is_green` + `state::lock::LedgerLock::contains`.
- Why an extension returned what it did → run the extension manually with the captured request JSON on stdin (the IPC contract is documented in `PLAN.md` §4.6).
- Why a workspace path didn't resolve → `crates/musts-core/src/workspace.rs`.
- What the JSON report looks like → `crates/musts-core/src/report.rs` + the §5 example in `PLAN.md`.
