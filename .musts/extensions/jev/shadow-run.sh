#!/usr/bin/env bash
# musts' own use of the capability. One call per file, recording only.
#
# Deliberately NOT a check in MUSTS.yml: `musts run` refuses non-built-in capabilities on
# purpose (crates/musts-core/src/run.rs), and a pending judgment task blocks every commit —
# a shadow wired as a check would block exactly what it promises not to block.
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
bin="$root/target/release/musts-jev"; [ -x "$bin" ] || bin="$root/target/debug/musts-jev"
[ -x "$bin" ] || { echo "build it first: cargo build -p musts-jev"; exit 1; }
if [ "${1:-}" = "--all" ]; then
  set -- $(cd "$root" && git ls-files 'crates/**/*.rs' 'tests/**/*.rs')
else
  set -- $(cd "$root" && git diff --name-only origin/main...HEAD -- 'crates/**/*.rs' 'tests/**/*.rs')
fi
[ "$#" -eq 0 ] && { echo "nothing to judge"; exit 0; }
# Count what went in and what came out, and fail loudly if they differ. An earlier version
# piped each call into `head -1`, and SIGPIPE plus `pipefail` killed the loop after ONE file
# while the run still looked like it had judged everything. A sweep that silently judges
# fewer files than it was given is the same failure as a shadow that records nothing: it
# reads as coverage. So the count is checked rather than trusted.
wanted=$#
got=0
for f in "$@"; do
  line="$("$bin" --questions "$root/.musts/extensions/jev/questions/self-comparing.json" \
    --ask self_comparing --expect no --mode shadow --root "$root" "$f" 2>&1 \
    | grep -E "^(ok|UNSURE|FAIL|SKIP|BROKEN)" || true)"
  if [ -n "$line" ]; then
    printf '%s\n' "$line"
    got=$((got + 1))
  else
    printf 'NO OUTPUT %s\n' "$f"
  fi
done
printf '\n%d of %d files produced a verdict line.\n' "$got" "$wanted"
if [ "$got" -ne "$wanted" ]; then
  printf 'INCOMPLETE: %d files were judged silently or not at all. This run proves nothing about them.\n' \
    "$((wanted - got))"
  exit 1
fi
