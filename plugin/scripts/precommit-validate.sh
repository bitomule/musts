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

# --- Read a string field out of the event JSON -----------------------------
# Prefer jq, then python3, then a best-effort sed. Any failure yields an
# empty string. We never want the hook to wedge commits because a parser
# hiccupped, so callers treat "empty" as fail-open.
json_field() {
  local field="$1"
  if command -v jq >/dev/null 2>&1; then
    printf '%s' "$input" | jq -r --arg f "$field" 'getpath($f | split(".")) // ""' 2>/dev/null
  elif command -v python3 >/dev/null 2>&1; then
    printf '%s' "$input" | FIELD="$field" python3 -c 'import json,os,sys
try:
    obj = json.load(sys.stdin)
    for part in os.environ["FIELD"].split("."):
        obj = obj.get(part, "") if isinstance(obj, dict) else ""
    print(obj if isinstance(obj, str) else "")
except Exception:
    pass' 2>/dev/null
  else
    # Flat best-effort: last-segment key only. Good enough for the fallback.
    local key="${field##*.}"
    printf '%s' "$input" | sed -n "s/.*\"$key\"[[:space:]]*:[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p"
  fi
}

cmd="$(json_field tool_input.command)"

# --- Does the command actually run `git commit`? ---------------------------
# We do NOT substring-match the raw command: `git log --grep="git commit"`
# and `echo "git commit"` are not commits, and matching them would block
# legitimate work whenever the loop is dirty. Instead we split on shell
# operators (`; & | && || ( )` and newlines) and, per segment, find the
# first real word (skipping leading VAR=value env assignments), require it
# to be `git`, skip global options (including value-taking `-C`/`-c`/
# `--git-dir` …), and check the first *subcommand* is `commit`. This also
# catches chained/subshell/global-flag commits the old regex missed.
#
# awk is POSIX-guaranteed. It does not parse quotes, but the
# first-subcommand rule makes that irrelevant for the cases that matter:
# `git log --grep="… commit"` stops at the `log` subcommand, and a quoted
# operator inside a real commit message still leaves `git commit` as the
# first segment.
command_runs_git_commit() {
  printf '%s' "$1" | awk '
    function is_commit(n, w,    i) {
      i = 1
      while (i <= n && w[i] ~ /^[A-Za-z_][A-Za-z0-9_]*=/) i++   # env assignments
      if (i > n) return 0
      prog = w[i]; sub(/^.*\//, "", prog)                        # basename of argv0
      if (prog != "git") return 0
      i++
      while (i <= n) {
        if (w[i] ~ /^-/) {
          if (w[i] == "-C" || w[i] == "-c" || w[i] == "--git-dir" || \
              w[i] == "--work-tree" || w[i] == "--namespace" || w[i] == "--exec-path")
            i += 2                                               # option takes a value
          else
            i += 1
        } else {
          return (w[i] == "commit") ? 1 : 0                      # first subcommand
        }
      }
      return 0
    }
    {
      gsub(/&&|\|\||[;&|()]/, "\n", $0)                          # operators -> segment breaks
      m = split($0, lines, /\n/)
      for (li = 1; li <= m; li++) {
        n = split(lines[li], words, /[ \t]+/)
        c = 0
        for (k = 1; k <= n; k++) if (words[k] != "") w[++c] = words[k]
        if (is_commit(c, w)) { found = 1 }
        delete w
      }
    }
    END { exit(found ? 0 : 1) }
  '
}

command_runs_git_commit "$cmd" || exit 0

# --- Locate the nearest MUSTS.yml above cwd --------------------------------
cwd="$(json_field cwd)"
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
