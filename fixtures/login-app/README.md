# `fixtures/login-app/`

Canonical workspace mirroring spec §15. Used by [`crates/musts/tests/phase6_e2e.rs::full_section_15_worked_example`](../../crates/musts/tests/phase6_e2e.rs) and as a manual smoke test for the §19 success criterion.

## Layout

```
fixtures/login-app/
├── MUSTS.yml                      # root bazel/build app-build check
├── App/Login/
│   ├── MUSTS.yml                  # bazel/build login-build + mav/expect login-flow
│   └── LoginView.swift              # source file under the deeper scope
└── .musts/extensions/
    ├── bazel/extension.yml          # points at `bazel-extension` on PATH
    └── mav/extension.yml            # points at `mav-extension` on PATH
```

## Walking the success criterion by hand

Extension descriptors resolve relative `command` paths against the descriptor directory (PLAN.md §4.6), so the binaries live next to `extension.yml`. The fixture ships **no binaries** — you symlink them in once after a `cargo build`:

```bash
cd fixtures/login-app

# 0. Build the workspace once.
cargo build --workspace --manifest-path ../../Cargo.toml

# 1. Link the freshly-built binaries into the descriptors. The
#    descriptor's `command: ["bazel-extension", "resolve"]` will then
#    find them at .musts/extensions/bazel/bazel-extension.
ln -sf "$(pwd)/../../target/debug/bazel-extension" .musts/extensions/bazel/bazel-extension
ln -sf "$(pwd)/../../target/debug/mav-extension"   .musts/extensions/mav/mav-extension

# 2. Use the musts binary directly (no install required).
MUSTS=../../target/debug/musts

# 3. Edit the source file to trigger a dirty scope.
echo "// edit" >> App/Login/LoginView.swift

# 4. Validate — should emit one bazel task (subsumes root) + one mav task.
$MUSTS --workspace . validate

# 5. Record evidence — assets staged OUTSIDE the workspace so they
#    don't mutate scope hashes.
mkdir -p /tmp/login-evidence
printf 'bazel build //App/Login:Login\nINFO: Build completed successfully\n' > /tmp/login-evidence/build.log
printf '\x89PNG\r\n\x1a\n' > /tmp/login-evidence/login.png
dd if=/dev/zero of=/tmp/login-evidence/login.mp4 bs=1 count=64 2>/dev/null
echo '{"summary":"ok"}' > /tmp/login-evidence/mav-report.json

$MUSTS --workspace . evidence bazel-build-app-login \
  --text "bazel build //App/Login:Login succeeded" \
  --asset /tmp/login-evidence/build.log

$MUSTS --workspace . evidence mav-expect-app-login \
  --text "MAV: validated both expectations" \
  --asset /tmp/login-evidence/login.png \
  --asset /tmp/login-evidence/login.mp4 \
  --asset /tmp/login-evidence/mav-report.json

# 6. Re-validate — should be clean.
$MUSTS --workspace . validate
```

After the second validate you should see:

```text
Harness validation clean.
No pending validation tasks for the current workspace snapshot.
```

The same flow is checked-in as `phase6_e2e::full_section_15_worked_example` and runs on every `cargo test`.

## Resetting between runs

```bash
rm -rf .musts/state.sqlite* .musts/evidence .musts/.lock
```

The symlinks under `.musts/extensions/<name>/` are intentionally not
checked in; they're produced by step 1 above and survive between runs.
