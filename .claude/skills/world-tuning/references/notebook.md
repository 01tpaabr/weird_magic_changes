# The notebook

One per goal: `tuning/<goal>/notes.md`, beside `goal.scenario`, `goal-fast.scenario` and
`runs/` (CSVs, not committed). Write it as you go, not at the end: it is how a session
resumes, how the user audits the work, and where the lessons come from. Say what you
counted and what you inferred: "inferred from the kill rate and the food medians, not
counted bite by bite". Template:

```markdown
# <goal, in a few words>

**Asked:** "<the user's words>" (<date>), in <which world: packs, scenario, size>
**Goal:** `tuning/<goal>/goal.scenario` (<size, run, seeds, pass>); fast tier
`goal-fast.scenario` (<size, run>, why it is harsher, what it still shows)
**Budget:** <20 labs or 2 hours, or what was agreed>
**Cost:** <seconds a lab variant takes, full and fast; so a sweep of 3 is about N minutes>

## Properties

| expect line | why this number |
|---|---|
| `expect always count K >= N from T` | "never die out": half the baseline's usual lowest (L), after the start-up wave |
| `expect mean avg NAME of K >= X from T` | "<the user's word>": above the baseline's best seed (B) |

## Baseline (<date>)

Rules as written (`hash ...`). Every line measured with thresholds loosened and `wmc lab
--full` (`runs/baseline.csv`); founders per seed from a one-tick lab.

| seed | founders (per kind) | <each property's number> |
|---|---|---|

`wmc observe <T> --scenario ... --gates` on <seeds that differ: median, worst, best>, per
kind: births and deaths a day per head by cause; who kills or feeds whom (per head); what
its thinks decide; which gate binds each rule that matters; what limits it. `wmc rates`
against what observe measured: <agrees / where it does not, and why>.

## The story

Who limits whom, in a few sentences, with the arithmetic (`references/arithmetic.md`) and
the measured numbers: <e.g. "takes a day = consumers x ~3 each (the cap); consumers are
few because their birth gate needs 7 times the density they hold their resource at">.

## Experiments

### 1. <mechanism> (<date>)
- **Change:** <rules, or `--set` values>
- **Mechanism:** <from the cookbook, in a sentence>
- **Prediction:** <for each expect line: direction and rough size, and the arithmetic>
- **Run:** `wmc lab goal-fast.scenario --set ... --csv runs/01.csv`
- **Result:** <the lab's rows that matter>
- **Explained:** <why it matched; or what the miss showed, and how you found out>

## Decision

<the chosen values; the full goal's result on more seeds; the margin at -20% and +20%;
what is still fragile>

## Lessons

<what goes into `references/lessons.md`, with its numbers>
```
