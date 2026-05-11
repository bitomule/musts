# `fixtures/login-app/`

Canonical workspace mirroring spec §15. Used by [`crates/harness/tests/phase6_e2e.rs::full_section_15_worked_example`](../../crates/harness/tests/phase6_e2e.rs) and as a manual smoke test for the §19 success criterion.

## Layout

```
fixtures/login-app/
├── HARNESS.yml                      # root bazel/build app-build check
├── App/Login/
│   ├── HARNESS.yml                  # bazel/build login-build + mav/expect login-flow
│   └── LoginView.swift              # source file under the deeper scope
└── .harness/extensions/
    ├── bazel/extension.yml          # points at `bazel-extension` on PATH
    └── mav/extension.yml            # points at `mav-extension` on PATH
```

## Walking the success criterion by hand

Both extensions need to be on `PATH`. Either `cargo install` them or use the absolute paths:

```bash
cd fixtures/login-app

# Use the workspace binaries directly:
export PATH="$(pwd)/../../target/debug:$PATH"
cargo build --workspace                # populates the target/ binaries

# 1. Edit the source file to trigger a dirty scope.
echo "// edit" >> App/Login/LoginView.swift

# 2. Validate — should emit one bazel task (subsumes root) + one mav task.
harness --workspace . validate

# 3. Record evidence — assets staged OUTSIDE the workspace so they
#    don't mutate scope hashes.
mkdir -p /tmp/login-evidence
echo "bazel build //App/Login:Login\nINFO: Build completed successfully\n" > /tmp/login-evidence/build.log
printf '\x89PNG' > /tmp/login-evidence/login.png
printf '' > /tmp/login-evidence/login.mp4
echo '{"summary":"ok"}' > /tmp/login-evidence/mav-report.json

harness --workspace . evidence bazel-build-app-login \
  --text "bazel build //App/Login:Login succeeded" \
  --asset /tmp/login-evidence/build.log

harness --workspace . evidence mav-expect-app-login \
  --text "MAV: validated both expectations" \
  --asset /tmp/login-evidence/login.png \
  --asset /tmp/login-evidence/login.mp4 \
  --asset /tmp/login-evidence/mav-report.json

# 4. Re-validate — should be clean.
harness --workspace . validate
```

After the second validate you should see:

```text
Harness validation clean.
No pending validation tasks for the current workspace snapshot.
```

The same flow is checked-in as `phase6_e2e::full_section_15_worked_example` and runs on every `cargo test`.

## Resetting between runs

```bash
rm -rf .harness/state.sqlite* .harness/evidence
```
