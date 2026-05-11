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
Recompute the task_snapshot_hash from the current scope_hashes of every
satisfied check; compare with the stored task_snapshot_hash. Mismatch =
stale → render §12.5 message and exit 2.
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
  - For each check it lists, write one evidence_records row keyed by
    that check's **declaring-manifest scope_hash** (NOT the task's
    aggregate hash). Unlisted-but-claimed satisfies remain pending so
    the next `validate` still emits a task for them. Wrap the inserts
    and the scope_snapshot upserts in a single SQLite transaction so a
    crash never half-greens a multi-check task.
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
| `evidence::store` | Allocate `submission-NNN` directory, copy + checksum assets, write `evidence.json`. | `EvidenceStore` |
| `evidence::ledger` | Read/write `evidence_records`, expose "is this check green for this scope hash?" | `Ledger` |
| `report::text` | Convert tasks + ignored checks into the §11.2 output. | `Report` |
| `validate` | Top-level orchestrator. | `Validator` |

### 4.4 Stable IDs

- **Check ID (global)**: `<scope_path>/<local_id>`. The root manifest's scope path is literally `root` (not `.`), so a root-declared `app-build` is `root/app-build`. The format is the single source of truth across the report, ledger, and IPC; the report always prints the global ID, never just the local one. Two manifests with the same `local_id` produce distinct globals (`root/login-build` vs `App/Login/login-build`) and are valid; the report and ledger keep them apart.
- **Conflicts**: rejected at parse time only inside the same manifest. Cross-manifest collisions are impossible by construction because the scope path is part of the ID.
- **Task ID**: extension-provided. If two extensions return the same task id in one resolve cycle, the core prefixes them with the capability (`bazel/build:bazel-build-login`) and logs a warning.
- **Submission ID**: `submission-001`, `-002`, … allocated per task by counting existing dirs.

### 4.5 Snapshot model (concrete decisions)

- **Hash function**: blake3, 256-bit, hex-encoded. Reason: ~10× faster than SHA-256 on Apple Silicon; large repos hit IO long before CPU.
- **Per-check effective scope (important)**: a check's scope hash is computed over the files in its declaring manifest's folder **minus the files under any deeper manifest that declares a check of the same capability**. Without this carve-out a root `bazel/build` check would re-fire on every edit anywhere in the repo and the loop would never converge. With it, editing a file inside `App/Login/` (which has its own `bazel/build` check) does not dirty `root/app-build`; editing a top-level `README.md` does.
- **File granularity**: every file under the effective scope, minus the ignore list. Ignored: `.git/`, `.harness/evidence/`, `.harness/cache/`, `node_modules/`, `target/`, `bazel-bin*`, `bazel-out*`, `bazel-testlogs*`, `DerivedData/`, `*.xcodeproj/xcuserdata/`. Also obeys `.gitignore` via the `ignore` crate so derived files outside Git but inside the workspace still count.
- **Scope hash**: `blake3(sorted_join(rel_path || "\0" || file_hash) || "\0" || manifest_hash || "\0" || ext_descriptor_hash || "\0" || sorted_join(descendant_manifest_rel_path))`. Including descendant manifest *paths* (not contents — those are hashed for their own scopes) means adding/removing a child manifest invalidates the parent's idea of "what's applicable to me." Including the extension descriptor hash means swapping or upgrading an extension invalidates evidence. Both are explicit and intentional.
- **Per-check scope_hash provenance**: a check is always hashed against its **declaring manifest's** scope, never against the task that ends up satisfying it. A task that satisfies checks from two different manifests records two distinct ledger rows, each keyed by the corresponding declaring-manifest scope hash.
- **Manifest hash**: blake3 of the file bytes.
- **Cheap path**: on second+ runs, if a file's mtime+size match the cached fingerprint, reuse the stored hash. Only recompute hashes when (mtime OR size) changes.
- **Discovery invalidation**: we do **not** rely on directory mtimes to detect new/removed manifests — APFS and some git operations leave them stale. Every run does a parallel `ignore::WalkBuilder` traversal of the workspace, which is fast enough on warm caches (low ms for >100k files) and is the only correctness-safe option. The cached `manifest_index` provides the previous state so we can detect adds/removes.
- **Symlinks**: not followed when walking scope contents (avoids cycles and pulls of huge external trees). A symlinked `HARNESS.yml` is loaded but its target file's bytes are hashed via the link.

### 4.6 Extension IPC contract (concrete)

- **Command form**: the descriptor's `resolve.command` / `evidence.command` accepts **either** an argv array (preferred):

  ```yaml
  resolve:
    command: ["bin/bazel-extension", "resolve", "build"]
  ```

  …or a string parsed with `shell-words` rules (POSIX-ish), not naive whitespace split. Shell metacharacters (`|`, `;`, `&`, `<`, `>`, `$`, backticks) are rejected in the string form to keep the contract free of any implicit shell layer. Both forms are validated at descriptor-load time.
- **Working directory**: workspace root, regardless of where the user invoked `harness`. Relative paths inside the descriptor (binaries, schemas) resolve against the descriptor's directory.
- **stdin**: exactly one JSON object matching `ResolveRequest` or `EvidenceValidationRequest` from `harness-protocol`. EOF terminates the request.
- **stdout**: exactly one JSON object matching the response type. Trailing newline allowed; any garbage **before** or **after** the JSON document is a protocol error. Extensions that need to log must write to stderr.
- **Max response size**: 4 MiB. Responses larger than this are rejected with a clear error pointing at the extension. (Evidence-validation responses can carry diagnostics, but 4 MiB is generous and prevents pathological extensions from blowing core memory.)
- **stderr**: free-form; captured and surfaced verbatim on non-zero exit or on protocol error.
- **Timeout**: 30 s default, configurable via env `HARNESS_EXTENSION_TIMEOUT_SECS`. Timeout = treat as failure; the child is killed and stderr surfaced.
- **Protocol version**: every request carries `protocol_version: 1`. Responses without it, or with a higher major version, are rejected. v2 reserves the right to break the shape.

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
- **Exit codes** (designed so `harness validate && commit` is the natural agent idiom):
  - `validate`: **0 iff the report is clean**; **1 if any validation task is pending**; 2 on configuration errors (bad manifest, missing extension, schema-invalid `with` payload); 70 on internal errors.
  - `evidence`: 0 if accepted; 1 if rejected by the extension; 2 if the task is unknown or the snapshot is stale; 70 on internal errors.
- All errors go to stderr; reports go to stdout.
- **Workspace root resolution** (in order):
  1. `--workspace <path>` flag, when provided, is used verbatim.
  2. Else: walk upward from `cwd` to the nearest ancestor containing a `.git` directory; that directory is the workspace root, regardless of where the `HARNESS.yml` files live below it.
  3. Else (no git repo): walk upward from `cwd` to the nearest ancestor containing a `HARNESS.yml`. **Stop at the first one — do not climb across that boundary.** This prevents us from accidentally selecting `/Users/<name>/` as a workspace just because someone left a stray YAML file there.
  4. If still not found, exit 2 with a clear error suggesting `--workspace`.

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
- `extension::runtime`: timeout fires; non-zero exit surfaces stderr; oversized (>4 MiB) response rejected; non-JSON stdout rejected; multiple concatenated JSON documents rejected; `protocol_version` mismatch rejected.
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

These use a stub extension provided as a tiny test binary built from a `tests/fixtures/stub_extension` crate so we exercise the actual IPC path.

### 7.3 E2E tests (workspace `tests/e2e/`)

Run the real `harness` binary on a temp workspace using `assert_cmd`. Each scenario is one file; output is snapshot-tested with `insta` (snapshots reviewed by hand). Real Rust extensions are built with `cargo build --bin bazel-extension --bin mav-extension` before the test suite.

Scenarios (mirror §19 success criterion and beyond):

1. **`clean_repo_clean_report`** — no changes since last accepted evidence → "Harness validation clean." (exit 0)
2. **`first_run_emits_tasks`** — fresh repo with the §15 manifests → two pending tasks (`bazel-build-login`, `mav-login-flow`). (exit 1)
3. **`evidence_loop`** — submit valid evidence for both tasks → next `validate` is clean.
4. **`modify_file_reopens_task`** — touch `App/Login/LoginView.swift` → previously-green checks reopen.
5. **`stale_evidence_rejected`** — submit evidence; mutate a file before the next call; submit again with stale snapshot → rejection with the §12.5 message. (exit 2)
6. **`bazel_picks_deepest_target`** — root + child build checks both apply; only the child task is emitted; root appears in `ignored_checks`. After child evidence is accepted, root remains pending **only** until the next `validate`, where `bazel/build` transitively marks it satisfied via the evidence-accept `satisfies` list. Then a subsequent unrelated edit *outside* `App/Login/` correctly re-opens root **without** re-opening the child (validates the effective-scope carve-out in §4.5).
7. **`mav_groups_expectations`** — two `mav/expect` checks in the same scope produce one task with merged expectations.
8. **`bad_manifest_errors`** — invalid YAML, missing `version`, conflicting local IDs in one manifest, and a `with` payload that fails the extension's JSON Schema → exit 2 with a pinpointed manifest path + JSON pointer.
9. **`extension_failure`** — extension returns non-zero, times out, prints garbage on stdout, or returns >4 MiB → error mentions the extension and surfaces stderr; other capabilities still run; exit 2.
10. **`json_output`** — `--json` produces a parseable, stable shape; schema snapshot checked.
11. **`partial_accept`** — extension's evidence accept lists only one of the task's `satisfies` entries; the listed check goes green, the unlisted one remains pending and re-appears on the next `validate` until separately satisfied.
12. **`same_local_id_two_manifests`** — `root/login-build` and `App/Login/login-build` (same `local_id`, different scopes) appear as distinct rows in the report, the ledger keys them independently, and accepting evidence for one leaves the other pending.

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
- Uses the **stub** extension for all tests in this phase.
- ✅ E2E scenarios that pass with the stub: **2 (first_run_emits_tasks)**, **8 (bad_manifest_errors)**, **9 (extension_failure)**, **10 (json_output)**. Insta snapshots checked in.

### Phase 4 — `harness evidence` command + ledger semantics *(2 days)*
- `evidence::store` copies assets, allocates submission dirs.
- Calls extension `evidence` capability, persists ledger rows on accept (per the §4.2 partial-accept rule: extension's `satisfies` is authoritative).
- Stale-snapshot detection (per-task `task_snapshot_hash`) and rendered rejection.
- Re-running `validate` after a green ledger row is found returns clean.
- ✅ E2E scenarios that need the ledger but can still use the stub: **1 (clean_repo_clean_report)**, **3 (evidence_loop)**, **4 (modify_file_reopens_task)**, **5 (stale_evidence_rejected)**, **11 (partial_accept)**, **12 (same_local_id_two_manifests)**.

### Phase 5 — `bazel/build` reference extension *(1 day)*
- Implements §16.1 deepest-target policy.
- Evidence validation: text + log asset, log non-empty.
- Transitive `satisfies`: when a deep target's evidence is accepted, the response may also list an ancestor `bazel/build` check as satisfied, so the noisy-root convergence path from §4.5 actually closes.
- ✅ E2E scenario **6 (bazel_picks_deepest_target)** passes against the real binary; build half of the §15 worked example passes.

### Phase 6 — `mav/expect` reference extension *(1 day)*
- Implements §16.2 grouping.
- Evidence validation: kind-by-kind checks (screenshots image/*, mav-report parseable JSON, …).
- ✅ E2E scenario **7 (mav_groups_expectations)** plus the full §15 worked example pass.

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
- **Manifest discovery cost on huge monorepos**: full walk on every run, parallelised by the `ignore` crate. Profiling target: <500 ms on a 100k-file workspace with a warm OS cache. If we miss it, we add a watched-directory cache before Phase 4 ships.
- **Cross-platform**: macOS is the primary target. Linux should "just work" because we avoid Apple-specific APIs. Windows is best-effort — not part of the MVP success criterion.
- **Atomic ledger writes**: every accept call wraps `evidence_records` insert + scope-snapshot upsert in a single transaction so a crash mid-call never leaves a half-green check.
- **Extension protocol versioning**: every request carries `protocol_version: 1`. Extensions that respond without it or with a higher major version are rejected; reserve the right to bump in v2.
- **Transitive-satisfy contract**: the only way a check goes green is the extension listing it in an evidence-accept's `satisfies` array. We rely on `bazel/build`'s evidence handler to transitively mark ancestor build checks satisfied when a deep target's build was accepted, otherwise the loop won't converge on root-with-deeper-children layouts. This is an extension contract, not a core feature — but core's docs need to spell it out so future extension authors don't get it wrong.
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
