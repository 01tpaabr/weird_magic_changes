---
name: world-tuning
description: Make a world, a pack or a kind behave a certain way by measuring, not guessing — balance populations ("foxes hunt more chickens, but chickens never die out"), hold a rate or a coverage, fix a crash or a boom, find out why something dies. Turns the request into scenario expectations, explains the current dynamics with `wmc observe`, predicts before changing, sweeps consts across seeds with `wmc lab`, confirms the margin, and leaves the goal behind as a test. Use for any request about how a world behaves over time, whatever its kinds.
---

# world-tuning

Reading a rules file tells you what one actor does in one think. It does not tell you what
ten thousand of them do over sixteen days: that is a stochastic dynamical system, and
tweaking numbers until a run looks right is what failed here before (hours of runs, results
fitted to one seed, nothing understood). Work like a lab instead: state the goal as
properties the scenario checks, measure why the world does what it does now, predict what a
change will do, test the prediction on many seeds at once, explain every miss, and leave the
goal behind as a test.

References, read when the step needs them:
- `references/arithmetic.md`: ticks, cadence, chances and lifespans as per-day rates; food
  and energy budgets; how to reason about an equilibrium. Read before predicting anything.
- `references/cookbook.md`: properties to mechanisms to rules idioms to the number that
  should move (satiation, prey switching, territory, refuges, density-dependent births,
  delays, regrowth, timing).
- `references/lessons.md`: what this project already learned, dated. Read first; add to it
  last.
- `references/notebook.md`: the notebook template.
- The language itself: `docs/RULES.md` (§2 how a think runs, §14 scenarios and goals, §17
  the tools).

## The tools, by zoom level

| zoom | tool | answers |
|---|---|---|
| one actor, one think | `wmc why <dir> <x> <y> [ticks [w h seed]]`; the inspector in `wmc play` (click an actor) | which rules it checks, which fires, what it decides |
| one mechanism | a probe: a tiny drawn-map scenario with `run`/`expect` lines | one rate in isolation (a fox in a pen with N hens: kills a day) |
| the world over time | `wmc observe <time> --scenario f [--kind K] [--csv f]` | alive over time; births and deaths a day by cause; needs; states; what thinks decide; which rules fire |
| the goal | `wmc scenario f` (`seeds N`, `pass M`, `expect always\|never\|eventually\|mean ... from T to T`) | does it hold, on how many seeds, and what each expectation saw |
| the knobs | `wmc lab f --set K=a,b,c [--set ...] [--seeds N] [--full] [--csv f]` | which values hold the goal, each expectation's spread across seeds |
| the rules | `wmc lint <packs> --scenario f --strict` | names, unreachable rules, unused needs, wrong layers |

Speed: build once with `cargo build --profile fast -p app --bin wmc` (about 3 minutes) and
run `target/fast/wmc`: a lab runs about twice as fast as under `make run` (12 day-long runs
of a 192x192 world: 3.9 s against 6.5 s on 8 cores), and it runs on its own. Rebuild after changing
Rust, not after changing rules: but the built-in `rules/` are compiled into the binary, so a
goal file must name its packs with a `rules` line (`rules ../../rules`, relative to the
file) for edits to take effect without a rebuild. Any command takes `--set NAME=V` to try a
const without editing.

## The protocol

Keep the notebook (`tuning/<goal>/notes.md`, template in `references/notebook.md`) as you go:
every step below writes in it. `tuning/<goal>/runs/` holds CSVs and is not committed.

1. **The goal as properties.** Write `tuning/<goal>/goal.scenario`: the world (`rules`,
   `seed`, `size`, terrain or a map, `start` lines), `seeds 8` and `pass 7` (or what the
   user's tolerance says), `run T`, and one `expect` per property. Every vague word becomes
   a number over a window: "hunt more" is `expect mean eaten chicken by fox per day >= X
   from 2d` with X from the baseline; "never die out" is `expect always count chicken >= N
   from 2d`; "stable" is a lowest and a highest (`always >=`, `always <=`); "most of the
   meadow" is `mean count grass >= K`. Windows start after the start-up transient. Write a
   fast tier beside it, `goal-fast.scenario`: the same goal on a smaller world or a shorter
   run, for iterating; the full one decides.
2. **The baseline.** `wmc scenario goal.scenario`: which properties fail now, by how much.
   `wmc observe <T> --scenario goal.scenario --csv runs/baseline.csv`: why. Write the
   dynamics story: per kind, births and deaths a day by cause, what its thinks mostly
   decide and which rules fire, and what limits it (food, water, predation, age, room,
   a gate in a rule). Turn the rule numbers into rates with `references/arithmetic.md` and
   check them against what observe measured: where they disagree, you misread a rule.
3. **Check in with the user, once:** the goal file (are these the numbers they meant?), the
   baseline story, and the mechanism you mean to try first. After their go-ahead, work on
   your own within the budget (below).
4. **Predict, then run.** Before each experiment, write the change, the mechanism (from
   `references/cookbook.md`), and for every expectation the direction and rough size its
   number should move. Smallest change first. Put a number you want to vary in a `const`
   and sweep it: `wmc lab goal-fast.scenario --set K=a,b,c --csv runs/NN.csv`. A change of
   rule structure (a new gate, a new rule) is allowed; it needs the prediction like any
   other, and goes in the report.
5. **Explain every miss before the next change.** Compare the lab's numbers with the
   prediction. If they disagree, find out why first: observe the variant (`--set K=v`),
   probe the mechanism alone, or follow one actor doing the surprising thing (`wmc why`, the
   inspector). Update the story. Never two unexplained tweaks in a row: a miss you don't
   understand is the most useful thing in the notebook.
6. **Confirm.** The full goal file on more seeds (`--seeds 16`), and the margin: each knob
   you changed at about -20%, as chosen, and +20% in one lab (`--set K=0.8v,v,1.2v`). A goal
   that holds only at the exact value is fragile: say so.
7. **Lock it in.** Write the chosen values into the rules. Put the goal (or a fast version
   that still fails if the behaviour regresses) in `scenarios/tests/<name>.scenario`, so
   `make test` keeps it true at 1 and 8 threads. `wmc lint --strict`, `make ci`, commit with
   the lab table in the message.
8. **Report, and keep the lesson.** What changed, why it works (the mechanism, in two
   sentences), the evidence (the lab's rows), the margin, what is still fragile. Add a dated
   entry to `references/lessons.md`: the mechanism and the numbers, so the next goal starts
   from it.

**Budget.** Unless the user sets one: 20 lab sweeps or 2 hours, whichever comes first. Then
report what is known, met or not: the best variant, what limits it, what you would try
next.

## Rules of thumb

- Measure before changing, predict before running, explain before changing again.
- Same seed, same world: compare variants on the same seeds, and use several. A difference
  on one seed is real for that seed and nothing more.
- Small worlds lie: a few dozen individuals die out by chance, and edges matter. Iterate
  small, decide big.
- A rule fires (thinks a day) x (the chance no rule above it acted first) x (the chance its
  condition holds). Order is a knob; so is cadence.
- Family matching: `nearest chicken` and `count chicken` see chicks too. A count that gates
  births usually wants `only` (a fox litter counting chicks once crashed the hens).
- A trapped think idles. If observe prints `TRAPS`, fix that before tuning anything.
- One mechanism at a time, and no blind search over many knobs: a value found by luck on
  eight seeds breaks on the ninth.
- Prefer a mechanism that makes the property hold for a reason (satiation, a refuge, a
  density gate) over a number that happens to land in the window.
