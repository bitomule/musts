#!/usr/bin/env bash
# Stop hook for the musts Claude Code plugin.
#
# Reads the Stop-event JSON from stdin, locates the nearest MUSTS.yml above
# the session's cwd, runs `musts validate`, and on a non-clean exit feeds the
# report back to Claude (stderr + exit 2) so the model has to address the
# tasks before it stops.
#
# Exits 0 (silent) when: not a musts repo, musts binary missing, validate
# clean, or an earlier hook already triggered a block.

set -uo pipefail

input="$(cat)"

# Avoid recursive blocking if Claude is already responding to a hook block.
case "$input" in
  *'"stop_hook_active"'*':'*'true'*) exit 0 ;;
esac

# Extract cwd from the event payload. Falls back to $PWD if absent.
cwd="$(printf '%s' "$input" | sed -n 's/.*"cwd"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
[ -z "$cwd" ] && cwd="$PWD"

# Walk up the directory tree looking for MUSTS.yml.
found=""
dir="$cwd"
while :; do
  if [ -f "$dir/MUSTS.yml" ]; then
    found="$dir"
    break
  fi
  parent="$(dirname "$dir")"
  if [ "$parent" = "$dir" ]; then
    break
  fi
  dir="$parent"
done

# Not a musts repo — exit silently. The hook should be invisible everywhere
# else.
[ -z "$found" ] && exit 0

# musts must be on PATH. If it isn't, print one short hint to stderr and exit
# 0 so we don't block Stop in repos that haven't installed the CLI yet.
if ! command -v musts >/dev/null 2>&1; then
  echo "musts: binary not on PATH. Install with 'brew install bitomule/tap/musts' or 'cargo install musts'." >&2
  exit 0
fi

# Run validate from the discovered root and capture combined output.
output="$(cd "$found" && musts validate 2>&1)"
status=$?

if [ "$status" -eq 0 ]; then
  exit 0
fi

{
  echo "musts validate reports pending work. Address every task before stopping:"
  echo
  echo "$output"
} >&2
exit 2
