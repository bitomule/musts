# Recording a `bazel test` you already ran: `musts record`

`musts run bazel-test-root` runs the check's `bazel test` itself. When the
agent has just run the same tests on the same tree, that second run proves
nothing new. `musts record` accepts the first run instead, from Bazel's own
machine-readable record of it, and only when that record proves the check.

```console
$ bazel test //crates/... //tools:licenses_test
$ musts record bazel-test-root
Recorded from bazel invocation 5ae1c0b7:
  ✓ root/bazel-test — 87 tests passed, tree unchanged since the invocation
Run `musts validate` to confirm the loop is clean.
```

## Setup: two `.bazelrc` lines and a script

```
# .bazelrc
build --workspace_status_command=tools/musts-stamp.sh
test --build_event_json_file=.musts/bep.json
```

```sh
#!/bin/sh
# tools/musts-stamp.sh (chmod +x)
command -v musts >/dev/null 2>&1 && musts stamp
exit 0
```

And in `.gitignore`:

```
.musts/bep.json
```

- `--build_event_json_file` makes every `bazel test` write its Build Event
  Protocol stream (one JSON event per line) to `.musts/bep.json`, the path
  `musts record` reads by default (`--from <path>` reads another). A relative
  path is resolved against the workspace root. Bazel does not create the
  folder: if it is missing the whole invocation fails, which is why the file
  lives in `.musts/`, present in every repo that commits its ledger. Each
  `bazel test` overwrites it, so `musts record` reads the latest run.
- `--workspace_status_command` runs `musts stamp` at the start of every
  build. It prints the current scope hash of every check as one line,
  `MUSTS_SCOPES {"root/bazel-test":"4f1c…"}`, and Bazel copies it into the
  stream's `workspaceStatus` event. That is how `musts record` knows which
  tree the run saw: the stream itself carries no hash of the sources. The key
  has no `STABLE_` prefix, so it never invalidates a cached action or test.
- `musts stamp` always exits 0 and prints nothing when it cannot stamp (no
  `MUSTS.yml`, the workspace lock busy for more than 5 s): a status command
  that fails would fail the build. The script also exits 0 where `musts` is
  not installed, such as CI.

Cost, measured on the Alondra repo (3,417 files): 0.07–0.10 s
wall and about 0.07 s CPU per build once the checkout's fingerprint cache is
warm; 2.2 s the first time in a fresh checkout.

## What `musts record` accepts

`musts record <task-id>` judges every check of a `bazel/test` task;
`musts record <check-id>` (e.g. `root/bazel-test`) judges one. Each check is
recorded only when all of these hold, and the first one that fails is the
reason it stays pending:

1. The file is a JSON build event stream (the binary
   `--build_event_binary_file` is not read).
2. The stream is complete: it has `buildFinished` and an event with
   `lastMessage: true`. A run still in progress or interrupted is not.
3. The command was `bazel test`.
4. It ran in this workspace (`started.workspaceDirectory`).
5. No `--test_filter` in the effective options (`optionsParsed.cmdLine`,
   which includes `.bazelrc`): a filtered run reports PASSED for tests it
   only partly ran.
6. The stream carries the musts stamp, with a hash for the check.
7. That hash equals the check's scope hash now: nothing in the check's scope
   changed since the run.
8. No file in the check's scope was modified after the run started: a file
   edited during the run and reverted has the same hash, but Bazel may have
   tested the other version.
9. Every entry of the check's `targets:` is proven:
   - a label needs a `testSummary` whose `overallStatus` is `PASSED`, in
     every configuration; a `test_suite` label needs that for each of its
     tests;
   - a pattern (`//pkg/...`, `//pkg:all`, `//...`) needs the run to have
     requested it or a pattern containing it, with no negative pattern
     cutting into it (unless the check declares the same exclusion); every
     test target under it (`targetKind` ending in `_test rule`) needs a
     `PASSED` summary, except targets skipped as incompatible; and at least
     one test under it must have passed.

A cached `PASSED` counts: Bazel caches a test result by the exact inputs of
the test. Configuration differences (`-c dbg`, `--config=…`,
`--runs_per_test`) are not rejected.

Everything is decided from those fields. `musts record` never reads the
human log or `statusDetails`.

Exit codes: 0 every requested check recorded; 1 some check rejected (the
proven ones are still recorded); 2 the task does not exist, is stale (run
`musts validate`), or is not a `bazel/test` task.

## Rejection reasons and what to do

| Reason | What to do |
|---|---|
| `cannot read the build event file .musts/bep.json` | Add the `.bazelrc` lines, or pass `--from`. |
| `… is not a JSON build event file` | Use `--build_event_json_file`, not the binary flag. |
| `the build event file is incomplete` | Wait for `bazel test` to finish, or run it again. |
| `the invocation was "bazel build", not "bazel test"` | The latest run was not a test; run the tests. |
| `the invocation ran in …, not in this workspace` | The file came from another checkout. |
| `--test_filter=… ran only part of each test` | Run without `--test_filter`. |
| `the invocation carries no musts stamp` | Add `--workspace_status_command`; check `musts` is on `PATH`. |
| `the tree changed since the invocation` | You edited after testing: test again, or `musts run`. |
| `… was modified at …, after the invocation started` | Same: test again on the final tree. |
| `//x:y FAILED` / `TIMEOUT` / `FAILED_TO_BUILD` / `NO_STATUS` | Fix the test. |
| `//x:y was FLAKY (passed only on a retry)` | A flaky pass proves nothing; fix it or `musts run`. |
| `//x:y was built but not run (excluded by a test filter)` | Drop `--test_tag_filters`/size/lang/timeout filters. |
| `//x:y was not part of the invocation` | Your run did not cover the check: run all its targets. |
| `//x/... was not requested` | Same: request the check's pattern (or a wider one). |
| `the invocation excluded -//x/…, inside //x/...` | Run without that negative pattern. |
| `//x:y was aborted (REASON)` | Fix the build or analysis error. |
| `no test under //x/... passed in the invocation` | The run produced no test results there. |

## For a repo's `AGENTS.md`

> **Tests already run? Record them.** After a `bazel test` that covers every
> target of a `bazel/test` check, record it with `musts record <task-id>`
> (the task id `musts validate` prints, e.g. `bazel-test-root`) instead of
> `musts run`, which would run the same tests again. It reads
> `.musts/bep.json`, which every `bazel test` writes here, and records the
> check only if every target passed and nothing in its scope changed since
> the run. If it answers "Not recorded", the reason says what is missing:
> edited after testing or a test that did not pass → fix and test again;
> a run that did not cover the check's targets → run them all, or
> `musts run <task-id>`. Never pass `--test_filter` or test tag/size
> filters to a run you mean to record.

## Sources

- Build Event Protocol: <https://bazel.build/remote/bep>,
  glossary <https://bazel.build/remote/bep-glossary>, proto
  [`build_event_stream.proto`](https://github.com/bazelbuild/bazel/blob/master/src/main/java/com/google/devtools/build/lib/buildeventstream/proto/build_event_stream.proto).
- Workspace status command: <https://bazel.build/docs/user-manual#workspace-status-command>.
- Flags: <https://bazel.build/reference/command-line-reference>.
- BEP is listed as a stable API: <https://bazel.build/release/backward-compatibility>.
- Field shapes checked against a real Bazel 8.4.2 stream:
  `crates/musts-core/src/builtin/testdata/bep_bazel_8_4_2.json`.
