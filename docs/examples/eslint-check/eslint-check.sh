#!/usr/bin/env bash
#
# Minimal harness extension implementing `eslint/check` in ~30 lines
# of bash + jq. Demonstrates that an extension is "any executable" —
# no Rust, no compiled binary, no harness-extension-util.
#
# Dependencies: jq.
#
# Wire-up: drop this file at
# `<workspace>/.harness/extensions/eslint/eslint-check.sh`
# (chmod +x) plus the `extension.yml` next to it.

set -euo pipefail
mode="${1:-}"
request=$(cat)

case "$mode" in
  resolve)
    jq -n --argjson req "$request" '{
      protocol_version: 1,
      tasks: [{
        id: "eslint-root",
        extension: $req.capability,
        title: "Run eslint over changed files",
        satisfies: [$req.checks[].id],
        parallelizable: false,
        instructions: [
          "Run `npx eslint .` from the workspace root.",
          "Capture stdout/stderr as a log asset and record evidence with:",
          "  harness evidence eslint-root --text \"<summary>\" --asset <log>"
        ],
        evidence_contract: {
          text: { required: true,
                  description: "State whether eslint exited 0 and list any remaining warnings." },
          assets: [{ kind: "log", required: true }]
        }
      }],
      ignored_checks: [],
      notes: []
    }'
    ;;

  evidence)
    jq -n --argjson req "$request" '
      ($req.submission.text // "") as $text
      | [$req.submission.assets[]
         | select(.mime | startswith("text/") or . == "application/octet-stream")] as $logs
      | ($text | length) as $text_len
      | ($logs | length) as $log_count
      | ((if $text_len == 0
          then [{kind:"text", message:"Provide a text summary."}]
          else [] end)
         + (if $log_count == 0
            then [{kind:"log", message:"Attach the eslint output as a log asset."}]
            else [] end)) as $missing
      | if ($missing | length) > 0
        then {
          protocol_version: 1, accepted: false, satisfies: [],
          missing: $missing,
          message: "Evidence is incomplete."
        }
        else {
          protocol_version: 1, accepted: true,
          satisfies: $req.task.satisfies,
          summary: "eslint evidence accepted"
        }
        end'
    ;;

  *)
    echo "eslint-check.sh: expected \`resolve\` or \`evidence\`, got \`$mode\`" >&2
    exit 2
    ;;
esac
