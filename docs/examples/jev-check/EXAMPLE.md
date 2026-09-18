# A worked example: a script that rewrites a String Catalogue

```yaml
checks:
  xcstrings-dumper:
    uses: jev
    paths: ["tools/scripts/**/*.py", "scripts/**/*.py"]
    with:
      questions: questions/xcstrings-dumper.json
      ask: rewrites_catalogue
      expect: "no"
      mode: shadow
```

## The defect

Reading a `.xcstrings` into a dict and writing the whole thing back reorders and reformats a
13,000-line file. The diff is unreadable, so a real change hides inside it — and worse, a bare
`json.load` / `json.dump` round trip **drops duplicated keys silently**. That is not
hypothetical: `Nokoru/tools/scripts/check_xcstrings.py` says in its own header that it cost
Nokoru 467 lines and sent two agents chasing the wrong culprit.

## Why this is not a `bash/check`

This is the argument the capability lives or dies on, so it is written out rather than
asserted. The deterministic version — grep for `json.dump` near `xcstrings` — is **worse on
our real code**. Over nine real files it produced two false positives that jev did not repeat:

| file | grep | jev | truth |
|---|---|---|---|
| `koubou/src/koubou/generator.py` | dumper | **abstains** (0.65) | not a dumper: its `json.dumps` writes a *layout* file |
| `Nokoru/tools/scripts/check_xcstrings.py` | dumper | **no** (0.02) | not a dumper: `json.dump` appears inside a comment warning about the hazard |

A check that cries wolf twice out of nine on real code is not a check people keep. If your
grep is *not* worse than the model on your own code, write the grep: jev never replaces a
script, only a model.

## Measured

- 10 constructed scripts in the real shapes: **10/10, zero abstentions.**
- 9 real files across Boxy, Nokoru, HiddenFace, koubou and Undolly: **8 correct, 1 abstention,
  0 wrong.**

## What it found on the first run — these are real, and not part of the example

Four scripts in this organisation rewrite a catalogue wholesale today:

- `Boxy/tools/scripts/add_strings.py`
- `Boxy/tools/scripts/remove_strings.py`
- `Nokoru/tools/scripts/dedupe_xcstrings.py`
- `HiddenFace/tools/scripts/rescue_referenced_strings.py`

(`koubou/src/koubou/localization.py` also does, and there it may well be intended — it is a
generator, not a repo utility.)

## The shape that worked, and the one that did not

It works because the answer is **visible in the text**: a `json.dump` of a parsed catalogue is
there to be read. Questions that ask whether two things *mean* the same abstain instead — see
the dead end recorded in `DESIGN.md`.
