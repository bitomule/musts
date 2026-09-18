#!/usr/bin/env bash
# Runs the hollow-test question over this repo's test sources and records one row per judged
# file. Deliberately NOT a check in MUSTS.yml: a pending judgment task blocks every commit,
# and `musts run` refuses external extensions on purpose (crates/musts-core/src/run.rs).
#
#   .musts/extensions/jev/shadow-run.sh          # files changed against origin/main
#   .musts/extensions/jev/shadow-run.sh --all    # every test source
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
bin="$root/target/release/musts-jev"
[ -x "$bin" ] || bin="$root/target/debug/musts-jev"
[ -x "$bin" ] || { echo "build it first: cargo build -p musts-jev"; exit 1; }
# Plain `set --`, not mapfile: macOS ships bash 3.2 and mapfile is bash 4.
if [ "${1:-}" = "--all" ]; then
  set -- $(cd "$root" && git ls-files 'crates/**/*.rs' 'tests/**/*.rs')
else
  set -- $(cd "$root" && git diff --name-only origin/main...HEAD -- 'crates/**/*.rs' 'tests/**/*.rs')
fi
[ "$#" -eq 0 ] && { echo "nothing to judge"; exit 0; }
exec "$bin" --questions "$root/.musts/extensions/jev/questions/hollow-rust-test.json" \
  --ask hollow --expect no --mode shadow --root "$root" "$@"
