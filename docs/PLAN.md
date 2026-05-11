# `harness` MVP — Implementation Plan

Companion to [`harness-validation-plan.md`](harness-validation-plan.md) (v0.2 spec). This document picks the technology, architecture, and execution order to ship the MVP described in §18–§19 of the spec.

---

## 1. What we are actually building

A single CLI binary, `harness`, exposing two commands:

```bash
harness validate
harness evidence <task-id> --text "<summary>" [--asset <path>]...
```

…plus the runtime needed to make those commands meaningful: manifest discovery, snapshot/change detection, extension protocol (JSON over stdio), an evidence store, and a SQLite-backed ledger. We also ship two reference extensions (`bazel/build`, `mav/expect`) so we can prove the §15 worked example end-to-end.

Out of scope for MVP (matches §3 and §17 of the spec):
- Claude hooks, CI integration, prompt injection, facts system, full harness graph.
- Extension sandboxing, signing, allowlists.
- `harness init`, `harness doctor`, `harness install` (deferred to "later layers").
- Resource locks across parallel agents (we expose `parallelizable`; orchestration is the agent's job).

---

## 2. Technology choice

**Language: Rust (stable, edition 2021).**

Why Rust over the alternatives:

| Option | Verdict | Why |
|---|---|---|
| **Rust** | Chosen | Single static binary; first-class CLI tooling (`clap`, `serde`, `rusqlite`); the protocol has many serde-shaped structs that benefit from an exhaustive type system; `blake3` is the fastest hasher we will find and matters once the workspace gets large; cross-compiles cleanly to macOS+Linux. |
| Swift | Rejected | Familiar to the maintainer, but distribution outside macOS is fragile and the YAML/JSON/SQLite story is weaker than Rust's. The tool will be invoked by agents in many environments — not just on the maintainer's iOS laptop. |
| Go | Plausible | Comparable distribution story, simpler language. We prefer Rust's type system because the extension protocol's invariants (which fields are required, which response shapes are accepted) are easier to enforce at compile time. |
| Node/TS | Rejected | npm install latency + Node runtime requirement is hostile for an agent tool. |
| Python | Rejected | Same distribution story as Node + dependency hell. |

If the maintainer wants to revisit, the only honest alternative is Swift; we should commit to one choice now and only switch if a Phase-1 deliverable proves painful.

### Key Rust dependencies

- `clap` (derive feature) — argument parsing.
- `serde`, `serde_yaml`, `serde_json` — manifest + protocol serde.
- `jsonschema` — validate `with` payloads against extension-supplied JSON Schemas.
- `rusqlite` (bundled SQLite) — state DB. WAL mode.
- `blake3` — content hashing.
- `ignore` — `.gitignore`-aware directory walking (already used by `ripgrep`).
- `walkdir` — fallback raw traversal where we want to bypass ignore rules.
- `mime_guess` — asset MIME detection.
- `fs2` — advisory file lock for cross-process coordination (`.harness/.lock`).
- `unicode-normalization` — NFC normalisation of paths before hashing.
- `shell-words` — parse the descriptor's string-form `command`.
- `anyhow` (binary), `thiserror` (library boundaries) — error handling.
- `tracing` + `tracing-subscriber` — structured logs gated behind `--log`.
- `time` (or `chrono`) — timestamps in the ledger.

Dev-only:
- `assert_cmd` + `predicates` — drive the CLI from integration tests.
- `insta` — snapshot tests for CLI output and rendered reports.
- `tempfile` — scratch workspaces.
- `rstest` — parameterised tests.

---

## 3. Repository layout

```
validator-claude/
├── Cargo.toml                       # workspace manifest
├── rust-toolchain.toml              # pin stable
├── Makefile                         # thin wrapper (build/test/e2e)
├── README.md
├── docs/
│   ├── harness-validation-plan.md   # the spec (vendored)
│   ├── PLAN.md                      # this file
│   ├── architecture.md              # written in Phase 1
│   ├── extensions.md                # written in Phase 5
│   └── skill.md                     # the agent skill, written in Phase 7
├── crates/
│   ├── harness-protocol/            # pure serde types shared with extensions
│   │   ├── Cargo.toml
│   │   └── src/lib.rs               # ResolveRequest/Response, EvidenceRequest/Response, …
│   ├── harness-extension-util/      # helpers for Rust extension authors
│   │   ├── Cargo.toml
│   │   └── src/lib.rs               # stdio framing, asset-kind by MIME,
│   │                                # size/text guards. Lands in Phase 5 so
│   │                                # bazel-build & mav-expect share it.
│   ├── harness-core/                # library: all the domain logic
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── error.rs
│   │       ├── manifest/            # discovery + parsing + check IDs
│   │       ├── snapshot/            # blake3 hashing + ignore rules + scope hash
│   │       ├── state/               # sqlite layer + migrations
│   │       ├── extension/           # descriptor loading + IPC runtime
│   │       ├── evidence/            # asset copy, submission build, ledger write
│   │       ├── report/              # text renderer (plus future --json)
│   │       └── validate.rs          # orchestrator behind `harness validate`
│   └── harness/                     # binary entry point
│       ├── Cargo.toml
│       └── src/main.rs              # clap parser → core
├── extensions/
│   ├── bazel-build/                 # reference Rust extension
│   │   ├── Cargo.toml
│   │   ├── src/main.rs
│   │   ├── schemas/build.schema.json
│   │   └── extension.yml.template
│   └── mav-expect/
│       ├── Cargo.toml
│       ├── src/main.rs
│       ├── schemas/expect.schema.json
│       └── extension.yml.template
├── tests/                           # workspace-level E2E tests (Rust)
│   ├── common/                      # test harness: build workspaces, run binary
│   └── e2e/                         # one file per scenario
└── fixtures/                        # canned manifests + repos used by tests and docs
    └── login-app/                   # mirrors §15 of the spec
```

Rationale for splitting the protocol crate: Rust extensions (and any third party) can depend on `harness-protocol` to get type-safe IPC types without pulling in `rusqlite`, `blake3`, etc. Non-Rust extensions ignore the crate; the JSON shape is the source of truth.

### 3.1 Root `Cargo.toml` (workspace membership)

```toml
[workspace]
resolver = "2"
members = [
  "crates/harness-protocol",
  "crates/harness-extension-util",
  "crates/harness-core",
  "crates/harness",
  "extensions/bazel-build",
  "extensions/mav-expect",
  "tests/fixtures/stub_extension",
]

[workspace.package]
edition = "2021"
rust-version = "1.81"
```

Every crate (including the reference extensions and the stub used by tests) is a workspace member. That means `cargo build --workspace` produces every binary the test harness needs in one step, and the stub gets the same dependency versions as core — preventing serde-version drift between core and the IPC peer.

---

## 4. Core architecture

### 4.1 Pipeline for `harness validate`

```text
Discover manifests (cached)
        │
        ▼
Refresh fingerprints for files in scope (mtime+size first, blake3 only on diff)
        │
        ▼
Compute scope hashes (= blake3(sorted file hashes ++ manifest hash ++ ext descriptor hashes))
        │
        ▼
For each check:
    is its scope dirty? (= no green ledger row for current scope_hash)
        │
        ▼
Group dirty checks by `uses` capability
        │
        ▼
For each capability, call extension `resolve` over stdin/stdout JSON
        │
        ▼
Replace the `tasks` table with the current run's tasks (truncate within
the same transaction that inserts the new rows). Old tasks that the
extension no longer emits are gone, so `harness evidence` for a stale
task_id reports "task no longer applies — run harness validate". Render
report.
```

`harness validate` is read-mostly for the ledger + replaces the tasks table. It never writes `evidence_records`.

### 4.2 Pipeline for `harness evidence`

```text
Look up the task by id in the `tasks` table. If absent, render
"task no longer applies — run harness validate" and exit 2.
        │
        ▼
Recompute the task_snapshot_hash from the current scope_hashes of every
satisfied check; compare with the stored task_snapshot_hash. Mismatch =
stale → render §12.5 message and exit 2. **Only files inside a satisfied
check's effective scope contribute to staleness; edits anywhere else
(including under unrelated manifests) leave the evidence valid.**
```

**Re-validate semantics (resolution of the "which task is current?" question).** The `tasks` table is replaced wholesale by every `validate` (§4.1). A task id is therefore always "the most recent `validate`'s row for that id". If the agent runs `validate` twice without recording evidence in between:

- If the resolver returned the same id both times **and no files changed in between**, the second `task_snapshot_hash` equals the first, and the agent's queued evidence is still valid.
- If the resolver returned the same id but files changed in between, the second `task_snapshot_hash` differs — the queued evidence is now stale and `harness evidence` rejects it.
- If the resolver no longer returns the id (the relevant check went green via earlier evidence, or the resolver changed its mind), the row is gone and `harness evidence` returns "task no longer applies."

In short: re-running `validate` is equivalent to issuing a fresh task list. The agent skill (§14, Phase 7) tells agents to record evidence before re-running `validate`; the core mechanic that makes that rule load-bearing lives here.

```text
        │
        ▼
Copy assets into .harness/evidence/<task_id>/submission-NNN/
Compute mime, size, file hash
        │
        ▼
Build submission JSON, call extension `evidence` capability
        │
        ▼
On accept:
  - The extension's returned `satisfies` array is authoritative.
  - Each check_id in that array **must already appear in the task's
    original `satisfies` set** (looked up via `tasks.scope_hashes`).
    Over-claims (an extension trying to mark unrelated checks satisfied)
    are rejected with a clear error naming the offending check_ids.
  - For each remaining check, write one evidence_records row keyed by
    that check's **declaring-manifest scope_hash** (NOT the task's
    aggregate hash). The scope_hash is read from `tasks.scope_hashes`
    so the extension never needs to know it. Unlisted entries from the
    task's original `satisfies` remain pending so the next `validate`
    still emits a task for them. Wrap the inserts and the
    scope_snapshot upserts in a single SQLite transaction so a crash
    never half-greens a multi-check task.
  - **Write `evidence.json` to the submission dir last**, after the
    ledger transaction commits. A submission dir without an
    `evidence.json` is therefore a partial/aborted submission (e.g.
    Ctrl-C between asset copy and ledger write) and is treated as
    garbage. See §4.8.
On reject: print the rejection message to stderr, exit 1.
```

### 4.3 Module responsibilities

| Module | Responsibility | Key types |
|---|---|---|
| `manifest::discovery` | Walk the workspace once, find every `HARNESS.yml`, watch for new/removed ones on subsequent runs using dir mtimes. | `ManifestPath`, `ManifestTree` |
| `manifest::parser` | Validate `version: 1`, load checks, capture `with` opaquely. | `Manifest`, `Check`, `CheckId` |
| `manifest::with_validation` | After extensions are loaded, validate each check's `with` against the capability's JSON schema. Schema failures surface as **manifest errors** (exit 2), not extension failures, and report the manifest path + JSON pointer to the offending field. Runs before any `resolve` call. | — |
| `manifest::ids` | Build globally stable check IDs: `<scope_path>/<local_id>` (root scope uses `root/`). | `CheckId` |
| `snapshot::fingerprint` | mtime+size cache, lazy blake3 rehash. | `FileFingerprint` |
| `snapshot::scope` | Compute scope hashes; encapsulate the ignored-directories list. | `ScopeHash` |
| `state::db` | Open `.harness/state.sqlite`, apply migrations, expose typed CRUD. | `Db` |
| `state::tables` | Definitions for `manifest_index`, `file_fingerprints`, `scope_snapshots`, `tasks`, `evidence_records`. | — |
| `extension::descriptor` | Parse `.harness/extensions/*/extension.yml`. | `ExtensionDescriptor`, `Capability` |
| `extension::runtime` | Spawn child process, write request JSON, read response JSON, enforce timeout, surface stderr on failure. | `ExtensionRunner` |
| `evidence::store` | Allocate `submission-NNN` directory, copy + checksum assets, write `evidence.json` **last** (after the ledger commit) so an interrupted submission leaves identifiable garbage. The `normalized_assets` array from a successful evidence response (spec §10.4) is stored verbatim inside the ledger's `result_json` blob; no separate column in v1. | `EvidenceStore` |
| `evidence::ledger` | Read/write `evidence_records`, expose "is this check green for this scope hash?" | `Ledger` |
| `report::text` | Convert tasks + ignored checks + extension `notes` into the §11.2 output. `notes` render as a final "Notes:" section grouped by capability. The companion `--json` shape exposes `notes`, `ignored_checks`, and `tasks` as top-level arrays. | `Report` |
| `validate` | Top-level orchestrator. | `Validator` |

### 4.4 Stable IDs

- **Check ID (global)**: `<scope_path>/<local_id>`. The root manifest's scope path is literally `root` (not `.`), so a root-declared `app-build` is `root/app-build`. The format is the single source of truth across the report, ledger, and IPC; the report always prints the global ID, never just the local one. Two manifests with the same `local_id` produce distinct globals (`root/login-build` vs `App/Login/login-build`) and are valid; the report and ledger keep them apart.
- **Conflicts**: rejected at parse time only inside the same manifest. Cross-manifest collisions are impossible by construction because the scope path is part of the ID.
- **Task ID**: extension-provided. If two extensions return the same task id in one resolve cycle, the core prefixes them with the capability (`bazel/build:bazel-build-login`) and logs a warning.
- **Submission ID**: `submission-001`, `-002`, … allocated per task by counting existing dirs.

### 4.5 Snapshot model (concrete decisions)

- **Hash function**: blake3, 256-bit, hex-encoded. Reason: ~10× faster than SHA-256 on Apple Silicon; large repos hit IO long before CPU.
- **Per-check effective scope (important)**: a check's scope hash is computed over the files in its declaring manifest's folder **minus the files under any deeper manifest that declares a check of the same capability**. The carve-out stabilises a check's `scope_hash` and its `changed_files` list when only files under a deeper sibling are edited; it does **not** make the check "clean" in §4.1's dirty-detection sense. A check is dirty whenever no green ledger row matches the current `scope_hash` — independent of whether the hash actually changed since the previous run. So a never-satisfied root check will always be dirty and always fan out to the resolver; the carve-out's job is to ensure the resolver sees the *same* input (`scope_hash`, `changed_files`) on each idle re-run and therefore returns the *same* `ignored_checks` answer. **Convergence does not depend on this carve-out** — the completion criterion is "no pending *tasks*", and an extension returns checks it does not want to act on in `ignored_checks`. The carve-out keeps that decision local and the IPC calls stable; it is not load-bearing for the agent loop terminating.
- **File granularity**: every file under the effective scope, minus the ignore list. Ignored: `.git/`, `.harness/evidence/`, `.harness/cache/`, `node_modules/`, `target/`, `bazel-bin*`, `bazel-out*`, `bazel-testlogs*`, `DerivedData/`, `*.xcodeproj/xcuserdata/`. Also obeys `.gitignore` via the `ignore` crate so derived files outside Git but inside the workspace still count.
- **Scope hash**: `blake3(sorted_join(rel_path || "\0" || file_hash) || "\0" || manifest_hash || "\0" || ext_descriptor_hash || "\0" || sorted_join(descendant_manifest_rel_path))`. Including descendant manifest *paths* (not contents — those are hashed for their own scopes) means adding/removing a child manifest invalidates the parent's idea of "what's applicable to me." Including the extension descriptor hash means swapping or upgrading an extension invalidates evidence. Both are explicit and intentional.
- **Per-check scope_hash provenance**: a check is always hashed against its **declaring manifest's** scope, never against the task that ends up satisfying it. A task that satisfies checks from two different manifests records two distinct ledger rows, each keyed by the corresponding declaring-manifest scope hash.
- **Manifest hash**: blake3 of the file bytes.
- **Cheap path**: on second+ runs, if a file's mtime+size match the cached fingerprint, reuse the stored hash. Only recompute hashes when (mtime OR size) changes.
- **Discovery invalidation**: we do **not** rely on directory mtimes to detect new/removed manifests — APFS and some git operations leave them stale. Every run does a parallel `ignore::WalkBuilder` traversal of the workspace, which is fast enough on warm caches (low ms for >100k files) and is the only correctness-safe option. The cached `manifest_index` provides the previous state so we can detect adds/removes.
- **Symlinks**: not followed when walking scope contents (avoids cycles and pulls of huge external trees). A symlinked `HARNESS.yml` is loaded but its target file's bytes are hashed via the link.
- **Filename normalisation (required for hash stability across macOS APFS/HFS+ and Linux)**:
  - Every relative path that feeds into a hash is first **Unicode-NFC normalised** (some macOS APIs return NFD for non-ASCII filenames; git stores NFC; without normalisation the same checkout produces different scope hashes on different surfaces).
  - On case-insensitive filesystems (default macOS APFS, HFS+), paths are additionally lowercased before hashing. The detection happens once per workspace by probing the FS; the result is cached in `manifest_index` so we cannot accidentally mix modes.
  - The text of `changed_files` returned to extensions is the *original* casing on disk, but the hash inputs are normalised. Extensions therefore see real paths but the snapshot model stays stable.

### 4.4.1 Orphan-submission GC

`harness validate` runs an opportunistic GC over `.harness/evidence/` at the start of every run, before the resolve fan-out:

- For each `<task_id>/submission-NNN/`: if `evidence.json` is missing, treat the dir as an aborted submission (Ctrl-C between asset copy and ledger commit) and delete it.
- For each submission that has an `evidence.json` but no row in `evidence_records`, the same applies — the ledger transaction never committed. Delete.
- Submissions with a matching ledger row are kept as history even after their scope_hash goes stale; they are evidence of past acceptance and are cheap to retain.

The GC is best-effort; failures are logged at warn level and never abort the run.

### 4.5.1 Cross-process locking & first-run bootstrap

Two `harness` invocations on the same workspace must not corrupt the ledger or persist inconsistent task rows. SQLite's WAL prevents bytes-level corruption but not logical races (two `validate`s computing different `tasks` rows simultaneously, or `evidence` racing a `validate` truncate).

The bootstrap sequence for any state-writing command, in order:

1. **Ensure `.harness/` exists.** `fs::create_dir_all(".harness")`. If it exists but is not writable → exit 2 with the scenario-21 message. Concurrent first runs both `mkdir -p` idempotently; no race.
2. **Open the lock file** with `OpenOptions::new().create(true).write(true).open(".harness/.lock")`. The file is *always* created if missing; multiple callers calling `create(true)` simultaneously is safe (POSIX guarantees one inode). Do **not** use `create_new` — that would turn a benign existing file into a hard error.
3. **Acquire the lock** with `fs2`'s `FileExt::try_lock_exclusive` on the opened handle. On `WouldBlock`, exit 2 with `"another harness process is running — retry shortly"`. We do not block-wait in v1; the agent loop can retry trivially.
4. **Open and migrate `state.sqlite`.** Migrations are idempotent (§7.1 test).
5. Do the work. Drop the handle on exit — `fs2` releases the lock; the lock file itself persists, which is fine.

- `validate` holds the lock from step 3 through "commit replaced tasks table".
- `evidence` holds the lock from step 3 through "ledger commit + write `evidence.json`".
- Read-only paths (`--help`, `--version`, future `harness list-tasks`) skip the entire bootstrap.

### 4.6 Extension IPC contract (concrete)

- **Command form**: the descriptor's `resolve.command` / `evidence.command` accepts **either** an argv array (preferred):

  ```yaml
  resolve:
    command: ["bin/bazel-extension", "resolve", "build"]
  ```

  …or a string parsed with `shell-words` rules (POSIX-ish), not naive whitespace split. Shell metacharacters (`|`, `;`, `&`, `<`, `>`, `$`, backticks) are rejected in the string form to keep the contract free of any implicit shell layer. Both forms are validated at descriptor-load time.
- **Working directory**: workspace root, regardless of where the user invoked `harness`. Relative paths inside the descriptor (binaries, schemas) resolve against the descriptor's directory.
- **stdin**: exactly one JSON object matching `ResolveRequest` or `EvidenceValidationRequest` from `harness-protocol`. EOF terminates the request. **Core must close (drop) the child's stdin handle immediately after writing the request bytes**, before reading stdout — otherwise extensions that parse with `serde_json::from_reader(stdin())` deadlock waiting for EOF while core deadlocks waiting for output. `extension::runtime` has a dedicated unit test for this to prevent silent regressions.
- **stdout**: exactly one JSON object matching the response type. Trailing newline allowed; any garbage **before** or **after** the JSON document is a protocol error. Extensions that need to log must write to stderr.
- **Max response size**: 4 MiB. Responses larger than this are rejected with a clear error pointing at the extension. (Evidence-validation responses can carry diagnostics, but 4 MiB is generous and prevents pathological extensions from blowing core memory.)
- **stderr**: free-form; captured and surfaced verbatim on non-zero exit or on protocol error.
- **Timeout**: 30 s default, configurable via env `HARNESS_EXTENSION_TIMEOUT_SECS`. Timeout = treat as failure; the child is killed and stderr surfaced.
- **Protocol version**: every request carries `protocol_version: 1`. Responses without it, or with a higher major version, are rejected. v2 reserves the right to break the shape.
- **`changed_files` and `dirty_scopes` semantics**:
  - On the **first run** for a workspace (no rows in `file_fingerprints` / `scope_snapshots`), or for any check whose current scope_hash has no matching ledger row, every file inside the check's effective scope is listed in `changed_files`, and the check's `scope_path` is listed in `dirty_scopes`. The contract is "no prior fingerprint = treat all in-scope files as changed."
  - On subsequent runs, `changed_files` lists exactly the files whose `content_hash` differs from the previously stored one for any dirty scope; `dirty_scopes` lists the `scope_path`s whose `scope_hash` no longer matches a ledger row.
  - The lists are **deduplicated** and **sorted** (lexicographic, post-normalisation — see filename-normalisation note below).

### 4.7 SQLite schema

```sql
CREATE TABLE schema_version (version INTEGER NOT NULL);

CREATE TABLE manifest_index (
  manifest_path TEXT PRIMARY KEY,        -- relative to workspace_root
  scope_path    TEXT NOT NULL,
  mtime_ns      INTEGER NOT NULL,
  size_bytes    INTEGER NOT NULL,
  content_hash  TEXT NOT NULL,
  last_seen_at  INTEGER NOT NULL
);

CREATE TABLE file_fingerprints (
  rel_path     TEXT PRIMARY KEY,
  mtime_ns     INTEGER NOT NULL,
  size_bytes   INTEGER NOT NULL,
  content_hash TEXT NOT NULL,
  last_seen_at INTEGER NOT NULL
);

CREATE TABLE scope_snapshots (
  scope_path  TEXT PRIMARY KEY,
  scope_hash  TEXT NOT NULL,
  computed_at INTEGER NOT NULL
);

CREATE TABLE tasks (
  task_id             TEXT PRIMARY KEY,
  capability          TEXT NOT NULL,
  title               TEXT NOT NULL,
  satisfies_json      TEXT NOT NULL,     -- JSON array of check IDs
  scope_hashes        TEXT NOT NULL,     -- JSON map check_id → scope_hash
  task_snapshot_hash  TEXT NOT NULL,     -- blake3 of sorted scope hashes (stale-detection key)
  payload_json        TEXT NOT NULL,     -- full task body for re-render
  created_at          INTEGER NOT NULL
);

-- Per-validate-run diagnostic notes from resolve responses (spec §9.4).
-- Repopulated every run alongside tasks; rendered as a "Notes:" footer
-- in the text report and a "notes" array in --json.
CREATE TABLE resolve_notes (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  capability  TEXT NOT NULL,
  note        TEXT NOT NULL,
  created_at  INTEGER NOT NULL
);

CREATE TABLE evidence_records (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id        TEXT NOT NULL,
  submission_id  TEXT NOT NULL,
  check_id       TEXT NOT NULL,
  scope_hash     TEXT NOT NULL,
  accepted       INTEGER NOT NULL,        -- 0/1
  summary        TEXT,
  submission_json TEXT NOT NULL,
  result_json    TEXT NOT NULL,
  submitted_at   INTEGER NOT NULL,
  UNIQUE(task_id, submission_id, check_id)
);

CREATE INDEX evidence_records_check_idx
  ON evidence_records(check_id, scope_hash, accepted);
```

A check is "green" iff `SELECT 1 FROM evidence_records WHERE check_id=? AND scope_hash=? AND accepted=1 LIMIT 1` returns a row. Stale evidence is not deleted — it stays as history. We just no longer find a matching `scope_hash` row.

---

## 5. CLI surface

```
harness validate [--json] [--log <level>]
harness evidence <task-id> --text <str> [--asset <path>]... [--log <level>]
harness --version
harness --help
```

- `--json` on `validate`: emit the same data as the text report but in a stable JSON shape. The shape is **frozen at first ship** and `insta`-snapshotted; future fields are added with care. Exit codes are unchanged under `--json` (0 = clean, 1 = pending). The contract:

  ```json
  {
    "protocol_version": 1,
    "status": "clean" | "pending",
    "workspace_root": "/abs/path",
    "tasks": [
      {
        "id": "bazel-build-login",
        "capability": "bazel/build",
        "title": "Build Login module",
        "satisfies": ["App/Login/login-build"],
        "parallelizable": true,
        "instructions": ["…"],
        "evidence_contract": { "text": { "required": true }, "assets": [ { "kind": "log", "required": true } ] }
      }
    ],
    "ignored_checks": [
      { "id": "root/app-build", "capability": "bazel/build", "reason": "A deeper bazel/build target covers the changed scope." }
    ],
    "notes": [
      { "capability": "bazel/build", "note": "bazel/build selected the deepest applicable target." }
    ]
  }
  ```

  The per-task fields mirror spec §9.4's `ResolveResponse.tasks[]` verbatim — `--json` is "the merged resolve responses with status + workspace_root added", not a new shape. When clean, `tasks` and `ignored_checks` are empty arrays (never absent); `notes` may be empty.
- **Exit codes** (designed so `harness validate && commit` is the natural agent idiom):
  - `validate`: **0 iff the report is clean**; **1 if any validation task is pending**; 2 on configuration errors (bad manifest, missing extension, schema-invalid `with` payload); 70 on internal errors.
  - `evidence`: 0 if accepted; 1 if rejected by the extension; 2 if the task is unknown or the snapshot is stale; 70 on internal errors.
- All errors go to stderr; reports go to stdout.
- **Workspace root resolution** (in order):
  1. `--workspace <path>` flag, when provided, is canonicalised (symlinks resolved) and used verbatim.
  2. Else: canonicalise `cwd`, then walk upward to the nearest ancestor containing a **`.git` directory** (not a `.git` *file*); that directory is the workspace root, regardless of where the `HARNESS.yml` files live below it. A `.git` *file* (the gitlink that marks a submodule worktree) is treated as **transparent** — we keep walking up so submodules resolve to the outer repo. If the user genuinely wants the submodule as a workspace, they pass `--workspace`.
  3. Else (no git repo): walk upward from canonicalised `cwd` to the nearest ancestor containing a `HARNESS.yml`. **Stop at the first one — do not climb across that boundary.** This prevents us from accidentally selecting `/Users/<name>/` as a workspace just because someone left a stray YAML file there.
  4. If still not found, exit 2 with a clear error suggesting `--workspace`.
- The `workspace_root` passed to extensions in the resolve/evidence requests is always the canonical absolute path.
- **Canonicalisation failures**: `std::fs::canonicalize` returns `Err` for broken symlinks, missing components, or unreadable ancestors. We translate any such error into exit 2 with the message `"could not canonicalise workspace path: <error>; pass --workspace <path>"` — never a raw `Os` error. The implementation tries `canonicalize` first; on failure with `--workspace` provided it falls back to a *logical* (non-resolved) absolute path with a `warn!` log, so users with intentionally non-canonical workspace paths can still proceed.

Deferred commands (documented but not implemented in MVP): `harness init`, `harness doctor`, `harness list-tasks`, `harness ledger`.

---

## 6. Reference extensions

Both extensions are Rust binaries built in the same workspace under `extensions/`. They depend on `harness-protocol` for IPC types. They are wired up by an `extension.yml` template that the E2E test harness materialises into a tmp workspace's `.harness/extensions/<name>/extension.yml`.

### 6.1 `bazel/build`

- Schema: `{ "target": string }`.
- Resolve policy:
  1. Bucket checks by `scope_path`.
  2. For each dirty scope, pick the deepest applicable check whose `scope_path` is an ancestor or equal.
  3. If several deep checks fire and share a common ancestor target that *also* has a check, prefer the ancestor when ≥ 3 siblings would otherwise run individually. (Threshold configurable later.)
  4. Emit `ignored_checks` with reasons for everything we did not pick.
- Task instructions: `"Run \`bazel build <target>\`. Capture stdout/stderr as a log asset."`.
- Evidence contract: text required; one asset of kind `log` required (we don't try to enforce log format yet — just non-empty and < 50 MiB).

### 6.2 `mav/expect`

- Schema: `{ "expectations": string[], "evidence": (screenshot|video|mav-report|accessibility-tree|logs)[] }`.
- Resolve policy:
  1. Bucket checks by `scope_path`.
  2. Within each bucket, merge all expectations into one task and union the requested evidence kinds.
  3. Emit one task per bucket. (Cross-bucket merging is future work — each scope may map to a different feature.)
- Task instructions enumerate the merged expectations and required evidence kinds.
- Evidence contract: text required; one asset per declared evidence kind; basic MIME validation per kind (e.g. screenshots must be `image/*`, mav-report must be `application/json` and parse).

Both extensions can be replaced by user-authored ones; nothing about the core depends on them.

---

## 7. Test plan

Three layers. Each test layer is required to be green at every checkpoint.

### 7.1 Unit tests (per crate, `#[test]`)

- `manifest::parser`: valid + invalid YAML; duplicate local IDs **inside the same manifest** rejected; unknown `version` rejected; `with` is captured opaquely.
- `manifest::ids`: stable global IDs match snapshots; root scope spelled `root/<local>`; same `local_id` in two manifests produces two distinct global IDs.
- `manifest::with_validation`: a JSON Schema violation reports the manifest path and the JSON pointer to the offending field; reports as a manifest error (not an extension failure).
- `snapshot::fingerprint`: mtime/size cache reused; cache busted on size change.
- `snapshot::scope`: hash determinism; changing a file flips the hash; reordering files in fs walk does not (sorted internally); adding a descendant manifest changes the parent's scope hash; editing a file under a deeper same-capability manifest does **not** change the parent check's effective scope hash (the carve-out works).
- `state::db`: migrations idempotent; round-trip of every table; ledger transaction is atomic (crash midway leaves no rows).
- `extension::descriptor`: schema validation; missing fields rejected; relative paths resolved against descriptor dir; both `command` forms (array and string) work; shell metacharacters in the string form are rejected.
- `extension::runtime`: timeout fires; non-zero exit surfaces stderr; oversized (>4 MiB) response rejected; non-JSON stdout rejected; multiple concatenated JSON documents rejected; `protocol_version` mismatch rejected; **stdin is closed before reading stdout so a `from_reader`-style extension does not deadlock**.
- `evidence::store`: asset copy preserves bytes; submission numbering monotonic.
- `evidence::ledger`: "is green?" query honours scope_hash; partial accept (extension returns subset of `satisfies`) green-marks only the listed checks.
- `harness-protocol`: serde round-trip on every public type.

### 7.2 Integration tests (per crate `tests/`)

- `harness-core::tests::validate_with_stub_extension`: an in-process stub registered through a dummy descriptor produces a deterministic resolve response; the orchestrator returns the expected tasks and renders the expected report.
- `harness-core::tests::evidence_accept_reject`: stub extension accepts on second submission; ledger reflects it; later `validate` call returns clean.
- `harness-core::tests::stale_snapshot_rejection`: modify a file between resolve and evidence; the evidence call returns the §12.5 stale message.
- `harness-core::tests::multi_scope_task_ledger`: a single stub-returned task lists `satisfies` from two different manifest scopes; on accept, two ledger rows are written, each keyed by the **declaring manifest's** scope_hash.
- `harness-core::tests::partial_accept_keeps_unlisted_pending`: stub task `satisfies: [a, b]`, evidence response `accepted: true, satisfies: [a]`; ledger has one row; next `validate` still emits a task for `b`.
- `harness-core::tests::workspace_root_resolution`: `.git` anchor wins over deeper `HARNESS.yml`; no-git fallback finds the nearest `HARNESS.yml` and stops there; missing both → exit 2.
- `harness-core::tests::overclaim_rejected`: stub task `satisfies: [a]`, evidence response `accepted: true, satisfies: [a, b]` where `b` is not in the task; whole submission rejected with a message naming `b`.
- `harness-core::tests::tasks_table_replaced_on_revalidate`: persisted task ids that the resolver no longer emits are gone after the next `validate`; `harness evidence <old_id>` returns the "task no longer applies" error.

These use a stub extension provided as a tiny test binary built from a `tests/fixtures/stub_extension` crate so we exercise the actual IPC path.

### 7.2.1 Stub extension modes

The stub needs to cover the failure matrix that the static reference extensions can't reproduce. It reads its behaviour from env vars set by each test:

| Env var | Values | Purpose |
|---|---|---|
| `HARNESS_STUB_RESOLVE_SHAPE` | `default`, `empty`, `multi_task`, `ignore_all` | Shape of the resolve response when the resolve call succeeds. |
| `HARNESS_STUB_RESOLVE_MODE` | `ok`, `timeout`, `garbage`, `oversized`, `nonzero_exit`, `bad_protocol_version` | Failure-injection knob for resolve calls. `ok` means "respect HARNESS_STUB_RESOLVE_SHAPE". |
| `HARNESS_STUB_EVIDENCE_SHAPE` | `accept_all`, `accept_subset`, `reject`, `overclaim` | Shape of the evidence-validation response when the call succeeds. `accept_subset` returns `satisfies` shorter than the task's set; `overclaim` returns an id not in the task. |
| `HARNESS_STUB_EVIDENCE_MODE` | `ok`, `timeout`, `garbage`, `oversized`, `nonzero_exit`, `bad_protocol_version` | Failure-injection knob for evidence calls. |
| `HARNESS_STUB_DELAY_MS` | integer | Sleep before responding (drives timeout tests). |
| `HARNESS_STUB_RESPONSE_BYTES` | integer | Pad the response with junk to test the 4 MiB cap. |

Phase 3 only exercises the `RESOLVE_*` knobs (evidence command doesn't exist yet). Phase 4 layers in the `EVIDENCE_*` knobs. The scenario→phase mapping in §9 splits accordingly.

The stub's source is part of the workspace so it is rebuilt with every `cargo test`. Each scenario sets the variables it needs and runs the harness binary with `assert_cmd`.

### 7.3 E2E tests (workspace `tests/e2e/`)

Run the real `harness` binary on a temp workspace using `assert_cmd`. Each scenario is one file; output is snapshot-tested with `insta` (snapshots reviewed by hand). Real Rust extensions are built with `cargo build --bin bazel-extension --bin mav-extension` before the test suite.

Scenarios (mirror §19 success criterion and beyond):

1. **`clean_repo_clean_report`** — no changes since last accepted evidence → "Harness validation clean." (exit 0)
2. **`first_run_emits_tasks`** — fresh repo with the §15 manifests → two pending tasks (`bazel-build-login`, `mav-login-flow`). (exit 1)
3. **`evidence_loop`** — submit valid evidence for both tasks → next `validate` is clean.
4. **`modify_file_reopens_task`** — touch `App/Login/LoginView.swift` → previously-green checks reopen.
5. **`stale_evidence_rejected`** — submit evidence; mutate a file before the next call; submit again with stale snapshot → rejection with the §12.5 message. (exit 2)
6. **`bazel_picks_deepest_target`** — root + child build checks both apply; only the child task is emitted; root appears in `ignored_checks`. After child evidence is accepted, the next `validate` returns clean (no pending tasks): root has **no ledger row** so per §4.1 it is still "dirty" and is fanned out to `bazel/build` every run — `bazel/build` simply puts it in `ignored_checks` every time, which costs one cheap IPC call but produces zero pending tasks (and so a clean report). What the §4.5 carve-out actually buys is that root's `scope_hash` and `changed_files` list stay stable across runs that only touch files under `App/Login/`, so the extension's "ignored: deeper covers it" reason doesn't churn. A subsequent edit **outside** `App/Login/` (e.g. a top-level `README.md`) dirties root's `scope_hash`, bumps its `changed_files`, and `bazel/build` now emits a root build task. Both directions validate the §4.5 carve-out.
7. **`mav_groups_expectations`** — two `mav/expect` checks in the same scope produce one task with merged expectations.
8. **`bad_manifest_errors`** — invalid YAML, missing `version`, conflicting local IDs in one manifest, and a `with` payload that fails the extension's JSON Schema → exit 2 with a pinpointed manifest path + JSON pointer.
9. **`extension_failure`** — extension returns non-zero, times out, prints garbage on stdout, or returns >4 MiB → error mentions the extension and surfaces stderr; other capabilities still run; exit 2.
10. **`json_output`** — `--json` produces a parseable, stable shape; schema snapshot checked.
11. **`partial_accept`** — extension's evidence accept lists only one of the task's `satisfies` entries; the listed check goes green, the unlisted one remains pending and re-appears on the next `validate` until separately satisfied.
12. **`same_local_id_two_manifests`** — `root/login-build` and `App/Login/login-build` (same `local_id`, different scopes) appear as distinct rows in the report, the ledger keys them independently, and accepting evidence for one leaves the other pending.
13. **`unrelated_edit_does_not_stale`** — `validate` issues a task; the user edits a file in a scope that the task does **not** satisfy; the subsequent `evidence` call is accepted (only in-task-scope edits cause staleness — companion test to §4.2).
14. **`stale_task_id_rejected`** — `validate` issues task A; the user edits a file; `validate` re-runs and emits task B (with a different id) because the extension's resolve output changed; `evidence A` is rejected with "task no longer applies — run harness validate" (exit 2).
15. **`concurrent_validate_locks`** — two `harness validate` processes started concurrently: one acquires the lock, the other exits 2 with the "another harness process is running" message.
16. **`unicode_path_stability`** — a fixture with NFD-normalised non-ASCII filenames on macOS produces the same scope_hash as the NFC-normalised equivalent.
17. **`submodule_workspace_root`** — `cwd` inside a `.git`-file (gitlink) submodule resolves to the outer repo, not the submodule.
18. **`missing_extension_capability`** — a manifest declares `uses: bazel/build` but no installed extension implements that capability → exit 2 with the manifest path, the offending check id, and a `"no extension implements capability bazel/build"` message.
19. **`missing_extension_binary`** — descriptor present but `bin/<binary>` is missing or not executable → exit 2 naming the descriptor path and the resolved binary path.
20. **`empty_extensions_dir`** — `.harness/extensions/` exists but is empty; any manifest with checks fails the same way as scenario 18 (capability has no implementor) — one shared error message format.
21. **`readonly_state_dir`** — `.harness/` is read-only → exit 2 with `".harness/ is not writable; harness needs to create state.sqlite"` instead of a panic.
22. **`empty_workspace_no_manifests`** — `.git` exists, zero `HARNESS.yml` files → exit 0 with "Harness validation clean. No HARNESS.yml files found." Distinguishes "nothing to validate" from "everything passed."
23. **`broken_cwd_canonicalisation`** — `cwd` resolves through a broken symlink or non-existent path → exit 2 with `"could not canonicalise workspace path: <error>; pass --workspace <path>"`, not a raw OS error.

### 7.4 How to run

```bash
make test            # cargo test --workspace
make e2e             # cargo test --test '*e2e*' --release
make lint            # cargo fmt --check && cargo clippy -- -D warnings
make all             # lint + test + e2e
```

`make e2e` builds the reference extensions in release mode before invoking the suite; tests pass an absolute path to the binaries into the materialised descriptors.

### 7.5 Manual smoke walkthrough

A `fixtures/login-app/` directory mirrors §15. It is checked in and used by tests, but also documented in `README.md`:

```bash
cargo install --path crates/harness
cd fixtures/login-app
harness validate              # → two pending tasks
# follow the printed instructions, capture artefacts
harness evidence bazel-build-login --text "..." --asset build.log
harness evidence mav-login-flow   --text "..." --asset screen.png --asset run.mp4 --asset report.json
harness validate              # → clean
```

This is the human checkpoint at the end of every milestone.

---

## 8. Build & dev workflow

```bash
make build           # cargo build --workspace
make release         # cargo build --workspace --release
make test
make e2e
make lint
make install         # cargo install --path crates/harness
```

Toolchain pinned in `rust-toolchain.toml` to `1.81` (stable as of writing — bump on demand). `Cargo.lock` is committed because this repo ships a binary.

---

## 9. Phase plan and checkpoints

Each phase ends with a runnable demo + green tests for everything implemented so far, **followed by the per-phase review loop in §9.0**. Do not start the next phase until the current one exits the loop clean.

### 9.0 Per-phase review loop (mandatory)

After every phase's `✅` checkpoint is green, run this loop before moving on:

1. **Commit the phase's work** so the reviewer reads a stable snapshot.
2. **Spawn a fresh `general-purpose` subagent** (a new one each round — no shared context with prior reviewers) with this brief:
   - Read `docs/harness-validation-plan.md` (spec) and `docs/PLAN.md` (this plan).
   - Read the code landed in this phase only (the diff against the previous phase's tip, plus any files it materially depends on).
   - Hunt for **substantive** issues: spec/plan deviations, contract bugs, unhandled failure modes, missing tests for behaviour the phase claims to deliver, internal contradictions between code and PLAN.md. Skip nitpicks.
   - Return a numbered list (max 8) — each entry: one-line headline, 2–3 sentences of explanation citing file:line, a concrete fix. If clean, the reviewer responds exactly `No blocking issues found. Phase N is ready to land.`
3. **Apply the feedback** as one or more follow-up commits. If a fix changes the contract documented in PLAN.md, update PLAN.md in the same commit so docs and code never drift.
4. **Re-run the phase's tests** (`make test` + `make e2e` scoped to the phase's scenarios). If green, **spawn another fresh subagent** for round 2.
5. **Stop when a fresh subagent returns `No blocking issues found.`** That phase is done.

Rules of the loop:
- Always a **new** subagent per round; never reuse one mid-loop. Reuse causes the reviewer to defer to its earlier opinions.
- Cap at **5 review rounds per phase**. If a phase needs more, the implementation has diverged from PLAN.md — stop, update PLAN.md to reflect what was actually built (or rip it out and try again), and restart.
- The reviewer never writes code. It only finds issues. The main agent applies the fixes.
- A `No blocking issues found.` from one reviewer is enough — we do not require two consecutive clean rounds. The 5-round audit of PLAN.md itself was sufficient prior art that the bar "one fresh reviewer says it's clean" reliably catches the real bugs.

### Phase 0 — Workspace skeleton *(½ day)*
- Create the Cargo workspace, four crates (`harness-protocol`, `harness-extension-util`, `harness-core`, `harness`), two extension binaries with `main()` returning `Ok(())`, and the stub test extension binary under `tests/fixtures/stub_extension/`.
- `Makefile`, `rust-toolchain.toml`, `.gitignore`, `docs/architecture.md` placeholder.
- ✅ `cargo test --workspace` runs (no tests yet) and `cargo build --workspace` succeeds.
- 🔁 Run §9.0 review loop until clean before starting Phase 1.

### Phase 1 — Manifest model + state ground work *(2 days)*
- `harness-protocol` types defined and snapshot-serded.
- `manifest::discovery`, `manifest::parser`, `manifest::ids`.
- `state::db` with the schema in §4.7 and migrations.
- `snapshot::fingerprint`, `snapshot::scope`.
- `harness validate` walks a workspace and exits 0 without error when there are no manifests (a workspace with `.git` but no `HARNESS.yml` is trivially clean — printed as "Harness validation clean. No HARNESS.yml files found."). When manifests exist but the extension loader is not wired yet, fail with a clearly-labelled "Phase 1 only — extension loading lands in Phase 2" error to make the placeholder visible. The Phase 2 wiring then replaces this with the scenario-20 behaviour (exit 2, capability has no implementor) — no contradiction remains by end of Phase 2.
- ✅ Unit tests for every module + one integration test on a two-manifest fixture; one E2E test for the no-HARNESS.yml empty-workspace path.
- 🔁 Run §9.0 review loop until clean before starting Phase 2.

### Phase 2 — Extension loading + IPC *(2 days)*
- `extension::descriptor` loads `.harness/extensions/*/extension.yml`.
- `extension::runtime` spawns a child, exchanges JSON, enforces the timeout.
- Schema validation of `with` payloads against extension-declared schemas.
- Stub extension binary used by tests.
- ✅ Integration test: round-trip with stub extension; bad schema rejected; timeout exercised.
- 🔁 Run §9.0 review loop until clean before starting Phase 3.

### Phase 3 — `harness validate` report *(2 days)*
- `validate.rs` orchestrator: dirty-scope detection, fan-out to extensions, persist tasks.
- `report::text` renders the §11.2 output. `--json` produces a stable companion.
- Idempotent re-runs: no extra writes if nothing changed.
- Uses the **stub** extension for all tests in this phase (modes per §7.2.1).
- ✅ E2E scenarios passing with the stub at the resolve layer only: **2 (first_run_emits_tasks)**, **8 (bad_manifest_errors)**, **9a (resolve-side `extension_failure` — every `HARNESS_STUB_RESOLVE_MODE`)**, **10 (json_output)**, **15 (concurrent_validate_locks)**, **16 (unicode_path_stability)**, **17 (submodule_workspace_root)**. Insta snapshots checked in.
- 🔁 Run §9.0 review loop until clean before starting Phase 4.

### Phase 4 — `harness evidence` command + ledger semantics *(2 days)*
- `evidence::store` copies assets, allocates submission dirs.
- Calls extension `evidence` capability, persists ledger rows on accept (per the §4.2 partial-accept rule: extension's `satisfies` is authoritative).
- Stale-snapshot detection (per-task `task_snapshot_hash`) and rendered rejection.
- Re-running `validate` after a green ledger row is found returns clean.
- ✅ E2E scenarios that need the ledger but can still use the stub: **1 (clean_repo_clean_report)**, **3 (evidence_loop)**, **4 (modify_file_reopens_task)**, **5 (stale_evidence_rejected)**, **9b (evidence-side `extension_failure` — every `HARNESS_STUB_EVIDENCE_MODE`)**, **11 (partial_accept)**, **12 (same_local_id_two_manifests)**, **13 (unrelated_edit_does_not_stale)**, **14 (stale_task_id_rejected)**.
- 🔁 Run §9.0 review loop until clean before starting Phase 5.

### Phase 5 — `bazel/build` reference extension + shared util *(1.5 days)*
- Land `harness-extension-util` first (stdio framing helpers, asset-kind classification by MIME, response-size guard). Phase 6 reuses it; doing it now avoids a retroactive refactor.
- Implements §16.1 deepest-target policy. No transitive-satisfy in MVP — the §4.5 carve-out plus the convergence model in §4.5 keep the loop closed.
- Evidence validation: text + log asset, log non-empty.
- ✅ E2E scenario **6 (bazel_picks_deepest_target)** passes against the real binary; build half of the §15 worked example passes.
- 🔁 Run §9.0 review loop until clean before starting Phase 6.

### Phase 6 — `mav/expect` reference extension *(1 day)*
- Implements §16.2 grouping.
- Evidence validation: kind-by-kind checks (screenshots image/*, mav-report parseable JSON, …).
- ✅ E2E scenario **7 (mav_groups_expectations)** plus the full §15 worked example pass.
- 🔁 Run §9.0 review loop until clean before starting Phase 7.

### Phase 7 — Agent skill + docs *(½ day)*
- `docs/skill.md` based on spec §14.1 — copy-pasteable into Claude/Codex skill folders. **Must include the "record evidence for every task from the current `harness validate` output before re-running `harness validate`" rule**: re-running `validate` truncates the previous run's tasks table (§4.1), so any un-recorded task ids from that run will be rejected with "task no longer applies." This is the single most likely agent-loop bug; the skill addresses it explicitly.
- `docs/architecture.md` filled in.
- `docs/extensions.md` describes the JSON contract for third-party extension authors.
- ✅ The §19 success criterion runs end-to-end on `fixtures/login-app/`.
- 🔁 Run §9.0 review loop a final time over the MVP as a whole before declaring §11 (Definition of Done) met.

Total: ~10–12 working days of net implementation, plus 1–3 review-loop rounds per phase. Each phase is its own PR; the repo stays buildable and tested at the tip of every PR.

---

## 10. Risks & open questions

- **Concurrent invocations**: two agents running `validate` simultaneously may race the SQLite DB. SQLite WAL handles most of it; for the MVP we accept "best-effort" semantics and document it. A future `harness lock` advisory file can fix it cheaply.
- **Large assets**: copying a multi-GiB video into `.harness/evidence/` is wasteful. MVP behavior: copy with a warning over 100 MiB; future option to register by absolute path without copying.
- **`.gitignore` correctness**: if the workspace is not a git repo, the `ignore` crate's `.gitignore` rules are skipped and our built-in ignore list applies alone. That's fine, but worth documenting.
- **Extension trust**: extensions execute arbitrary code with the user's permissions. We document it; we do not sandbox. Aligns with §17.8.
- **Manifest discovery cost on huge monorepos**: full walk on every run, parallelised by the `ignore` crate. Profiling target: <500 ms on a 100k-file workspace with a warm OS cache. If we miss it, we add a watched-directory cache before Phase 4 ships.
- **Cross-platform**: macOS is the primary target. Linux should "just work" because we avoid Apple-specific APIs. Windows is best-effort — not part of the MVP success criterion.
- **Atomic ledger writes**: every accept call wraps `evidence_records` insert + scope-snapshot upsert in a single transaction so a crash mid-call never leaves a half-green check.
- **Extension protocol versioning**: every request carries `protocol_version: 1`. Extensions that respond without it or with a higher major version are rejected; reserve the right to bump in v2.
- **Convergence model (no transitive-satisfy in MVP)**: a check goes green only when an extension lists it in an evidence-accept's `satisfies` array. The agent's loop terminates because `validate` reports "no pending tasks", **not** because every applicable check has a ledger row. An extension that returns a check in `ignored_checks` is telling the agent "I have deliberately not emitted a task for this." The check stays without a ledger row and will be re-sent to the extension on the next `validate` (cheap), but it does not block completion. This deliberately leaves no "transitive-satisfy" mechanism in v1 — adding one is straightforward later if a real extension needs it.
- **Branch switching mid-flight**: a `git checkout` between `validate` and `evidence` invalidates scope hashes for the changed files, so `evidence` rejects as stale. This is the correct behaviour, but the error message should mention "branch may have changed" as a hint.

---

## 11. Definition of Done for the MVP

Reproduce §19 of the spec against `fixtures/login-app/` using only:

```bash
cargo install --path crates/harness
cd fixtures/login-app
harness validate
# … run instructions, record evidence …
harness validate            # clean
```

…with all E2E scenarios in §7.3 green in CI-equivalent local runs.

Once this is true, the loop the spec promises — *"the task is not done until `harness validate` is empty"* — is a thing the agent can actually run.
