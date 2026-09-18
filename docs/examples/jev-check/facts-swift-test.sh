#!/usr/bin/env bash
# Resolves, in code, the one thing jev cannot: how many elements each loop that
# encloses an assertion actually yields. Measured: without this field jev scored
# 0/10 on hollow tests and abstained 10/10; with it, 10/10 correct.
set -euo pipefail
f="${1:?file}"
python3 - "$f" <<'PY'
import re,sys,json,os
src=open(sys.argv[1],errors="ignore").read()
loops=[]
for m in re.finditer(r'for\s+\S+\s+in\s+([^\{]+)\{', src):
    expr=m.group(1).strip()
    tail=src[m.end():m.end()+1200]
    if not re.search(r'XCTAssert|#expect', tail.split("\n\n")[0] if "\n\n" in tail else tail):
        continue
    # Follow the pointer the way jev cannot: a bare identifier is traced back
    # to its `let` in the same file before we try to resolve it.
    seen=set(); e=expr
    while re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', e) and e not in seen:
        seen.add(e)
        a=re.search(r'\blet\s+'+re.escape(e)+r'\s*(?::[^=]+)?=\s*([^\n]+)', src)
        if not a: break
        e=a.group(1).strip()
    count=None
    if re.match(r'^\[\s*\](\s*as\s*\[[^\]]+\])?$', e) or re.match(r'^\[[A-Za-z0-9_]+\]\(\)$', e):
        count=0
    else:
        g=re.search(r'subpaths\(atPath:\s*(.+?)\)\s*\?\?', e)
        if g and "#filePath" in g.group(1):
            # under bazel #filePath resolves inside the runner's container,
            # where only test sources are mounted: this yields zero files.
            count=0
    resolved_from = e if e!=expr else None
    entry={"expression":expr,"encloses_assertions":True,
           "resolved_element_count":count,
           "resolved_under":"test runner bundle (bazel)"}
    if resolved_from: entry["expression_resolves_to"]=resolved_from
    loops.append(entry)
print(json.dumps({"loops_enclosing_assertions":loops}))
PY
