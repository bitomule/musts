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
Persist returned tasks (state.tasks). Render report.
```

`harness validate` is read-mostly + writes the new task list. It never writes ledger rows.

### 4.2 Pipeline for `harness evidence`

```text
Look up the task by id
        │
        ▼
Verify the workspace snapshot still matches the task's snapshot
    (fail with the "stale" message from §12.5 if not)
        │
        ▼
Copy assets into .harness/evidence/<task_id>/submission-NNN/
Compute mime, size, file hash
        │
        ▼
Build submission JSON, call extension `evidence` capability
        │
        ▼
On accept: write evidence_records row + green-mark each `satisfies` check
            against the current scope_hash
On reject: print the rejection message to stderr, exit non-zero
```

### 4.3 Module responsibilities

| Module | Responsibility | Key types |
|---|---|---|
| `manifest::discovery` | Walk the workspace once, find every `HARNESS.yml`, watch for new/removed ones on subsequent runs using dir mtimes. | `ManifestPath`, `ManifestTree` |
| `manifest::parser` | Validate `version: 1`, load checks, validate `with` against the relevant extension schema (deferred until extension loading). | `Manifest`, `Check`, `CheckId` |
| `manifest::ids` | Build globally stable check IDs: `<scope_path>/<local_id>` (root scope uses `root/`). | `CheckId` |
| `snapshot::fingerprint` | mtime+size cache, lazy blake3 rehash. | `FileFingerprint` |
| `snapshot::scope` | Compute scope hashes; encapsulate the ignored-directories list. | `ScopeHash` |
| `state::db` | Open `.harness/state.sqlite`, apply migrations, expose typed CRUD. | `Db` |
| `state::tables` | Definitions for `manifest_index`, `file_fingerprints`, `scope_snapshots`, `tasks`, `evidence_records`. | — |
| `extension::descriptor` | Parse `.harness/extensions/*/extension.yml`. | `ExtensionDescriptor`, `Capability` |
| `extension::runtime` | Spawn child process, write request JSON, read response JSON, enforce timeout, surface stderr on failure. | `ExtensionRunner` |
| `evidence::store` | Allocate `submission-NNN` directory, copy + checksum assets, write `evidence.json`. | `EvidenceStore` |
| `evidence::ledger` | Read/write `evidence_records`, expose "is this check green for this scope hash?" | `Ledger` |
| `report::text` | Convert tasks + ignored checks into the §11.2 output. | `Report` |
| `validate` | Top-level orchestrator. | `Validator` |

### 4.4 Stable IDs

- Check ID: `<scope_path>/<local_id>`. Root scope is `root`. Conflicts inside the same manifest are rejected at parse time.
- Task ID: extension-provided. Core namespaces collisions by prepending the capability when two extensions return the same id (`bazel/build:bazel-build-login`). Logged as a warning.
- Submission ID: `submission-001`, `-002`, … allocated per task by counting existing dirs.

### 4.5 Snapshot model (concrete decisions)

- **Hash function**: blake3, 256-bit, hex-encoded. Reason: ~10× faster than SHA-256 on Apple Silicon; large repos hit IO long before CPU.
- **File granularity**: every file under the manifest scope, minus the ignore list. Ignored: `.git/`, `.harness/evidence/`, `.harness/cache/`, `node_modules/`, `target/`, `bazel-bin*`, `bazel-out*`, `bazel-testlogs*`, `DerivedData/`, `*.xcodeproj/xcuserdata/`. Also obeys `.gitignore` via the `ignore` crate so derived files outside Git but inside the workspace still count.
- **Scope hash**: `blake3(sorted_join(rel_path || "\0" || file_hash) || "\0" || manifest_hash || "\0" || ext_descriptor_hash)`. Including extension descriptor hashes means changing an extension's behaviour invalidates evidence — explicit and intentional.
- **Manifest hash**: blake3 of the file bytes.
- **Cheap path**: on second+ runs, if a file's mtime+size match the cached fingerprint, reuse the stored hash. Only recompute hashes when (mtime OR size) changes.

### 4.6 Extension IPC contract (concrete)

- Binary called as: `<descriptor.command>` (split on whitespace by the descriptor). The capability and method are baked into the command line in the descriptor (e.g. `bin/bazel-extension resolve build`).
- stdin: a single JSON object matching `ResolveRequest` or `EvidenceValidationRequest` from `harness-protocol`. EOF on the request.
- stdout: a single JSON object matching the response type. Trailing newline allowed. Any non-JSON output triggers a rendered error.
- stderr: free-form; captured and surfaced on non-zero exit.
- Timeout: 30 s default, configurable via env `HARNESS_EXTENSION_TIMEOUT_SECS`. Timeout = treat as failure.
- Working directory: workspace root, regardless of where the user invoked `harness`.

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
  task_id        TEXT PRIMARY KEY,
  capability     TEXT NOT NULL,
  title          TEXT NOT NULL,
  satisfies_json TEXT NOT NULL,          -- JSON array of check IDs
  scope_hashes   TEXT NOT NULL,          -- JSON map check_id → scope_hash
  payload_json   TEXT NOT NULL,          -- full task body for re-render
  created_at     INTEGER NOT NULL
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

- `--json` on `validate`: emit the same data as the text report but in a stable JSON shape. Allows future tooling (Claude hook, etc.) to consume it. Implementing this in Phase 3 alongside the text renderer costs little.
- Exit codes:
  - `validate`: 0 if the report renders (even with pending tasks). 2 on configuration errors (bad manifest, missing extension). 70 on internal errors.
  - `evidence`: 0 if accepted. 1 if rejected by the extension. 2 if the task is unknown or the snapshot is stale. 70 on internal errors.
- All errors go to stderr; reports go to stdout.
- Working directory: harness walks upward from `cwd` to find the nearest workspace root (defined as the directory containing a `HARNESS.yml`; ambiguity resolved by stopping at the topmost `HARNESS.yml`).

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

- `manifest::parser`: valid + invalid YAML; duplicate local IDs rejected; unknown `version` rejected; `with` is captured opaquely.
- `manifest::ids`: stable IDs match snapshots; root scope spelled `root/<local>`.
- `snapshot::fingerprint`: mtime/size cache reused; cache busted on size change.
- `snapshot::scope`: hash determinism; changing a file flips the hash; reordering files in fs walk does not (sorted internally).
- `state::db`: migrations idempotent; round-trip of every table.
- `extension::descriptor`: schema validation; missing fields rejected; relative paths resolved against descriptor dir.
- `extension::runtime`: timeout fires; non-zero exit surfaces stderr; oversized response rejected.
- `evidence::store`: asset copy preserves bytes; submission numbering monotonic.
- `evidence::ledger`: "is green?" query honours scope_hash.
- `harness-protocol`: serde round-trip on every public type.

### 7.2 Integration tests (per crate `tests/`)

- `harness-core::tests::validate_with_stub_extension`: an in-process stub registered through a dummy descriptor produces a deterministic resolve response; the orchestrator returns the expected tasks and renders the expected report.
- `harness-core::tests::evidence_accept_reject`: stub extension accepts on second submission; ledger reflects it; later `validate` call returns clean.
- `harness-core::tests::stale_snapshot_rejection`: modify a file between resolve and evidence; the evidence call returns the §12.5 stale message.

These use a stub extension provided as a tiny test binary built from a `tests/fixtures/stub_extension` crate so we exercise the actual IPC path.

### 7.3 E2E tests (workspace `tests/e2e/`)

Run the real `harness` binary on a temp workspace using `assert_cmd`. Each scenario is one file; output is snapshot-tested with `insta` (snapshots reviewed by hand). Real Rust extensions are built with `cargo build --bin bazel-extension --bin mav-extension` before the test suite.

Scenarios (mirror §19 success criterion and beyond):

1. **`clean_repo_clean_report`** — no changes since last accepted evidence → "Harness validation clean."
2. **`first_run_emits_tasks`** — fresh repo with the §15 manifests → two pending tasks (`bazel-build-login`, `mav-login-flow`).
3. **`evidence_loop`** — submit valid evidence for both tasks → next `validate` is clean.
4. **`modify_file_reopens_task`** — touch `App/Login/LoginView.swift` → previously-green checks reopen.
5. **`stale_evidence_rejected`** — submit evidence; mutate a file before the next call; submit again with stale snapshot → rejection with the §12.5 message.
6. **`bazel_picks_deepest_target`** — root + child build checks both apply; only the child task is emitted; root appears in `ignored_checks`.
7. **`mav_groups_expectations`** — two `mav/expect` checks in the same scope produce one task with merged expectations.
8. **`bad_manifest_errors`** — invalid YAML, missing `version`, conflicting IDs → exit 2 with a pinpointed error.
9. **`extension_failure`** — extension returns non-zero → error mentions the extension and surfaces stderr; other capabilities still run.
10. **`json_output`** — `--json` produces a parseable, stable shape; schema snapshot checked.

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

Each phase ends with a runnable demo + green tests for everything implemented so far.

### Phase 0 — Workspace skeleton *(½ day)*
- Create the Cargo workspace, three crates, two extension binaries with `main()` returning `Ok(())`.
- `Makefile`, `rust-toolchain.toml`, `.gitignore`, `docs/architecture.md` placeholder.
- ✅ `cargo test --workspace` runs (no tests yet) and `cargo build --workspace` succeeds.

### Phase 1 — Manifest model + state ground work *(2 days)*
- `harness-protocol` types defined and snapshot-serded.
- `manifest::discovery`, `manifest::parser`, `manifest::ids`.
- `state::db` with the schema in §4.7 and migrations.
- `snapshot::fingerprint`, `snapshot::scope`.
- `harness validate` walks a workspace and prints "No extensions configured." for every applicable check it found, just to prove discovery works.
- ✅ Unit tests for every module + one integration test on a two-manifest fixture.

### Phase 2 — Extension loading + IPC *(2 days)*
- `extension::descriptor` loads `.harness/extensions/*/extension.yml`.
- `extension::runtime` spawns a child, exchanges JSON, enforces the timeout.
- Schema validation of `with` payloads against extension-declared schemas.
- Stub extension binary used by tests.
- ✅ Integration test: round-trip with stub extension; bad schema rejected; timeout exercised.

### Phase 3 — `harness validate` report *(2 days)*
- `validate.rs` orchestrator: dirty-scope detection, fan-out to extensions, persist tasks.
- `report::text` renders the §11.2 output. `--json` produces a stable companion.
- Idempotent re-runs: no extra writes if nothing changed.
- ✅ E2E scenarios 1, 2, 6, 8, 9, 10 pass. Insta snapshots checked in.

### Phase 4 — `harness evidence` command *(2 days)*
- `evidence::store` copies assets, allocates submission dirs.
- Calls extension `evidence` capability, persists ledger rows on accept.
- Stale-snapshot detection and rendered rejection.
- ✅ E2E scenarios 3, 4, 5 pass.

### Phase 5 — `bazel/build` reference extension *(1 day)*
- Implements §16.1 deepest-target policy.
- Evidence validation: text + log asset, log non-empty.
- ✅ E2E scenario "build only" from §15 passes against the real binary.

### Phase 6 — `mav/expect` reference extension *(1 day)*
- Implements §16.2 grouping.
- Evidence validation: kind-by-kind checks (screenshots image/*, mav-report parseable JSON, …).
- ✅ Scenario 7 plus the full §15 worked example pass.

### Phase 7 — Agent skill + docs *(½ day)*
- `docs/skill.md` based on §14.1 — copy-pasteable into Claude/Codex skill folders.
- `docs/architecture.md` filled in.
- `docs/extensions.md` describes the JSON contract for third-party extension authors.
- ✅ The §19 success criterion runs end-to-end on `fixtures/login-app/`.

Total: ~10–12 working days for a single contributor. Each phase is its own PR; the repo stays buildable and tested at the tip of every PR.

---

## 10. Risks & open questions

- **Concurrent invocations**: two agents running `validate` simultaneously may race the SQLite DB. SQLite WAL handles most of it; for the MVP we accept "best-effort" semantics and document it. A future `harness lock` advisory file can fix it cheaply.
- **Large assets**: copying a multi-GiB video into `.harness/evidence/` is wasteful. MVP behavior: copy with a warning over 100 MiB; future option to register by absolute path without copying.
- **`.gitignore` correctness**: if the workspace is not a git repo, the `ignore` crate's `.gitignore` rules are skipped and our built-in ignore list applies alone. That's fine, but worth documenting.
- **Extension trust**: extensions execute arbitrary code with the user's permissions. We document it; we do not sandbox. Aligns with §17.8.
- **Manifest discovery cost on huge monorepos**: full walk on first run is `O(files)`. Subsequent runs use the manifest_index and only re-walk directories whose dir-mtime changed. We accept the first-run cost.
- **Cross-platform**: macOS is the primary target. Linux should "just work" because we avoid Apple-specific APIs. Windows is best-effort — not part of the MVP success criterion.
- **Atomic ledger writes**: every accept call wraps `evidence_records` insert + scope-snapshot upsert in a single transaction so a crash mid-call never leaves a half-green check.
- **Extension protocol versioning**: every request carries `protocol_version: 1`. Extensions that respond without it or with a higher major version are rejected; reserve the right to bump in v2.

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
