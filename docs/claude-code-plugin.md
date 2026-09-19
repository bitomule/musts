# Claude Code plugin

The `musts` repo ships a Claude Code plugin that bundles the [`musts` skill](../skills/musts/SKILL.md) **and** a `PreToolUse` hook. Once installed, whenever Claude is about to run a `git commit` inside a repo with a `MUSTS.yml`, the hook runs `musts validate` and — if the loop is not clean — blocks the commit and feeds the task list back so Claude has to close the loop first.

This is stricter than the skill alone: the skill depends on the model recognising the right moment to invoke it; the hook enforces the loop at the commit boundary regardless.

### Why the commit boundary and not `Stop`

Earlier versions hooked `Stop` and re-ran `validate` on every turn end. In practice that fired at the wrong moments — while the agent was awaiting background subagents or a workflow, when it stopped to ask a question, or after unrelated non-code work — re-injecting the full task list each time. `git commit` is the agent's explicit "this is done" signal, so validating there enforces the loop exactly once, at the right time, without the per-turn nagging. CI (`musts validate` as a required check) remains the backstop for anything committed by other means.

## Prerequisites

The plugin shells out to the `musts` binary. Install it first:

```bash
brew install bitomule/tap/musts        # macOS / Linux Homebrew tap
# or
cargo install musts --locked
# or grab a prebuilt binary from https://github.com/bitomule/musts/releases
```

The hook itself is a small POSIX shell script. It reads the command being run from the event JSON with `jq` when available, falling back to `python3` and then `sed`, so no hard runtime dependency beyond `bash` is required.

## Install (from the public marketplace)

```text
/plugin marketplace add bitomule/musts
/plugin install musts@musts
```

The first command points Claude Code at the marketplace manifest in this repo (`.claude-plugin/marketplace.json`). The second installs the one plugin it advertises (also named `musts`).

## Install (from a private fork)

The mechanism is identical — only the repo coordinates change. Claude Code shells out to `git`, so any auth that works for `git clone` works here:

```bash
export GITHUB_TOKEN=ghp_xxx…   # PAT with read access to the private fork
```

Then, in Claude Code:

```text
/plugin marketplace add my-org/musts-private
/plugin install musts@musts
```

Alternatives to a PAT: cached `gh auth login` credentials, or an SSH key loaded in `ssh-agent` with the host in `known_hosts`. There is no plugin-side configuration to set.

## What the hook does

On every `PreToolUse` event for the `Bash` tool, the hook:

1. Reads the command being run. **Exits silently (allows the tool call)** unless it is a `git commit` — every non-commit Bash call passes straight through.
2. Walks up from Claude's working directory looking for `MUSTS.yml`. **Exits silently** if there is none — the plugin is invisible in non-`musts` repos.
3. Verifies `musts` is on `PATH`. If it isn't, prints a one-line install hint and exits 0 (does not block the commit).
4. Runs `musts validate` from the repo root.
5. **If clean** (exit 0): allows the commit.
6. **If pending**: writes the validate report to stderr and exits 2, which is the Claude Code contract for *"feed this back to the model and block the tool call."*

The commit matcher recognises `git commit`, `git commit -m …`, `git -C <dir> commit`, and forms joined with `&&` / `;` / `|`. It deliberately does **not** match `git commit-graph`. Uncommon global-flag forms (other than `-C`) may slip past the matcher; CI is the backstop.

## Committing with pending tasks

If you legitimately need to commit while tasks are pending (rare — usually the right move is to close the loop), commit through a path the hook doesn't gate, or resolve the tasks first. There is no opt-out flag; the CI `musts validate` check still enforces the loop on the PR.

## Uninstall

```text
/plugin uninstall musts@musts
/plugin marketplace remove musts        # optional, if you no longer want the marketplace registered
```

This removes the hook entirely; the bundled skill goes away with it. The CLI itself is unaffected (uninstall it with your package manager).

## Updating

`plugin.json` deliberately omits an explicit `version`, so Claude Code tracks the plugin by commit SHA. To pull the latest:

```text
/plugin marketplace update musts
/plugin update musts@musts
```
