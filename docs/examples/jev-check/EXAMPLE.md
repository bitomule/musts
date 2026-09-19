# A worked example: does this translation say what the original says?

```yaml
checks:
  translation-says-the-same:
    uses: jev
    paths: ["**/*.xcstrings"]
    with:
      ask: says_the_same
      expect: "yes"
      mode: tripwire
      question:
        type: noul
        instructions: "The translation states the same thing as the source text."
        criteria:
          "true": "A user reading the translation would understand the same instruction or label as a user reading the source"
          "false": "The translation says something different, names a different action, or refers to something the source does not"
```

Quote the `criteria` keys. Bare `true:` and `false:` are YAML booleans, not strings; musts
refuses that loudly rather than quietly, but it is easier not to write it.

## Why this one, and what makes a judgment check worth having

Four tests. A question that fails any of them produces a check people turn off.

**1. It fires before the damage is written.** This is the criterion, not a nice-to-have: the
check runs on the string you just touched, while you can still fix it. A judgment check that
only speaks up once the mistake is in the tree is a report, not a guard. It is the test most
candidate questions fail, and it is worth applying first because it rules them out for free.

**2. What it watches changes constantly.** Catalogues get edited every day — one measurement
here counted 6,119 string values changed in thirty days. A check aimed at something that
changes once in a file's lifetime spends its life green by inertia, and the one day it goes
red the damage is already committed.

**3. The regression is ordinary, not exotic.** It happened while this was being written: a
button reading *Set Reminder* in English and *Guardar la hora* — "save the time" — in Spanish
and German. Two different actions, shipped.

**4. No script can do it.** Comparing what two sentences mean, in two languages, has no
deterministic version. If a grep can catch your case, write the grep: jev never replaces a
script, only a model.

## Measured

Real en/es pairs from a shipping catalogue, with the wrong answers made mechanically by
pairing a string with another key's translation:

| arm | correct | **wrong** | abstained |
|---|---|---|---|
| 28 pairs, half crossed | 26 | **0** | 2 |
| the hard case: donor sharing words with the right answer | 12/14 detected | **0** | 2 |

**Abstention on the healthy half: 1 of 14, 7%.** That number is the one to copy when judging
your own question. The first question tried on this repo abstained on 47% of healthy files —
same model, same day, different wording. A high abstention rate on the healthy case does not
mean the model is weak, it means the question is asking for something it cannot see; and
under two states it costs nothing, so the number is a signal about your wording rather than a
bill.

## The other example, and its one job: why this is not a `bash/check`

`questions/xcstrings-dumper.json` asks whether a script rewrites a whole String Catalogue by
serialising it from memory — a real defect that reorders 13,000 lines and silently drops
duplicate keys.

It is **not** the example to copy: it watches a property that changes once in a script's life,
and when it fires, the script is already written. It is kept for one thing, which it does
better than anything else here: it shows why a grep is not enough. Over nine real files, the
deterministic version — grep for `json.dump` near `xcstrings` — produced two false positives
that jev did not repeat:

| file | grep | jev | truth |
|---|---|---|---|
| a generator | dumper | **abstains** (0.65) | its `json.dumps` writes a *layout* file |
| a checker | dumper | **no** (0.02) | `json.dump` appears inside a comment warning about the hazard |

A check that cries wolf twice out of nine on real code is not a check anyone keeps.
