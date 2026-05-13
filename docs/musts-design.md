# Musts Validation Manifest

Implementation specification, v0.2.

This document describes a scoped, extensible, agent-first validation system. It is intended to be used as the working design document for building the tool.

The tool tells an agent what must be validated after a change, how to produce evidence, and when the work is allowed to be called done.

---

## 1. One-Line Design

Put local validation manifests next to code. When files change, `musts validate` deterministically reports the validation tasks that are pending. The agent executes those tasks, records text and assets with `musts evidence`, and repeats until no validation tasks remain.

The completion rule:

> The agent is not done until `musts validate` returns an empty task list for the current workspace state.

The core idea is deliberately narrow:

- Repository manifests declare validation checks.
- Extensions turn those checks into agent-executable validation tasks.
- Agents execute those tasks using tools, commands, MAV, subagents, or manual interaction.
- Agents submit evidence through the CLI.
- Extensions decide whether the submitted evidence satisfies the task.
- The CLI records accepted evidence against the current content snapshot.

This is not a test runner. It is not CI. It is not another `CLAUDE.md`. It is the missing validation loop between agent work and trustworthy completion.

---

## 2. Decisions We Have Made

### 2.1 Scope

The MVP is validation-only.

It does not:

- distribute skills
- configure MCP servers
- manage Claude hooks
- manage Codex configuration
- replace `AGENTS.md` or `CLAUDE.md`
- model all musts context
- manage semantic facts
- implement a full musts graph

It only answers:

> Given the current workspace state, what validation tasks are still required before the agent can call the task done?

### 2.2 Agent-First CLI

`musts validate` is a report, not a runner.

It does not execute:

- builds
- tests
- MAV flows
- Playwright checks
- Docker Compose
- shell commands

It tells the agent what must be done.

The agent executes the tasks.

### 2.3 Evidence Command

The agent records task results with:

```bash
musts evidence <task-id> --text "<summary>" --asset <path> --asset <path>
```

The CLI:

1. receives text and asset paths
2. copies or registers assets into its evidence store
3. calls the relevant extension to validate the evidence
4. records accepted evidence in the ledger
5. rejects incomplete or invalid evidence with actionable feedback

### 2.4 No User-Exposed Policies

Users do not configure resolution strategies such as:

- `child_first`
- `parent_first`
- `group`
- `fallback`
- `subsumes`
- `prefer_smallest_build`
- `escalate_to_root`

Those policies are internal to each extension.

For example:

- `bazel/build` decides whether a child target replaces a root target.
- `mav/expect` decides whether multiple expectations can be grouped into one MAV session.
- `playwright/check` decides whether multiple page checks can share a browser run.

The manifest stays simple. Extensions own the domain logic.

### 2.5 Extensions Return JSON

The CLI talks to extensions through JSON.

Extensions return structured task data. The CLI renders that structured data as an agent-readable report.

This keeps the protocol deterministic while keeping the CLI output useful for agents.

### 2.6 YAML Manifests

Repository manifests use YAML, not Markdown.

Markdown is good for human context but too loose for deterministic parsing and extension schemas.

The manifest should be boring, structured, and parseable.

### 2.7 Stable IDs Are Required

Every check has a stable ID.

IDs are needed for:

- task generation
- evidence recording
- ledger entries
- debugging
- status reports
- extension responses
- explaining which checks were satisfied

### 2.8 Snapshot-Based Validity

The tool must not rely on `git diff` as the source of truth.

It must catch:

- manual edits
- edits made by Claude
- edits made by Codex
- Xcode saves
- generated files
- scripts
- branch changes
- file changes outside Git

Therefore, evidence validity is based on content fingerprints and internal snapshots.

`git diff` can be an optimization or a hint. It is not the correctness model.

### 2.9 Hooks Are Future Work

Claude hooks may later enforce:

- run `musts validate` before completion
- block "done" if validation tasks remain
- inject validation context after edits

But hooks are not part of the MVP.

The tool must first work through:

- CLI
- agent skill
- evidence ledger

---

## 3. Non-Goals

### 3.1 Not a CI Replacement

CI validates commits, branches, and releases.

This tool validates agent completion before an agent reports the task as done.

The mental model is:

```text
CI validates repository integration.
Musts validation validates agent completion.
```

### 3.2 Not Another Facts System

Facts capture semantic truths:

```text
Login rejects invalid email text.
Discounts apply in deterministic priority order.
```

Musts validation captures obligations:

```text
If this area changed, produce evidence that the relevant checks still hold.
```

Facts and this tool can complement each other, but the MVP should not include facts.

### 3.3 Not a Full Harness Graph

The MVP does not model:

- ownership
- risk
- dependencies between truths
- product requirements
- architectural decisions
- review policies
- semantic scope
- permission boundaries

Those may come later, but they are not necessary to prove the validation loop.

### 3.4 Not a Prompt Framework

Extensions may generate instructions for agents, but the tool is not a general prompt engineering system.

Its prompts are task-specific validation instructions.

---

## 4. Core Concepts

### 4.1 `MUSTS.yml`

A scoped validation manifest living in the repository.

By default, a manifest applies to the folder it is in and its descendants.

Example:

```text
repo/
  MUSTS.yml
  App/
    MUSTS.yml
    Login/
      MUSTS.yml
      LoginView.swift
```

If `App/Login/LoginView.swift` changes, the applicable manifests are:

```text
repo/MUSTS.yml
repo/App/MUSTS.yml
repo/App/Login/MUSTS.yml
```

The core collects checks from all applicable manifests and gives them to the relevant extensions.

### 4.2 Check

A check is a declared validation obligation.

Checks live in `MUSTS.yml`.

Each check has:

- a stable ID
- a `uses` value pointing to an extension capability
- a `with` payload owned by the extension

Example:

```yaml
checks:
  login-flow:
    uses: mav/expect
    with:
      expectations:
        - Login works with multiple valid emails.
        - Invalid email text shows an error when used as email.
      evidence:
        - screenshot
        - video
        - mav-report
```

The core does not understand the `with` payload. The extension does.

### 4.3 Extension

An extension is a plugin that knows how to resolve checks of a specific type into validation tasks and how to validate submitted evidence.

Examples:

- `bazel/build`
- `bazel/test`
- `mav/expect`
- `playwright/check`
- `docker/compose-health`
- `npm/test`

The extension owns:

- schema for its `with` payload
- resolution behavior
- grouping behavior
- task instructions
- evidence contract
- evidence validation

### 4.4 Task

A task is the final validation instruction returned by an extension.

A task can satisfy one or more checks.

Example:

```text
Task: mav-login-flow
Satisfies:
  - App/Login/login-flow
  - App/Login/invalid-email-flow
```

This lets extensions group multiple checks into a single agent task.

### 4.5 Evidence

Evidence is submitted by the agent through the CLI.

Evidence consists of:

- text
- assets

Assets may include:

- logs
- screenshots
- videos
- JSON reports
- MAV reports
- accessibility trees
- traces
- test output

The CLI stores evidence. The extension decides whether it satisfies the task.

### 4.6 Snapshot

A snapshot is an internal content fingerprint representing the relevant workspace state.

Evidence is accepted against a snapshot. If relevant files change, the evidence becomes stale.

Agents do not pass snapshot IDs around. Snapshot handling is internal to the CLI.

### 4.7 Ledger

The ledger is the internal record of evidence accepted for checks against snapshots.

Conceptually:

```text
check X is satisfied by evidence Y for snapshot Z
```

If snapshot Z changes, check X becomes pending again.

---

## 5. Repository Manifest Files

### 5.1 File Layout

Recommended initial convention:

```text
repo/
  MUSTS.yml
  App/
    MUSTS.yml
    Login/
      MUSTS.yml
      LoginView.swift
      LoginViewModel.swift
    Checkout/
      MUSTS.yml
      DiscountEngine.swift
```

The default scope of a manifest is its folder recursively.

### 5.2 Minimal Manifest Schema

```yaml
version: 1

checks:
  <check-id>:
    uses: <extension-name>/<capability-name>
    with:
      # Extension-owned payload.
    paths:
      # Optional. Single string or list of gitignore-style globs.
      # When present, only files matching at least one pattern
      # contribute to the check's effective scope; a check that
      # matches no files is treated as not-applicable and is
      # dropped from the task list. See `docs/extensions.md` →
      # "Narrowing a check to specific files: `paths`".
      - "**/Tracking*.swift"
```

### 5.3 Root Manifest Example

The root can define a broad build check.

The user does not mark it as fallback. The `bazel/build` extension decides whether it should be used or ignored when deeper build checks are available.

```yaml
version: 1

checks:
  app-build:
    uses: bazel/build
    with:
      target: //App:App
```

### 5.4 Login Manifest Example

```yaml
version: 1

checks:
  login-build:
    uses: bazel/build
    with:
      target: //App/Login:Login

  login-flow:
    uses: mav/expect
    with:
      expectations:
        - Login works with multiple valid emails.
        - Invalid email text shows an error when used as email.
      evidence:
        - screenshot
        - video
        - mav-report
```

### 5.5 Workspace `.mustsignore`

A workspace-level `.mustsignore` (and any nested `.mustsignore` files in
subdirectories) excludes matched files from the walker that builds each
check's scope hash. Syntax and semantics match `.gitignore` exactly,
including negation with `!pattern` and the rule that a child can't be
re-included once its parent directory is excluded.

Precedence — applied in this order during the walk:

1. Built-in ignores (`.git/`, `.musts/`, `node_modules/`, `target/`,
   `DerivedData/`, `xcuserdata/`, `bazel-*`).
2. `.gitignore` (and `.git/info/exclude`) — honoured even outside a git
   repository.
3. `.mustsignore` — file is excluded from the walk → does not enter
   `compute_scope_file_inputs` → does not contribute to `scope_hash` →
   edits to it never re-invalidate dependent checks.
4. Per-check `paths:` filter (narrows an already-walked scope to the
   matching subset).

`.mustsignore` is committed to the repo. Divergent files across clones
produce different `scope_hash` values for the same code and break lock
portability.

### 5.6 Future Scope Fields

Not required for MVP, but likely useful later:

```yaml
version: 1

scope:
  include:
    - "**/*.swift"
  exclude:
    - "**/Generated/**"

checks:
  login-flow:
    uses: mav/expect
    with:
      expectations:
        - Login works with valid credentials.
      evidence:
        - screenshot
        - video
```

Initial MVP should avoid explicit scope fields unless necessary.

---

## 6. Extension Installation Layout

Extensions should be repo-local for reproducibility.

Recommended layout:

```text
.musts/
  extensions/
    bazel/
      extension.yml
      schemas/
        build.schema.json
        test.schema.json
      bin/
        bazel-extension
    mav/
      extension.yml
      schemas/
        expect.schema.json
      bin/
        mav-extension
```

The CLI loads `.musts/extensions/*/extension.yml`.

Global extensions can come later.

---

## 7. Extension YAML

### 7.1 Bazel Extension Descriptor

Example: `.musts/extensions/bazel/extension.yml`

```yaml
name: bazel
version: 0.1.0

capabilities:
  build:
    uses: bazel/build
    schema: schemas/build.schema.json
    resolve:
      command: bin/bazel-extension resolve build
    evidence:
      command: bin/bazel-extension evidence build

  test:
    uses: bazel/test
    schema: schemas/test.schema.json
    resolve:
      command: bin/bazel-extension resolve test
    evidence:
      command: bin/bazel-extension evidence test
```

### 7.2 MAV Extension Descriptor

Example: `.musts/extensions/mav/extension.yml`

```yaml
name: mav
version: 0.1.0

capabilities:
  expect:
    uses: mav/expect
    schema: schemas/expect.schema.json
    resolve:
      command: bin/mav-extension resolve expect
    evidence:
      command: bin/mav-extension evidence expect
```

### 7.3 Descriptor Fields

| Field | Meaning |
|---|---|
| `name` | Extension name. |
| `version` | Extension version. |
| `capabilities` | Named capabilities implemented by the extension. |
| `uses` | Fully qualified capability reference used in manifests. |
| `schema` | JSON Schema for validating the manifest `with` payload. |
| `resolve.command` | Command called by the core to turn checks into tasks. |
| `evidence.command` | Command called by the core to validate submitted evidence. |

### 7.4 Extension Runtime

MVP recommendation:

- Extension commands are arbitrary executables.
- The core passes JSON through stdin.
- The extension returns JSON through stdout.
- Non-zero exit means extension failure.

This keeps the system language-agnostic.

---

## 8. Extension Schemas

### 8.1 `bazel/build` Schema

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["target"],
  "properties": {
    "target": {
      "type": "string"
    }
  },
  "additionalProperties": false
}
```

### 8.2 `bazel/test` Schema

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["targets"],
  "properties": {
    "targets": {
      "type": "array",
      "items": { "type": "string" },
      "minItems": 1
    }
  },
  "additionalProperties": false
}
```

### 8.3 `mav/expect` Schema

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["expectations", "evidence"],
  "properties": {
    "expectations": {
      "type": "array",
      "items": { "type": "string" },
      "minItems": 1
    },
    "evidence": {
      "type": "array",
      "items": {
        "enum": [
          "screenshot",
          "video",
          "mav-report",
          "accessibility-tree",
          "logs"
        ]
      },
      "minItems": 1
    }
  },
  "additionalProperties": false
}
```

---

## 9. Extension Resolve Contract

The core never knows how Bazel, MAV, Playwright, Docker, or another validator should behave.

The core sends all applicable checks of a capability to that capability's resolver.

The resolver returns final validation tasks.

### 9.1 Resolve Request

Example request sent to `bazel/build`:

```json
{
  "protocol_version": 1,
  "workspace_root": "/repo",
  "capability": "bazel/build",
  "changed_files": [
    "App/Login/LoginView.swift"
  ],
  "checks": [
    {
      "id": "root/app-build",
      "local_id": "app-build",
      "manifest_path": "MUSTS.yml",
      "scope_path": ".",
      "depth": 0,
      "with": {
        "target": "//App:App"
      }
    },
    {
      "id": "App/Login/login-build",
      "local_id": "login-build",
      "manifest_path": "App/Login/MUSTS.yml",
      "scope_path": "App/Login",
      "depth": 2,
      "with": {
        "target": "//App/Login:Login"
      }
    }
  ],
  "snapshot": {
    "handle": "opaque-core-handle",
    "dirty_scopes": [
      "App/Login"
    ]
  }
}
```

### 9.2 Resolve Request Fields

| Field | Meaning |
|---|---|
| `protocol_version` | Contract version. |
| `workspace_root` | Absolute repo root. |
| `capability` | Capability being resolved, such as `bazel/build`. |
| `changed_files` | Files considered changed/dirty by the snapshot system. |
| `checks` | Applicable checks for this capability. |
| `checks[].id` | Globally stable check ID generated by core. |
| `checks[].local_id` | ID from the manifest. |
| `checks[].manifest_path` | Manifest that declared the check. |
| `checks[].scope_path` | Scope folder of the manifest. |
| `checks[].depth` | Folder depth, where root is 0. |
| `checks[].with` | Extension-owned check configuration. |
| `snapshot.handle` | Opaque snapshot handle. Extensions should not rely on its structure. |
| `snapshot.dirty_scopes` | Scopes that need validation for this run. |

### 9.3 Why the Extension Gets Depth and Scope

This is how policies stay internal.

The core passes:

- root check
- parent check
- child check
- scope paths
- depth
- changed files

Then the extension decides.

For example, `bazel/build` can internally implement:

```text
If a deeper target exists for the changed scope, prefer it.
If multiple sibling targets are affected, group them.
If too many targets are affected, choose a nearest common parent target.
If no child target exists, use the broadest applicable parent target.
```

The user does not configure these policies.

### 9.4 Resolve Response

Example response:

```json
{
  "tasks": [
    {
      "id": "bazel-build-login",
      "extension": "bazel/build",
      "title": "Build Login module",
      "satisfies": [
        "App/Login/login-build"
      ],
      "parallelizable": true,
      "instructions": [
        "Run `bazel build //App/Login:Login`.",
        "Capture stdout/stderr as a log asset.",
        "Record the result with `musts evidence bazel-build-login`."
      ],
      "evidence_contract": {
        "text": {
          "required": true,
          "description": "State the command that was run and whether it succeeded."
        },
        "assets": [
          {
            "kind": "log",
            "required": true,
            "description": "Build stdout/stderr log."
          }
        ]
      }
    }
  ],
  "ignored_checks": [
    {
      "id": "root/app-build",
      "reason": "A deeper bazel/build target covers the changed scope."
    }
  ],
  "notes": [
    "bazel/build selected the deepest applicable target."
  ]
}
```

### 9.5 Resolve Response Fields

| Field | Meaning |
|---|---|
| `tasks` | Final validation tasks to show to the agent. |
| `tasks[].id` | Stable task ID for this validation run. |
| `tasks[].extension` | Capability that owns the task. |
| `tasks[].title` | Short human/agent readable title. |
| `tasks[].satisfies` | Checks satisfied if this task's evidence is accepted. |
| `tasks[].parallelizable` | Whether the task can run in parallel with other tasks. |
| `tasks[].instructions` | Agent-facing instructions. |
| `tasks[].evidence_contract` | What evidence must be submitted. |
| `ignored_checks` | Checks intentionally not turned into tasks. |
| `notes` | Optional diagnostic notes. |

### 9.6 Task IDs

Task IDs should be stable enough for the agent to use during a loop.

For MVP, extension-generated task IDs are acceptable if the core namespaces them internally.

Example:

```text
bazel-build-login
mav-login-flow
```

If a task ID collides, the core should namespace or reject with an extension error.

---

## 10. Evidence Contract

Evidence recording is the second half of the extension protocol.

The CLI accepts generic evidence:

- text
- asset paths

The extension decides whether that evidence satisfies the task.

### 10.1 Evidence Command

```bash
musts evidence <task-id> \
  --text "<freeform summary>" \
  --asset <path> \
  --asset <path>
```

### 10.2 Design Rationale

The agent should not hand-craft a complex JSON bundle.

The agent should do simple things:

```bash
musts evidence mav-login-flow \
  --text "Validated valid and invalid email login behavior." \
  --asset /tmp/login-success.png \
  --asset /tmp/login-run.mp4 \
  --asset /tmp/mav-report.json
```

The CLI:

1. infers MIME types
2. records file size and metadata
3. copies assets into `.musts/evidence`
4. builds the structured submission object
5. calls the extension

### 10.3 Evidence Validation Request

Example request sent to `mav/expect`:

```json
{
  "protocol_version": 1,
  "workspace_root": "/repo",
  "task": {
    "id": "mav-login-flow",
    "extension": "mav/expect",
    "satisfies": [
      "App/Login/login-flow"
    ],
    "evidence_contract": {
      "text": {
        "required": true
      },
      "assets": [
        {
          "kind": "screenshot",
          "required": true
        },
        {
          "kind": "video",
          "required": true
        },
        {
          "kind": "mav-report",
          "required": true
        }
      ]
    }
  },
  "submission": {
    "text": "Validated two valid email logins and one invalid email error path.",
    "assets": [
      {
        "path": ".musts/evidence/mav-login-flow/submission-001/success.png",
        "mime": "image/png",
        "size": 182331
      },
      {
        "path": ".musts/evidence/mav-login-flow/submission-001/run.mp4",
        "mime": "video/mp4",
        "size": 4839102
      },
      {
        "path": ".musts/evidence/mav-login-flow/submission-001/report.json",
        "mime": "application/json",
        "size": 4210
      }
    ]
  },
  "snapshot": {
    "handle": "opaque-core-handle"
  }
}
```

### 10.4 Evidence Accepted Response

```json
{
  "accepted": true,
  "satisfies": [
    "App/Login/login-flow"
  ],
  "summary": "MAV report passed and required screenshot/video assets were present.",
  "normalized_assets": [
    {
      "kind": "screenshot",
      "path": ".musts/evidence/mav-login-flow/submission-001/success.png"
    },
    {
      "kind": "video",
      "path": ".musts/evidence/mav-login-flow/submission-001/run.mp4"
    },
    {
      "kind": "mav-report",
      "path": ".musts/evidence/mav-login-flow/submission-001/report.json"
    }
  ]
}
```

### 10.5 Evidence Rejected Response

```json
{
  "accepted": false,
  "missing": [
    {
      "kind": "screenshot",
      "message": "No screenshot asset was submitted."
    }
  ],
  "message": "Evidence is incomplete. Capture a screenshot and submit it with `musts evidence mav-login-flow --asset <path>`."
}
```

The CLI should render this directly for the agent:

```text
Evidence rejected for mav-login-flow.

Missing:
- screenshot: No screenshot asset was submitted.

Next:
Capture a screenshot and run:
  musts evidence mav-login-flow --asset <path>
```

### 10.6 Evidence Store Layout

Recommended internal layout:

```text
.musts/
  evidence/
    mav-login-flow/
      submission-001/
        evidence.json
        success.png
        run.mp4
        report.json
    bazel-build-login/
      submission-001/
        evidence.json
        build.log
```

### 10.7 Normalized Evidence Record

The core writes a normalized record after each submission.

```json
{
  "task_id": "mav-login-flow",
  "submitted_at": "2026-05-11T10:40:00Z",
  "snapshot_handle": "opaque-core-handle",
  "text": "Validated two valid email logins and one invalid email error path.",
  "assets": [
    {
      "original_path": "/tmp/success.png",
      "stored_path": ".musts/evidence/mav-login-flow/submission-001/success.png",
      "mime": "image/png",
      "size": 182331
    }
  ],
  "extension_result": {
    "accepted": true,
    "summary": "Required MAV report, video, and screenshot were present."
  }
}
```

---

## 11. CLI Commands

### 11.1 Required MVP Commands

```bash
musts validate
musts evidence <task-id>
```

Everything else is optional.

### 11.2 `musts validate`

`musts validate` computes pending validation tasks for the current workspace state.

It does not execute those tasks.

#### Behavior

1. Refresh manifest index.
2. Compute current content snapshot for manifest scopes.
3. Determine which scopes are dirty relative to accepted evidence.
4. Collect checks from applicable manifests.
5. Group checks by `uses`.
6. Call each extension resolver with all applicable checks for that capability.
7. Store returned task metadata internally.
8. Render an agent-readable validation report.

#### Example Output

```text
Musts validation pending.

Task: bazel-build-login
Title: Build Login module
Extension: bazel/build
Satisfies:
  - App/Login/login-build

Instructions:
  1. Run `bazel build //App/Login:Login`.
  2. Save stdout/stderr to a log file.
  3. Record evidence:
     musts evidence bazel-build-login \
       --text "bazel build //App/Login:Login succeeded" \
       --asset <build-log>

Task: mav-login-flow
Title: Validate Login flow with MAV
Extension: mav/expect
Satisfies:
  - App/Login/login-flow

Instructions:
  1. Use MAV to validate:
     - Login works with multiple valid emails.
     - Invalid email text shows an error when used as email.
  2. Produce required evidence:
     - screenshot
     - video
     - mav-report
  3. Record evidence:
     musts evidence mav-login-flow \
       --text "<summary>" \
       --asset <screenshot> \
       --asset <video> \
       --asset <mav-report>

Completion rule:
  Repeat `musts validate` after recording evidence.
  The task is not done until this report is empty.
```

#### Clean Output

```text
Musts validation clean.
No pending validation tasks for the current workspace snapshot.
```

### 11.3 `musts evidence`

`musts evidence` registers text and assets for a task.

#### Syntax

```bash
musts evidence <task-id> \
  --text "<freeform summary>" \
  --asset <path> \
  --asset <path>
```

#### Behavior

1. Load the task returned by the most recent applicable `musts validate`.
2. Copy or register submitted assets into the internal evidence store.
3. Attach submitted text.
4. Check whether the current workspace snapshot still matches the task snapshot.
5. Call the task's extension evidence validator.
6. If accepted, mark the satisfied checks green for the current snapshot.
7. If rejected, print exactly what is missing or invalid.

---

## 12. Snapshots and Efficient Change Detection

### 12.1 Requirement

The system must catch:

- manual edits
- agent edits
- Xcode saves
- generated files
- branch switches
- script outputs
- file changes outside Git

Therefore:

> Hooks and Git can be hints; content fingerprints are the truth.

### 12.2 State Database

Recommended:

```text
.musts/state.sqlite
```

### 12.3 Conceptual Tables

#### `manifest_index`

Stores:

- manifest path
- scope path
- mtime
- size
- content hash
- used extensions

#### `file_fingerprints`

Stores:

- path
- mtime
- size
- content hash
- last scanned timestamp

Hash is recomputed only when metadata changes.

#### `scope_snapshots`

Stores:

- scope path
- manifest hash
- aggregate hash of files in scope
- timestamp

#### `tasks`

Stores:

- task ID
- extension
- checks satisfied
- snapshot handle
- task metadata

#### `evidence_records`

Stores:

- task ID
- satisfied checks
- snapshot handle
- accepted/rejected status
- submitted text
- stored asset paths
- extension result
- timestamp

### 12.4 Efficient Algorithm

1. Index manifests once.
2. On future runs, check manifest mtime and size first.
3. Rehash only manifests whose metadata changed.
4. For a specific changed file path, find applicable manifests by walking ancestors:

```text
App/Login/LoginView.swift
App/Login/MUSTS.yml
App/MUSTS.yml
MUSTS.yml
```

5. For global `musts validate`, use the manifest index instead of recursively searching from scratch.
6. For files in affected scopes, use mtime and size as cheap invalidation checks.
7. Compute content hash only when metadata differs.
8. Compute aggregate scope hash from:

```text
manifest hashes + relevant file hashes
```

9. Accepted evidence remains valid only while relevant scope hashes remain unchanged.

### 12.5 What If Files Change While Evidence Is Being Recorded?

`musts evidence` refreshes the current snapshot before accepting.

If the task was generated for an older snapshot, the CLI rejects the evidence:

```text
Evidence rejected for mav-login-flow.

Reason:
  The workspace files covered by this validation task changed after the task was issued.

Next:
  Run `musts validate` again and follow the new task list.
```

The agent never passes snapshot IDs.

Snapshots are internal to the CLI.

---

## 13. Step-by-Step Runtime Flow

### Step 1: Agent Changes Code

The agent modifies files normally.

No hooks are required in the MVP.

### Step 2: Agent Runs `musts validate`

The CLI:

1. discovers applicable manifests
2. refreshes snapshots
3. groups checks by extension capability
4. calls extension resolvers
5. renders the task report

### Step 3: Extensions Resolve Checks Into Tasks

Examples:

- `bazel/build` may ignore the root app build and return only the deeper Login module build.
- `bazel/test` may group multiple targets into one test command.
- `mav/expect` may combine multiple expectations into one MAV session.

### Step 4: CLI Prints Task List

The output is written for agents.

It includes:

- task IDs
- extension names
- checks satisfied
- instructions
- required evidence
- completion rule

### Step 5: Agent Executes Tasks

The agent may:

- run Bazel
- run tests
- use MAV
- spawn subagents
- capture screenshots
- record video
- collect logs
- inspect app state

The CLI does not execute the tasks.

### Step 6: Agent Records Evidence

For each task:

```bash
musts evidence <task-id> --text "<summary>" --asset <path> ...
```

If the CLI rejects the evidence, the agent fixes what is missing and calls `musts evidence` again.

### Step 7: Agent Repeats Validation

The agent runs:

```bash
musts validate
```

If new tasks appear, it repeats the loop.

If no tasks remain, the work can be reported as complete.

---

## 14. Agent Skill

The skill is the behavioral protocol that makes the CLI useful.

The CLI is a state machine.

The skill teaches the agent how to loop over it.

### 14.1 Skill Draft

```md
# Musts Validation Skill

Purpose:
Ensure repository-defined musts validation is clean before declaring a task done.

Protocol:
1. Run `musts validate`.
2. Treat the returned tasks as the validation todo list.
3. If multiple tasks can be executed independently, use subagents in parallel.
4. If the report says one task satisfies multiple checks, execute it once.
5. Do not invent evidence requirements. Use the evidence contract in the task report.
6. For each task, perform the requested validation using the relevant tool.
7. Record evidence with:
   `musts evidence <task-id> --text "<summary>" --asset <path> ...`
8. If evidence is rejected, fix the missing/invalid evidence and call `musts evidence` again.
9. Run `musts validate` again.
10. Repeat until it reports no pending validation tasks.

Hard rule:
The task is not done until `musts validate` is empty.
```

### 14.2 Grouping Responsibilities

There are two levels of grouping.

| Grouping Type | Owner | Example |
|---|---|---|
| Domain grouping | Extension | `mav/expect` combines compatible login expectations into one MAV task. |
| Execution scheduling | Agent skill | Run Bazel build and docs lint in parallel subagents if safe. |

The extension does domain-correct grouping.

The agent does execution planning over final tasks.

---

## 15. Worked Example

### 15.1 Repo Setup

```yaml
# /MUSTS.yml
version: 1

checks:
  app-build:
    uses: bazel/build
    with:
      target: //App:App
```

```yaml
# /App/Login/MUSTS.yml
version: 1

checks:
  login-build:
    uses: bazel/build
    with:
      target: //App/Login:Login

  login-flow:
    uses: mav/expect
    with:
      expectations:
        - Login works with multiple valid emails.
        - Invalid email text shows an error when used as email.
      evidence:
        - screenshot
        - video
        - mav-report
```

### 15.2 Change

```text
Modified:
  App/Login/LoginView.swift
```

### 15.3 Core Sends Bazel Checks to `bazel/build`

The core includes both root and child checks.

The extension decides that the deeper Login target is the right task.

```json
{
  "capability": "bazel/build",
  "changed_files": [
    "App/Login/LoginView.swift"
  ],
  "checks": [
    {
      "id": "root/app-build",
      "scope_path": ".",
      "depth": 0,
      "with": {
        "target": "//App:App"
      }
    },
    {
      "id": "App/Login/login-build",
      "scope_path": "App/Login",
      "depth": 2,
      "with": {
        "target": "//App/Login:Login"
      }
    }
  ]
}
```

### 15.4 Extension Returns Task

```json
{
  "tasks": [
    {
      "id": "bazel-build-login",
      "title": "Build Login module",
      "satisfies": [
        "App/Login/login-build"
      ],
      "instructions": [
        "Run `bazel build //App/Login:Login`.",
        "Record stdout/stderr as a log asset."
      ],
      "evidence_contract": {
        "text": {
          "required": true
        },
        "assets": [
          {
            "kind": "log",
            "required": true
          }
        ]
      }
    }
  ],
  "ignored_checks": [
    {
      "id": "root/app-build",
      "reason": "A deeper target was available for the changed scope."
    }
  ]
}
```

### 15.5 Agent Runs and Records Build Evidence

```bash
bazel build //App/Login:Login 2>&1 | tee /tmp/login-build.log

musts evidence bazel-build-login \
  --text "bazel build //App/Login:Login succeeded" \
  --asset /tmp/login-build.log
```

### 15.6 Agent Runs and Records MAV Evidence

The agent uses MAV to drive the app and collect artifacts.

```bash
musts evidence mav-login-flow \
  --text "Validated valid email login and invalid email error state using MAV." \
  --asset /tmp/mav-login-success.png \
  --asset /tmp/mav-login-run.mp4 \
  --asset /tmp/mav-login-report.json
```

### 15.7 Final Check

```bash
musts validate
```

Expected:

```text
Musts validation clean.
No pending validation tasks for the current workspace snapshot.
```

---

## 16. Extension Behavior Examples

### 16.1 `bazel/build`

User manifests:

```yaml
# root
checks:
  app-build:
    uses: bazel/build
    with:
      target: //App:App
```

```yaml
# App/Login
checks:
  login-build:
    uses: bazel/build
    with:
      target: //App/Login:Login
```

If a file under `App/Login` changes, the extension may choose:

```text
bazel build //App/Login:Login
```

and ignore:

```text
bazel build //App:App
```

Reason:

```text
A deeper build target covers the changed scope.
```

If files under multiple sibling modules change, the extension may choose:

```text
bazel build //App/Login:Login //App/Profile:Profile
```

If many modules change, the extension may choose:

```text
bazel build //App:App
```

These strategies are internal to `bazel/build`.

### 16.2 `mav/expect`

Two checks:

```yaml
checks:
  login-valid:
    uses: mav/expect
    with:
      expectations:
        - Login works with multiple valid emails.
      evidence:
        - screenshot
        - video

  login-invalid:
    uses: mav/expect
    with:
      expectations:
        - Invalid email text shows an error when used as email.
      evidence:
        - screenshot
        - video
```

The extension may return one task:

```text
Task: mav-login-session
Satisfies:
  - login-valid
  - login-invalid

Instructions:
  Use MAV to validate both valid email login and invalid email error behavior in one session.

Evidence:
  - screenshot
  - video
  - mav-report
```

The user did not configure grouping. The extension decided.

---

## 17. Undefined or Still Risky

### 17.1 Naming

Options:

- `MUSTS.yml`
- `VERIFY.yml`
- `VALIDATE.yml`
- `.musts.yml`

Current recommendation:

```text
MUSTS.yml
```

Reason:

- matches the broader musts engineering concept
- makes room for future musts-related fields
- still scoped to validation in v1

Risk:

- may sound broader than the MVP

### 17.2 Extension Runtime

Question:

Should extensions be Node, Python, shell, WASM, or arbitrary executables?

MVP recommendation:

```text
arbitrary executable + stdin/stdout JSON
```

Reason:

- language-agnostic
- easy to implement
- compatible with repo-local tools

### 17.3 Asset Typing

Question:

Should the agent label assets manually?

Options:

```bash
--asset screenshot=/tmp/a.png
--asset-kind screenshot --asset /tmp/a.png
--asset /tmp/a.png
```

MVP recommendation:

```bash
--asset /tmp/a.png
```

The core infers MIME type. The extension classifies assets.

Add explicit asset kinds later only if inference fails too often.

### 17.4 Parallelism

Question:

How do we prevent subagents from fighting over:

- simulators
- local servers
- ports
- build locks
- database state

MVP recommendation:

- extensions return `parallelizable`
- agent skill uses that as guidance
- resource locks are future work

### 17.5 Snapshot Scope

Question:

Exactly which files are included in each scope hash?

MVP recommendation:

- include files under the manifest folder recursively
- exclude common ignored directories
- include manifest file hashes
- include extension descriptor hashes

Potential ignored directories:

```text
.git
.musts/evidence
.musts/cache
node_modules
bazel-bin
bazel-out
bazel-testlogs
DerivedData
```

This needs concrete implementation decisions.

### 17.6 Generated Files

Question:

Should generated files invalidate validation?

MVP recommendation:

Default yes unless ignored.

Reason:

False positives are safer than stale evidence.

### 17.7 Evidence Quality

Question:

Can screenshot/video prove behavior?

Answer:

Not fully.

They create reviewable evidence. Stronger validation requires structured reports where available.

For MAV, the extension should prefer:

- MAV report JSON
- screenshot
- video
- accessibility tree where useful

### 17.8 Security

Question:

Can extensions execute unsafe commands?

MVP stance:

Repo-local extensions are trusted.

Hardening is future work.

Possible future features:

- extension signing
- allowlist
- sandboxed extension runtime
- permission prompts

### 17.9 Hooks

Question:

Should Claude hooks block completion?

MVP stance:

No.

Future:

```text
before done -> run musts validate
if pending -> block/report tasks
```

---

## 18. MVP Implementation Plan

### Phase 1: Core Manifest and State

Implement:

- parse `MUSTS.yml`
- require `version: 1`
- require `checks`
- require each check to have `uses`
- allow arbitrary `with`
- build manifest index in `.musts/state.sqlite`
- compute simple scope snapshots

Deliverable:

```bash
musts validate
```

can discover manifests and report schema errors.

### Phase 2: Extension Loading

Implement:

- load `.musts/extensions/*/extension.yml`
- map `uses` values to capabilities
- validate `with` payloads with JSON Schema where available
- call resolver command over stdin JSON
- parse resolver JSON response

Deliverable:

```text
MUSTS.yml checks become extension-generated tasks.
```

### Phase 3: Validate Report

Implement:

- `musts validate`
- agent-readable task rendering
- ignored checks rendering
- clean state rendering
- extension failure rendering

Deliverable:

```text
Agent can run musts validate and receive a task list.
```

### Phase 4: Evidence Recording

Implement:

- `musts evidence <task-id>`
- `--text`
- repeatable `--asset`
- asset copy into `.musts/evidence`
- MIME/type metadata
- evidence validation command
- accepted evidence ledger
- rejected evidence feedback
- stale snapshot rejection

Deliverable:

```text
Agent can satisfy tasks by submitting evidence.
```

### Phase 5: Two Real Extensions

Implement:

#### `bazel/build`

Responsibilities:

- accept `target`
- choose deepest applicable target when root and child builds both apply
- return command instructions
- require text summary
- require log asset
- validate evidence contains a log and success text

Initial evidence validation can be simple.

Later it can parse structured Bazel output.

#### `mav/expect`

Responsibilities:

- accept expectations and required evidence types
- group compatible expectations into one task
- generate MAV-oriented agent instructions
- require text summary
- require screenshot/video/report assets based on check config
- validate submitted assets by MIME/path/report content

### Phase 6: Agent Skill

Implement a reusable skill:

```text
Run musts validate.
Execute returned tasks.
Record evidence.
Repeat until clean.
Do not finish until clean.
```

Deliverable:

```text
Agents can reliably follow the validation loop.
```

---

## 19. MVP Success Criterion

A successful MVP should support this scenario:

1. A repo has:

```text
MUSTS.yml
App/Login/MUSTS.yml
.musts/extensions/bazel
.musts/extensions/mav
```

2. The agent changes:

```text
App/Login/LoginView.swift
```

3. The agent runs:

```bash
musts validate
```

4. The CLI returns:

```text
- Build Login module
- Validate Login flow with MAV
```

It does not return the root full app build if `bazel/build` decides the child build is sufficient.

5. The agent runs the build and records evidence.

6. The agent uses MAV and records evidence.

7. The agent runs:

```bash
musts validate
```

8. The CLI returns:

```text
Musts validation clean.
No pending validation tasks for the current workspace snapshot.
```

This must work without:

- git diff
- hooks
- CI
- manually maintained task graphs
- user-exposed resolution policies

---

## 20. Later Layers

These are natural next steps but should not be in the MVP.

### 20.1 Claude Hooks

Potential use:

```text
Before final response:
  run musts validate
  if pending tasks exist, block completion
```

### 20.2 Codex Integration

Potential use:

- skill-based loop
- maybe local wrapper
- maybe future hook equivalent

### 20.3 Full Harness Management

Future command family:

```bash
musts install extension mav
musts doctor
musts sync
musts list extensions
musts init
```

### 20.4 Facts Integration

Possible later link:

```text
facts describe semantic truths
MUSTS.yml says how to validate areas of the repo
extensions produce evidence
```

### 20.5 Context Injection

Possible later behavior:

```text
Given changed files, generate the validation context the agent should read.
```

This may integrate with `AGENTS.md` or `CLAUDE.md`, but should not be required for MVP.

---

## 21. Design Summary

The current design is:

```text
User writes simple local checks.
Core discovers applicable checks.
Core groups checks by extension.
Extension resolves checks into final validation tasks.
Agent executes tasks.
Agent submits text/assets as evidence.
Core stores evidence.
Extension accepts or rejects evidence.
Core records accepted evidence against current snapshots.
Agent repeats until validate is clean.
```

The most important boundary:

```text
The CLI does not run validation.
The agent does.
The CLI decides whether the produced evidence is accepted.
```

The most important product sentence:

> The task is not done until `musts validate` is empty.

