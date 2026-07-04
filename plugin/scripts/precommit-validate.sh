#!/usr/bin/env bash
# PreToolUse hook for the musts Claude Code plugin.
#
# Enforces the musts loop at the moment the agent declares work done: a
# `git commit`. It reads the PreToolUse event JSON from stdin, and when
# the Bash command about to run is a `git commit`, it locates the nearest
# MUSTS.yml above the session cwd, runs `musts validate`, and on a
# non-clean result blocks the commit (stderr + exit 2) so the model has to
# close the loop first.
#
# Why the commit boundary instead of Stop: the Stop event fires on every
# turn end — including when the agent paused to await background subagents
# or a workflow, stopped to ask the user, or did unrelated non-code work —
# so a Stop hook nags at the wrong moments. `git commit` is the agent's
# explicit "this is done" signal; validating there fires exactly once, at
# the right time. CI (`musts validate` as a required check) remains the
# backstop for anything committed by other means.
#
# Exits 0 (silent, allows the tool call) when: the command isn't a
# git commit, not a musts repo, musts binary missing, or validate is clean.

set -uo pipefail

input="$(cat)"

# --- Extract the Bash command being run ------------------------------------
# Prefer jq, fall back to python3, then a best-effort sed. Any failure
# leaves `cmd` empty, which is treated as "not a git commit" (fail open —
# we never want the hook to wedge commits because a parser hiccupped).
cmd=""
if command -v jq >/dev/null 2>&1; then
  cmd="$(printf '%s' "$input" | jq -r '.tool_input.command // ""' 2>/dev/null)"
elif command -v python3 >/dev/null 2>&1; then
  cmd="$(printf '%s' "$input" | python3 -c 'import json,sys
try:
    print(json.load(sys.stdin).get("tool_input", {}).get("command", ""))
except Exception:
    pass' 2>/dev/null)"
else
  cmd="$(printf '%s' "$input" | sed -n 's/.*"command"[[:space:]]*:[[:space:]]*"\(.*\)".*/\1/p')"
fi

# --- Is this a `git commit`? -----------------------------------------------
# Match `git commit`, `git commit -m ...`, `git -C <dir> commit`, and forms
# joined with && / ; / |. Requires a non-word char before `git` so
# `mygit commit` doesn't match, and whitespace/end after `commit` so
# `git commit-graph` doesn't match. Global-flag forms other than `-C` are
# not matched (rare for commit); CI is the backstop if one slips through.
is_git_commit() {
  printf '%s' "$1" | grep -Eq \
    '(^|[^[:alnum:]_-])git[[:space:]]+(-C[[:space:]]+[^[:space:]]+[[:space:]]+)?commit([[:space:]]|$)'
}

is_git_commit "$cmd" || exit 0

# --- Locate the nearest MUSTS.yml above cwd --------------------------------
cwd="$(printf '%s' "$input" | sed -n 's/.*"cwd"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
[ -z "$cwd" ] && cwd="$PWD"

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

# Not a musts repo — allow the commit silently.
[ -z "$found" ] && exit 0

# musts must be on PATH. If it isn't, print one short hint and allow the
# commit (exit 0) so we don't wedge repos that haven't installed the CLI.
if ! command -v musts >/dev/null 2>&1; then
  echo "musts: binary not on PATH. Install with 'brew install bitomule/tap/musts' or 'cargo install musts'." >&2
  exit 0
fi

# --- Validate, and block the commit if the loop isn't clean ----------------
output="$(cd "$found" && musts validate 2>&1)"
status=$?

if [ "$status" -eq 0 ]; then
  exit 0
fi

{
  echo "musts validate reports pending work — resolve it before committing:"
  echo
  echo "$output"
} >&2
exit 2
