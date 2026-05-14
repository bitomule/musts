# `fixtures/login-app/`

Canonical workspace mirroring spec §15. Used by [`crates/musts/tests/phase6_e2e.rs::full_section_15_worked_example`](../../crates/musts/tests/phase6_e2e.rs) and as a manual smoke test for the §19 success criterion.

## Layout

```
fixtures/login-app/
├── MUSTS.yml                # root bazel/build app-build check
├── App/Login/
│   ├── MUSTS.yml            # bazel/build login-build + mav/expect login-flow
│   └── LoginView.swift      # source file under the deeper scope
```

The `bazel/build` and `mav/expect` capabilities are **built into the `musts` binary** — no extension descriptors, no schemas, no symlinks. The fixture is intentionally bare.

## Walking the success criterion by hand

```bash
cd fixtures/login-app

# 0. Build musts once.
cargo build --manifest-path ../../Cargo.toml

# 1. Use the musts binary directly (no install required).
MUSTS=../../target/debug/musts

# 2. Edit the source file to trigger a dirty scope.
echo "// edit" >> App/Login/LoginView.swift

# 3. Validate — should emit one bazel task (subsumes root) + one mav task.
$MUSTS --workspace . validate

# 4. Record evidence — assets staged OUTSIDE the workspace so they
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

# 5. Re-validate — should be clean.
$MUSTS --workspace . validate
```

After the second validate you should see:

```text
Musts validation clean.
No pending validation tasks for the current workspace snapshot.
```

The same flow is checked-in as `phase6_e2e::full_section_15_worked_example` and runs on every `cargo test`.

## Resetting between runs

```bash
rm -rf .musts/state.sqlite* .musts/evidence .musts/.lock
```
