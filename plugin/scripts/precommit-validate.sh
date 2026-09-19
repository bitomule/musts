#!/usr/bin/env bash
# PreToolUse hook for the musts Claude Code plugin.
#
# Enforces the musts loop at the moment the agent declares work done: a
# `git commit`. It reads the PreToolUse event JSON from stdin, and when
# the Bash command about to run is a `git commit`, it locates the
# MUSTS.yml governing *that commit's repository*, runs `musts validate`,
# and on a non-clean result blocks the commit (stderr + exit 2) so the
# model has to close the loop first.
#
# The repository is resolved from the command, not from the session cwd.
# A single agent session routinely spans several repos, and the session
# cwd is wherever it started — so anchoring there made a `git commit` in
# repo B fail on repo A's pending tasks, including when B had no musts
# manifest at all.
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

# --- Does the command run `git commit`, and against which repository? ------
# We do NOT substring-match the raw command: `git log --grep="git commit"`
# and `echo "git commit"` are not commits, and matching them would block
# legitimate work whenever the loop is dirty. Instead we split on shell
# operators (`; & | && || ( )` and newlines) and, per segment, find the
# first real word (skipping leading VAR=value env assignments), require it
# to be `git`, skip global options (including value-taking `-C`/`-c`/
# `--git-dir` …), and check the first *subcommand* is `commit`. This also
# catches chained/subshell/global-flag commits the old regex missed.
#
# The same walk collects what we need to find the *target repository*:
# any `cd <dir>` in an earlier segment of the same command, and the git
# global options that precede the `commit` subcommand. Those two decide
# where git itself would run, which is the only place worth validating.
#
# Output on a match, one record per line:
#   cd<TAB><dir>     zero or one, the effective `cd` target
#   opt<TAB><word>   zero or more, git global options before `commit`
#
# awk is POSIX-guaranteed. It does not parse quotes, but the
# first-subcommand rule makes that irrelevant for the cases that matter:
# `git log --grep="… commit"` stops at the `log` subcommand, and a quoted
# operator inside a real commit message still leaves `git commit` as the
# first segment.
#
# Known limits (CI's required `musts validate` is the backstop for these):
# - A commit run through a wrapper whose argv0 isn't `git` — `sudo git
#   commit`, `command git commit`, `env git commit`, `eval "git commit"` —
#   is not detected. Agents commit with plain `git`, so this is exotic.
# - A shell operator that appears *inside a quoted argument* of a non-git
#   command (`echo "a; git commit"`) can over-match. That only ever
#   over-validates (blocks a benign command while the loop is dirty); it
#   never lets an unvalidated commit through.
# - Paths containing whitespace lose their quoting in the same way, so a
#   `cd`/`-C` into such a directory falls back to the session cwd.
git_commit_context() {
  printf '%s' "$1" | awk '
    function first_word(n, w,    i) {                            # index past env assignments
      i = 1
      while (i <= n && w[i] ~ /^[A-Za-z_][A-Za-z0-9_]*=/) i++
      return i
    }
    # Emits opt records and returns 1 when this segment is a git commit.
    function scan(n, w,    i, prog, buf, c) {
      i = first_word(n, w)
      if (i > n) return 0
      prog = w[i]; sub(/^.*\//, "", prog)                        # basename of argv0
      if (prog != "git") return 0
      i++
      c = 0
      while (i <= n) {
        if (w[i] ~ /^-/) {
          buf[++c] = w[i]
          if (w[i] == "-C" || w[i] == "-c" || w[i] == "--git-dir" || \
              w[i] == "--work-tree" || w[i] == "--namespace" || w[i] == "--exec-path") {
            if (i + 1 <= n) buf[++c] = w[i + 1]                  # option takes a value
            i += 2
          } else {
            i += 1
          }
        } else {
          if (w[i] != "commit") return 0                         # first subcommand
          for (k = 1; k <= c; k++) print "opt\t" buf[k]
          return 1
        }
      }
      return 0
    }
    # Track `cd <dir>` so `cd repo && git commit` resolves to repo.
    function track_cd(n, w,    i, arg) {
      i = first_word(n, w)
      if (i > n || w[i] != "cd") return
      i++
      while (i <= n && w[i] ~ /^-/) i++                          # cd -L / -P
      if (i > n) return
      arg = w[i]
      if (arg ~ /^\//) cd_target = arg
      else if (cd_target != "") cd_target = cd_target "/" arg
      else cd_target = arg
    }
    {
      gsub(/&&|\|\||[;&|(){}]/, "\n", $0)                        # operators/groups -> segment breaks
      m = split($0, lines, /\n/)
      for (li = 1; li <= m; li++) {
        n = split(lines[li], words, /[ \t]+/)
        c = 0
        for (k = 1; k <= n; k++) if (words[k] != "") w[++c] = words[k]
        if (!found && scan(c, w)) {
          found = 1
          if (cd_target != "") print "cd\t" cd_target
        }
        if (!found) track_cd(c, w)
        delete w
      }
    }
    END { exit(found ? 0 : 1) }
  '
}

context="$(git_commit_context "$cmd")" || exit 0

cwd="$(json_field cwd)"
[ -z "$cwd" ] && cwd="$PWD"

# --- Where would git itself run? -------------------------------------------
# Replay the command's `cd` and its git global options, then let git
# answer. `rev-parse` handles the cases we should not re-implement:
# `--git-dir`/`--work-tree` overrides, linked worktrees, submodules, and
# `-C` values that are relative to one another.
base="$cwd"
git_opts=()
while IFS=$'\t' read -r kind value; do
  case "$kind" in
    cd)
      case "$value" in
        /*) base="$value" ;;
        *)  base="$cwd/$value" ;;
      esac
      ;;
    opt) git_opts+=("$value") ;;
  esac
done <<< "$context"

[ -d "$base" ] || base="$cwd"

# `${a[@]+"${a[@]}"}` rather than `"${a[@]}"`: macOS still ships bash 3.2,
# where expanding an empty array under `set -u` is an unbound-variable error.
toplevel="$(cd "$base" 2>/dev/null && git ${git_opts[@]+"${git_opts[@]}"} rev-parse --show-toplevel 2>/dev/null)"
prefix="$(cd "$base" 2>/dev/null && git ${git_opts[@]+"${git_opts[@]}"} rev-parse --show-prefix 2>/dev/null)"

# No work tree (bare repo, not a repo at all, git missing) — nothing to
# anchor a manifest to. Let the commit through; it will fail on its own
# terms if it was going to.
[ -z "$toplevel" ] && exit 0

# --- Locate the MUSTS.yml governing that repository ------------------------
# Walk up from the directory the commit runs in, stopping at the
# repository root. Never above it: a musts workspace that happens to sit
# above an unrelated checkout does not govern that checkout's commits.
found=""
dir="$toplevel${prefix:+/${prefix%/}}"
while :; do
  if [ -f "$dir/MUSTS.yml" ]; then
    found="$dir"
    break
  fi
  [ "$dir" = "$toplevel" ] && break
  parent="$(dirname "$dir")"
  [ "$parent" = "$dir" ] && break
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
