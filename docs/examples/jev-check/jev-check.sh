#!/usr/bin/env bash
# `uses: jev` — a judgment check that a machine can run.
#
# Design constraints, each one measured rather than assumed (see DESIGN.md):
#   1. The state carries the ARTEFACT plus FACTS the code already resolved.
#      jev is never asked to follow a pointer; the `facts` program follows it.
#   2. No narration. Nothing this script sends describes what any previous step
#      concluded. A single unverified sentence moved a probability from 0.06 to
#      0.74 in measurement, so the state has no field a caller could put one in.
#   3. Exactly one source per question, so two fields can never contradict.
#   4. Default thresholds. Never a hand-tuned cut.
#   5. Three outcomes. `unsure` neither passes nor fails: it degrades to an
#      agent task. It must never become green.
set -euo pipefail

mode="${1:-}"
request="$(cat)"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

die() { printf '%s\n' "$*" >&2; exit 2; }
command -v jq >/dev/null || die "jev-check needs jq on PATH"

# A judgment check that calls the network must never be load-bearing. It is not
# for CI and it degrades on its own: with no key, no network, jevi missing, or
# CI set, it SKIPS — it does not fail, does not hang, and says out loud that
# nothing was evaluated. Silence from this check is never evidence.
jev_unavailable() {
  [ -n "${CI:-}" ]              && { echo "CI is set"; return 0; }
  [ -n "${JEVI_DISABLE:-}" ]    && { echo "JEVI_DISABLE is set"; return 0; }
  command -v jevi >/dev/null    || { echo "jevi is not on PATH"; return 0; }
  return 1
}

case "$mode" in

resolve)
  # One task per check. `command` is what makes this a judgment check that
  # `musts run` can execute — the field musts documents as None for agent/mav.
  jq -n --argjson req "$request" --arg here "$here" '
    {
      protocol_version: 1,
      tasks: ($req.checks | map({
        id: ("jev-" + (.id | gsub("[^A-Za-z0-9]+"; "-"))),
        extension: $req.capability,
        title: ("Judge: " + (.with.ask // "?") + " (" + .local_id + ")"),
        satisfies: [.id],
        parallelizable: true,
        command: [($here + "/jev-check.sh"), "run", .id],
        instructions: (if (.with.mode // "gate") == "shadow" then [
          ("Run `musts run jev-" + (.id | gsub("[^A-Za-z0-9]+"; "-")) + "`. It is in SHADOW mode: it records verdicts and grants nothing."),
          "It cannot fail and its green means only that the rows were recorded."
        ] else [
          ("Run `musts run jev-" + (.id | gsub("[^A-Za-z0-9]+"; "-")) + "`."),
          "It asks jev one typed question per file in scope and records the verdicts.",
          "If it reports UNSURE, jev declined to answer: judge those files yourself and submit evidence with `musts evidence`. Never pass an unsure file without looking at it."
        ] end),
        evidence_contract: {
          text:   { required: true, description: "One line per judged file: verdict and probability." },
          assets: [ { kind: "log", required: true } ]
        }
      })),
      ignored_checks: [],
      notes: ["uses: jev records a measured verdict, not an attestation. The log asset carries every probability and the model id that produced it."]
    }'
  ;;

run)
  # Executed by `musts run <task-id>`, NOT by the agent. Exit 0 pass, 1 fail,
  # 3 unsure-needs-a-human. Never 2 (musts reads 2 as a protocol error).
  check_id="${2:?run needs a check id}"
  root="$(jq -r '.workspace_root' <<<"$request")"
  spec="$(jq -r --arg id "$check_id" '.checks[] | select(.id==$id) | .with' <<<"$request")"
  [ "$spec" = "null" ] && die "no check $check_id in request"

  qfile="$root/.musts/extensions/jev/$(jq -r '.questions' <<<"$spec")"
  ask="$(jq -r '.ask'    <<<"$spec")"
  expect="$(jq -r '.expect // "yes"' <<<"$spec")"
  cmode="$(jq -r '.mode // "gate"' <<<"$spec")"
  factsbin="$(jq -r '.facts // empty' <<<"$spec")"

  if reason="$(jev_unavailable)"; then
    printf 'SKIPPED: %s. Nothing was judged; this check proves nothing about this change.\n' "$reason"
    exit 0
  fi

  pass=0; fail=0; unsure=0; skipped=0
  while IFS= read -r f; do
    [ -z "$f" ] && continue

    # The artefact, and only the artefact.
    state="$(jq -n --arg path "$f" --rawfile src "$root/$f" '{path:$path, source:$src}')"

    # Facts the code resolved on jev's behalf. Namespaced under `facts` so a
    # fact can never collide with — and therefore never contradict — the artefact.
    if [ -n "$factsbin" ]; then
      computed="$("$root/.musts/extensions/jev/$factsbin" "$root/$f")" || die "facts program failed on $f"
      jq -e 'type == "object"' >/dev/null <<<"$computed" || die "facts program must print a JSON object ($f)"
      state="$(jq -n --argjson s "$state" --argjson c "$computed" '$s + {facts: $c}')"
    fi

    # --soft so a no-key / no-network / rate-limited answer comes back as
    # exit 0 with ok:false instead of killing the run. Read `ok`, never $?.
    out="$(printf '%s' "$state" | jevi ask -f "$qfile" --state-json --json --soft || true)"
    if [ "$(jq -r '.ok // false' <<<"$out")" != "true" ]; then
      skipped=$((skipped+1))
      printf 'SKIP   %-60s %s\n' "$f" "$(jq -r '.error.kind // "unknown"' <<<"$out")"
      continue
    fi
    v="$(jq -r --arg a "$ask" '.answers[$a].verdict // "unsure"' <<<"$out")"
    p="$(jq -r --arg a "$ask" '.answers[$a].p // 0'              <<<"$out")"
    m="$(jq -r '.model // "?"' <<<"$out")"

    if [ "$cmode" = "shadow" ]; then
      mkdir -p "$root/.musts/jev-shadow"
      jq -c -n --arg f "$f" --arg ask "$ask" --arg v "$v" --arg p "$p" --arg m "$m" \
              --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson st "$state" \
        '{ts:$ts, file:$f, question:$ask, verdict:$v, p:($p|tonumber), model:$m, facts:($st.facts // null)}' \
        >> "$root/.musts/jev-shadow/$ask.jsonl"
    fi
    case "$v" in
      unsure) unsure=$((unsure+1)); printf 'UNSURE %-60s p=%s %s\n' "$f" "$p" "$m" ;;
      "$expect") pass=$((pass+1));  printf 'ok     %-60s p=%s %s\n' "$f" "$p" "$m" ;;
      *) fail=$((fail+1));          printf 'FAIL   %-60s p=%s %s\n' "$f" "$p" "$m" ;;
    esac
  done < <(jq -r '.changed_files[]' <<<"$request")

  printf '\n%d ok, %d failed, %d unsure, %d not evaluated\n' "$pass" "$fail" "$unsure" "$skipped"
  if [ "$pass" -eq 0 ] && [ "$fail" -eq 0 ] && [ "$unsure" -eq 0 ]; then
    printf 'Nothing was judged. This check proves nothing about this change.\n'
    exit 0
  fi
  if [ "$cmode" = "shadow" ]; then
    printf 'SHADOW: recorded to .musts/jev-shadow/%s.jsonl. Nothing granted, nothing blocked.\n' "$ask"
    exit 0
  fi
  [ "$fail" -gt 0 ] && exit 1
  # A GATE grants green, so an unanswered file must not pass: exit 3, escalate.
  # A TRIPWIRE only ever fires, so unsure is silence — but its silence proves
  # nothing, which is why a tripwire may never be the only check on a risk.
  if [ "$cmode" = "gate" ] && [ "$unsure" -gt 0 ]; then exit 3; fi
  exit 0
  ;;

evidence)
  # A run that ended in `unsure` cannot be waved through with a sentence: the
  # log must show that a human or agent looked at each unsure file.
  log="$(jq -r '.submission.assets[0].path // empty' <<<"$request")"
  root="$(jq -r '.workspace_root' <<<"$request")"
  if [ -z "$log" ]; then
    jq -n '{protocol_version:1, accepted:false,
            missing:[{kind:"log", message:"Attach the jev-check output. A summary sentence is not evidence — the log carries the probabilities and the model id."}],
            message:"Evidence is incomplete."}'
    exit 0
  fi
  if grep -q '^UNSURE' "$root/$log" 2>/dev/null && ! jq -e '.submission.text | test("unsure"; "i")' >/dev/null <<<"$request"; then
    jq -n '{protocol_version:1, accepted:false,
            missing:[{kind:"log", message:"The run reported UNSURE files. Say in the evidence text what you found when you looked at each of them."}],
            message:"Unsure verdicts cannot be accepted silently."}'
    exit 0
  fi
  jq -n --argjson req "$request" '{protocol_version:1, accepted:true,
      satisfies: $req.task.satisfies,
      summary: "jev verdicts recorded.",
      normalized_assets: [{kind:"log", path: $req.submission.assets[0].path}]}'
  ;;

*) die "usage: jev-check.sh {resolve|run <check-id>|evidence}" ;;
esac
