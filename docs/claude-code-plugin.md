# Claude Code plugin

The `musts` repo ships a Claude Code plugin that bundles the [`musts` skill](../skills/musts/SKILL.md) **and** a `Stop` hook. Once installed, every time Claude finishes a turn inside a repo with a `MUSTS.yml`, the hook runs `musts validate` and — if the loop is not clean — re-injects the task list so Claude has to address it before stopping.

This is stricter than the skill alone: the skill depends on the model recognising the right moment to invoke it; the hook fires on every Stop.

## Prerequisites

The plugin shells out to the `musts` binary. Install it first:

```bash
brew install bitomule/tap/musts        # macOS / Linux Homebrew tap
# or
cargo install musts --locked
# or grab a prebuilt binary from https://github.com/bitomule/musts/releases
```

The hook itself is a small POSIX shell script — no extra runtime is required beyond `bash` and `sed`.

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

## What the Stop hook does

On every Stop event, the hook:

1. Bails immediately if it was triggered by a previous hook block (avoids loops).
2. Walks up from Claude's working directory looking for `MUSTS.yml`. **Exits silently** if there is none — the plugin is invisible in non-`musts` repos.
3. Verifies `musts` is on `PATH`. If it isn't, prints a one-line install hint and exits 0 (does not block Stop).
4. Runs `musts validate` from the repo root.
5. **If clean** (exit 0): allows Stop.
6. **If pending** (exit 1): writes the validate report to stderr and exits 2, which is the Claude Code contract for *"feed this back to the model and don't let it stop yet."*

## Silencing one turn

If you legitimately want Claude to stop while there are pending tasks (rare — usually the right move is to address them), the simplest path is to acknowledge the report in your next message and let Claude resolve them. There is no opt-out flag yet; if the hook turns out to be too aggressive in practice, we'll add one.

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
