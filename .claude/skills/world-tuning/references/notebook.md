# The notebook

One per goal: `tuning/<goal>/notes.md`, beside `goal.scenario`, `goal-fast.scenario` and
`runs/` (CSVs, not committed). Write it as you go, not at the end: it is how a session
resumes, how the user audits the work, and where the lessons come from. Template:

```markdown
# <goal, in a few words>

**Asked:** "<the user's words>" (<date>)
**Goal:** `tuning/<goal>/goal.scenario`; fast tier `goal-fast.scenario` (<size, run>)
**Budget:** <20 labs or 2 hours, or what was agreed>

## Properties

| expect line | why this number |
|---|---|
| `expect always count chicken >= 200 from 2d` | "never die out": the baseline's lowest is 260 |

## Baseline (<date>)

`wmc scenario goal.scenario`: <which fail, and what they saw>
`wmc observe <T> --scenario goal.scenario`, per kind: births and deaths a day by cause,
what its thinks decide, the rules that fire, what limits it.

## The story

Who limits whom, in a few sentences, with the arithmetic (`references/arithmetic.md`):
<e.g. "kills a day = foxes x about one hen each (energy budget); foxes are capped by the
territory gate at about one per 33x33 cells; so kills a day are capped by area">.

## Experiments

### 1. <mechanism> (<date>)
- **Change:** <rules or `--set` values>
- **Prediction:** <for each expect line: direction and rough size, and why>
- **Run:** `wmc lab goal-fast.scenario --set ... --csv runs/01.csv`
- **Result:** <the lab's rows that matter>
- **Explained:** <why it matched; or what the miss showed, and how you found out>

## Decision

<the chosen values; the full goal's result on more seeds; the margin at -20% and +20%;
what is still fragile>

## Lessons

<what goes into `references/lessons.md`, with its numbers>
```
