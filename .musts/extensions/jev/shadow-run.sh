#!/usr/bin/env bash
# Runs the hollow-test question over this repo's test sources and records one row
# per file. Deliberately NOT a check in MUSTS.yml: a pending judgment task blocks
# every commit, and `musts run` refuses external extensions on purpose
# (crates/musts-core/src/run.rs), so wiring it in would produce exactly the
# expensive manual agent check this capability exists to avoid.
#
#   .musts/extensions/jev/shadow-run.sh            # judge changed files (vs origin/main)
#   .musts/extensions/jev/shadow-run.sh --all      # judge every test source
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
here="$root/.musts/extensions/jev"
if [ "${1:-}" = "--all" ]; then
  files="$(cd "$root" && git ls-files 'crates/**/*.rs' 'tests/**/*.rs')"
else
  files="$(cd "$root" && git diff --name-only origin/main...HEAD -- 'crates/**/*.rs' 'tests/**/*.rs')"
fi
[ -z "$files" ] && { echo "nothing to judge"; exit 0; }
printf '%s\n' "$files" | python3 -c "
import sys,json
print(json.dumps({'protocol_version':1,'workspace_root':'$root','capability':'jev',
 'changed_files':[l.strip() for l in sys.stdin if l.strip()],
 'checks':[{'id':'shadow/hollow','local_id':'hollow','manifest_path':'MUSTS.yml','scope_path':'','depth':0,
   'with':{'mode':'shadow','questions':'questions/hollow-rust-test.json','ask':'hollow',
           'expect':'no','facts':'facts-rust-test.sh'}}],
 'snapshot':{'handle':'x','dirty_scopes':[]}}))" \
  | "$root/docs/examples/jev-check/jev-check.sh" run shadow/hollow
