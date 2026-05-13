# Writing a musts extension

A musts extension is **any executable** that speaks the JSON-over-stdio protocol described here and in [`PLAN.md`](PLAN.md) §4.6 + §9–§10 of the spec. The reference extensions in this repo are Rust binaries because they ride along the test suite, but the protocol is language-agnostic — a 30-line shell script is just as valid.

## TL;DR — a real extension in 30 lines of bash

The canonical "your first extension" is checked in at [`docs/examples/eslint-check/`](examples/eslint-check/) — a complete `eslint/check` implementation: `eslint-check.sh` (a small `bash` + `jq` script) plus the `extension.yml` next to it.

```yaml
# .musts/extensions/eslint/extension.yml
name: eslint
version: 0.1.0
capabilities:
  check:
    uses: eslint/check
    resolve:
      command: ["./eslint-check.sh", "resolve"]
    evidence:
      command: ["./eslint-check.sh", "evidence"]
```

The script:

```bash
#!/usr/bin/env bash
set -euo pipefail
mode="${1:-}"
request=$(cat)

case "$mode" in
  resolve)
    jq -n --argjson req "$request" '{
      protocol_version: 1,
      tasks: [{
        id: "eslint-root", extension: $req.capability,
        title: "Run eslint over changed files",
        satisfies: [$req.checks[].id],
        parallelizable: false,
        instructions: ["Run `npx eslint .`", "Submit a one-line summary + the full log."],
        evidence_contract: {
          text:   { required: true },
          assets: [{ kind: "log", required: true }]
        }
      }],
      ignored_checks: [], notes: []
    }';;
  evidence) … ;;   # see the file
esac
```

That's it. No Rust toolchain, no musts-only library, no extension protocol crate. It works because the protocol IS just JSON-in / JSON-out. A Python or Node version is equally short. The full file (with the evidence path written out) is `docs/examples/eslint-check/eslint-check.sh` and is exercised end to end by `crates/musts/tests/shell_extension_e2e.rs`.

## Core capabilities (no extension needed)

The core ships **one built-in capability** so a fresh workspace can use the validation loop with zero setup:

- **`agent`** — the agent itself verifies facts. Manifest:
  ```yaml
  checks:
    login-form-visual:
      uses: agent
      with:
        facts:
          - "Login form rejects empty email."
          - "Password is masked."
  ```
  No `evidence: [...]` field. Core supplies a text-required-assets-optional contract and accepts any evidence the agent attaches. See [`PLAN.md`](PLAN.md) §6.0 for the schema and policy.

When `uses: agent` is encountered, no `.musts/extensions/` directory is consulted. If you want to **override** the built-in (e.g. add MIME-specific evidence requirements), ship an extension whose `extension.yml` declares `uses: agent` — descriptor-backed extensions win over built-ins.

Everything else is an extension.

## Narrowing a check to specific files: `paths`

Every check (built-in or extension-backed) accepts an optional `paths` list of [`globset`](https://docs.rs/globset)-style patterns. When present, only files matching at least one pattern contribute to the check's effective scope hash, so unrelated edits leave the check green:

```yaml
checks:
  tracking-tests:
    uses: agent
    paths:
      - "**/Tracking*.swift"
      - "**/TrackingEvents/**"
    with:
      facts:
        - "TrackingEvents changes are covered by tests."

  integration-fixtures:
    uses: agent
    paths: "tests/integration/**/*.json"
    with:
      facts:
        - "The integration test JSON fixture is valid and intentional."
```

Semantics:

- `paths` accepts either a single string or a list of strings. Absent or empty means "no filter" — the legacy behaviour applies (all files under the manifest's folder, minus the same-capability carve-out).
- Patterns are matched against the workspace-relative path. `**/Tracking*.swift` matches at any depth; `tests/**` is rooted at the workspace.
- On case-insensitive filesystems (default macOS / Windows) matching is case-insensitive — mirrors the workspace's own behaviour so `**/Tracking*.swift` keeps matching `Tracking.swift` regardless of how the file is stored.
- A check whose `paths` currently matches **no** file is "not applicable" and is dropped from the task list. When a matching file is added later, the next `musts validate` picks it up automatically.
- An invalid glob is a manifest error (exit 2) — surfaced at parse time, with the check id and the offending pattern.

## What a workspace expects

```text
<workspace>/
├── MUSTS.yml                     # one or more manifests anywhere in the tree
└── .musts/
    └── extensions/
        └── <name>/
            ├── extension.yml       # this is what core loads
            ├── schemas/...         # optional JSON Schemas for `with` payloads
            └── …                   # binaries, scripts, schemas — anything `command` points at
```

A single `extension.yml` can declare multiple capabilities (e.g. `bazel.build` and `bazel.test`).

## `extension.yml`

```yaml
name: bazel
version: 0.1.0

capabilities:
  build:
    uses: bazel/build           # fully qualified capability id; manifests reference this
    schema: schemas/build.schema.json   # optional; validated as a manifest error if violated
    resolve:
      command: ["bin/bazel-extension", "resolve", "build"]   # preferred: argv array
    evidence:
      command: "bin/bazel-extension evidence build"          # OR: shell-words string
```

- `command` accepts either an **argv array** (always preferred) or a **string** parsed with POSIX-ish shell-words rules. The string form rejects `|`, `;`, `&`, `<`, `>`, `$`, and backticks at load time so the contract is free of any implicit shell layer.
- Relative paths (`bin/...`, `schemas/...`) resolve against the directory containing `extension.yml`.
- The descriptor `version` is informational. The wire protocol version is `1`.

## Protocol

For each call, core spawns the binary, writes one JSON document to stdin, closes stdin, and reads one JSON document from stdout.

### Hard rules

- **One JSON document per response.** Garbage before or after the document, or multiple concatenated objects, is a protocol error.
- **stdout is for the response only.** Use stderr for diagnostics — it is captured verbatim and surfaced in error messages.
- **Always flush stdout before exit.** Rust's `stdout()` is fully buffered when piped; `musts-extension-util::write_response` flushes for you, but if you roll your own you must explicitly flush.
- **Max response size: 4 MiB.** Larger responses are rejected.
- **Timeout: 30 s.** Configurable for the parent via `MUSTS_EXTENSION_TIMEOUT_SECS`.
- **`protocol_version: 1`** in every response. Responses with any other value are rejected.

### Resolve

`<binary> resolve <capability>` receives a [`ResolveRequest`](../crates/musts-protocol/src/lib.rs):

```json
{
  "protocol_version": 1,
  "workspace_root": "/abs/path",
  "capability": "bazel/build",
  "changed_files": ["App/Login/LoginView.swift", "…"],
  "checks": [
    {
      "id": "App/Login/login-build",
      "local_id": "login-build",
      "manifest_path": "App/Login/MUSTS.yml",
      "scope_path": "App/Login",
      "depth": 2,
      "with": { "target": "//App/Login:Login" }
    }
  ],
  "snapshot": { "handle": "opaque", "dirty_scopes": ["App/Login"] }
}
```

…and returns a `ResolveResponse`:

```json
{
  "protocol_version": 1,
  "tasks": [
    {
      "id": "bazel-build-app-login",
      "extension": "bazel/build",
      "title": "Build //App/Login:Login",
      "satisfies": ["App/Login/login-build", "root/app-build"],
      "parallelizable": true,
      "instructions": ["Run `bazel build //App/Login:Login`.", "…"],
      "evidence_contract": {
        "text": { "required": true, "description": "…" },
        "assets": [ { "kind": "log", "required": true } ]
      }
    }
  ],
  "ignored_checks": [
    { "id": "root/app-build", "reason": "subsumed by a deeper bazel/build target in the same run" }
  ],
  "notes": []
}
```

#### Conventions

- The `satisfies` array determines which checks the ledger marks green when evidence is accepted. **Subsume ancestor checks** by listing their ids here when one task makes them redundant — that's how the reference `bazel/build` extension converges the §15 worked example.
- Use `ignored_checks` for diagnostic visibility — they don't grant green status, but they tell the agent why a check that *could* have produced a task didn't.
- `notes` are rendered in the agent report under a "Notes:" footer; one per actionable diagnostic.
- A check that contains a malformed `with` (e.g. wrong type for `target`) belongs in `ignored_checks` with a clear reason. The orchestrator already rejects schema-invalid payloads as manifest errors before they reach you — but defensive resolvers are still encouraged.

### Evidence

`<binary> evidence <capability>` receives an [`EvidenceValidationRequest`](../crates/musts-protocol/src/lib.rs):

```json
{
  "protocol_version": 1,
  "workspace_root": "/abs/path",
  "task": {
    "id": "bazel-build-app-login",
    "extension": "bazel/build",
    "satisfies": ["App/Login/login-build", "root/app-build"],
    "evidence_contract": { … }
  },
  "submission": {
    "text": "bazel build //App/Login:Login succeeded",
    "assets": [
      { "path": ".musts/evidence/bazel-build-app-login/submission-001/build.log",
        "mime": "text/plain", "size": 4096 }
    ]
  },
  "snapshot": { "handle": "opaque", "dirty_scopes": [] }
}
```

…and returns an `EvidenceValidationResponse`. Two shapes:

**Accepted**

```json
{
  "protocol_version": 1,
  "accepted": true,
  "satisfies": ["App/Login/login-build", "root/app-build"],
  "summary": "Build evidence accepted (1 log asset).",
  "normalized_assets": [ { "kind": "log", "path": "…/build.log" } ]
}
```

**Rejected**

```json
{
  "protocol_version": 1,
  "accepted": false,
  "missing": [ { "kind": "log", "message": "Attach the bazel stdout/stderr…" } ],
  "message": "Evidence is incomplete."
}
```

#### Rules

- The `satisfies` array in an accept is **authoritative**. Core writes one ledger row per listed check, keyed by that check's declaring-manifest scope_hash.
- Any id in your accepted `satisfies` that wasn't in the request's `task.satisfies` is an **over-claim**: the core rejects the whole submission with exit 2 and the rejected ids in the error message. Don't return checks that weren't issued.
- Partial accept (returning a subset of `task.satisfies`) is legal and intentional. Unlisted ids remain pending and will be re-emitted on the next `validate`.
- Read evidence assets via `workspace_root.join(asset.path)` — `asset.path` is workspace-relative.
- Asset paths exist on disk before your binary is invoked; core copies them into `.musts/evidence/<task>/submission-NNN/` first. You can read, hash, parse, or inspect them however you like.
- The `evidence.json` marker file is written by core **after** your accept returns and the ledger commit succeeds. You never write it.

## If you prefer Rust

The shell-script flow above is the default recommendation. If you would rather write the extension in Rust (e.g. because it needs to do non-trivial parsing, or you want compile-time enforcement of the wire shapes), the workspace ships a `musts-extension-util` crate. A complete extension is roughly 30 lines plus your business logic:

```rust
use musts_extension_util::ipc_main;
use musts_protocol::{ResolveRequest, ResolveResponse, EvidenceValidationRequest, EvidenceValidationResponse};

fn main() -> std::process::ExitCode {
    ipc_main(resolve, evidence)
}

fn resolve(req: ResolveRequest) -> Result<ResolveResponse, String> { /* … */ }
fn evidence(req: EvidenceValidationRequest) -> Result<EvidenceValidationResponse, String> { /* … */ }
```

The helper closes stdin before reading stdout (so your `serde_json::from_reader` won't deadlock), flushes stdout on response, surfaces any returned `String` error on stderr, and exits with code 2 on failure. Read [`crates/musts-extension-util/src/lib.rs`](../crates/musts-extension-util/src/lib.rs).

Three Rust reference extensions are shipped:

- [`extensions/bazel-build`](../extensions/bazel-build/) — `bazel/build`, demonstrating the deepest-target subsumption policy across nested scopes.
- [`extensions/mav-expect`](../extensions/mav-expect/) — `mav/expect`, demonstrating MIME-driven asset classification (screenshots, videos, JSON reports).
- [`extensions/cargo`](../extensions/cargo/) — `cargo/{fmt,clippy,test}`, demonstrating capability-dispatched log-content heuristics (the same binary serves three capabilities). The repo uses it to validate itself; see the "Self-validation" section of the top-level README.

## Failure-injection matrix (stub-extension)

The test stub at [`tests/fixtures/stub_extension/`](../tests/fixtures/stub_extension/) implements every PLAN.md §7.2.1 mode and is a useful template for testing core changes that need a misbehaving extension. The env-var matrix:

- `MUSTS_STUB_RESOLVE_SHAPE`, `MUSTS_STUB_RESOLVE_MODE`
- `MUSTS_STUB_EVIDENCE_SHAPE`, `MUSTS_STUB_EVIDENCE_MODE`
- `MUSTS_STUB_DELAY_MS`, `MUSTS_STUB_RESPONSE_BYTES`

Set these from your tests via `std::env::set_var` + `#[serial_test::serial]`.

## Schema validation

The `schema` field in `extension.yml` is loaded eagerly and cached. Every `with` payload in every manifest is validated against the schema **before** any `resolve` call. Failures surface as **manifest errors** (exit 2) with the manifest path and a JSON pointer to the offending field — your resolver never sees an invalid `with`. Defensive type-checking in `resolve` is still good practice, but the orchestrator filters the truly malformed inputs out for you.
