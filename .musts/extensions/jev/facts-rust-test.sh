#!/usr/bin/env bash
# Resolves in code what a judgment cannot see: whether each assertion in a #[test]
# is actually reached, and whether it compares an expression against itself.
set -euo pipefail
python3 - "${1:?file}" <<'PY'
import re,sys,json
src=open(sys.argv[1],errors="ignore").read()
src_nc=re.sub(r'//[^\n]*','',src)
fns=[]
for m in re.finditer(r'#\[(?:tokio::)?test\][^\n]*\n\s*(?:async\s+)?fn\s+([A-Za-z0-9_]+)', src_nc):
    i=src_nc.index("{", m.end()); d=0
    for j in range(i,len(src_nc)):
        if src_nc[j]=="{": d+=1
        elif src_nc[j]=="}":
            d-=1
            if d==0: break
    body=src_nc[i:j+1]
    asserts=re.findall(r'\bassert(?:_eq|_ne)?!\s*\(([^;]*)\)\s*;', body)
    # In Rust a panic IS a failed assertion: unwrap/expect/? end the test red.
    panics=len(re.findall(r'\.unwrap\(\)|\.expect\(|\bpanic!|\?\s*;', body))
    # Fluent assertion builders (assert_cmd, predicates) never say "assert!".
    fluent=len(re.findall(r'\.assert\(\)|\.success\(\)|\.failure\(\)|\.stdout\(|\.stderr\(|\.code\(', body))
    loops=[]
    for lm in re.finditer(r'\bfor\s+\S+\s+in\s+([^\{]+)\{', body):
        e=lm.group(1).strip(); tail=body[lm.end():lm.end()+600]
        if not re.search(r'\bassert', tail): continue
        cnt=None
        # follow a bare identifier back to its `let`
        seen=set()
        while re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', e) and e not in seen:
            seen.add(e)
            a=re.search(r'\blet\s+(?:mut\s+)?'+re.escape(e)+r'\s*(?::[^=]+)?=\s*([^;\n]+)', body)
            if not a: break
            e=a.group(1).strip()
        if re.match(r'^(vec!\s*\[\s*\]|Vec::new\(\)|\[\s*\]|&\[\s*\])', e): cnt=0
        loops.append({"expression":lm.group(1).strip(),"resolved_element_count":cnt,
                      **({"expression_resolves_to":e} if e!=lm.group(1).strip() else {})})
    taut=0
    for a in asserts:
        parts=[p.strip() for p in a.split(",")]
        if len(parts)>=2 and parts[0]==parts[1]: taut+=1
        if len(parts)==1 and parts[0] in ("true","1 == 1"): taut+=1
    fns.append({"test_name":m.group(1),"assertion_count":len(asserts)+panics+fluent,
                "explicit_assertions":len(asserts),"panicking_calls":panics,
                "fluent_assertions":fluent,
                "self_comparing_assertions":taut,
                "loops_enclosing_assertions":loops})
print(json.dumps({"tests":fns}))
PY
