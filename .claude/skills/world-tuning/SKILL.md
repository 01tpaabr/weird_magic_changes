---
name: world-tuning
description: Make a world, a pack or a kind behave a certain way by measuring, not guessing, whatever the world is about: an ecosystem ("foxes hunt more chickens, but chickens never die out"), a cellular automaton ("a Life variant that settles at a fifth alive"), a society or an economy ("nobody falls below 100 wealth, most of the village at work"), an epidemic, a market. Models the dynamics from the rules first (`wmc rates`), turns the request into scenario expectations, measures the baseline (`wmc observe`, `--gates` for the gate that binds each rule), predicts before every change, sweeps consts across seeds with `wmc lab`, confirms the margin, and leaves the goal behind as a test. Use for any request about how a world behaves over time.
---

# world-tuning

A rules file says what one actor does in one think. A goal is about what thousands of them
do over days or generations: a stochastic dynamical system. Tweaking numbers until a run
looks right is what failed here before (hours of runs, results fitted to one seed, nothing
understood). Work like a lab instead. Model the dynamics from the rules. State the goal as
properties the scenario checks. Measure why the world does what it does now. Predict what a
change will do, test the prediction on many seeds at once, explain every miss, and leave the
goal behind as a test.

Nothing here is about one kind of world. Kinds, needs, states, rules and counters are the
whole vocabulary: a fox's `food`, a villager's `wealth` and a Life cell's neighbours are
all needs, counts and gates to the tools. The examples come from three packs on purpose:
the ecosystem in `rules/`, Conway's Life in `packs/life`, the village in `packs/village`.

References, read when the step needs them:
- `references/arithmetic.md`: ticks and generations, chances and lifespans per day,
  transfers and caps, densities and gates, cohorts. Read before predicting anything.
- `references/cookbook.md`: properties to mechanisms to rules idioms to the number that
  should move, for ecosystems, societies, automata and contagion; worked examples.
- `references/lessons.md`: what this project already learned, dated, with numbers. Read
  first; add to it last.
- `references/notebook.md`: the notebook template.
- The language itself: `docs/RULES.md` (§2 how a think runs, §14 scenarios and goals, §17
  the tools).

## Think first, run to check

The rules are a program, and you can read programs. Most of the answer is in them:
- `wmc rates <packs>` turns every rule into day units: thinks a day, how long each need
  lasts, each rule's dice a think and a day, lifespans past an age.
- Write the balance: per kind, births and deaths per head a day by cause, as functions of
  the others (`arithmetic.md`). Where does each rate come from: a die, a gate, a cap?
- Predict the baseline from that before running anything. Then run to calibrate: where
  the numbers disagree, a gate holds less often than you thought, or a cap clips a
  transfer, or the founders started somewhere else. Find which. That is the model
  improving.

Simulation calibrates and confirms the model; it is not a search. A lab is for
discriminating between hypotheses you wrote down, not for sampling the space blind.

## The tools, by zoom level

| zoom | tool | answers |
|---|---|---|
| the rules, in day units | `wmc rates [packs] [--kind K]` | thinks a day, needs' durations, each rule's odds a think and times a day, lifespans |
| one actor, one think | `wmc why <dir> <x> <y> [ticks] --scenario f`; the inspector in `wmc play` (click an actor) | which rules it checks, which fires, under each rule that did not the term that was false (`false: food < 20h`), what it decides |
| one mechanism | a probe: a tiny drawn-map scenario with `run`/`expect` lines | one rate in isolation (a fox penned with N hens: kills a day; two villagers: one gift) |
| the world over time | `wmc observe <time> --scenario f [--kind K] [--gates] [--csv f]` | alive over time; births and deaths a day by cause and per head; who kills whom (per killer and per head); needs; states; decisions; rules that fire |
| why a rule (doesn't) fire | `wmc observe ... --gates` | per rule: thinks that reached it and fired it; per term of its condition, how many of the thinks that got that far it stopped (the binding gate stops most), with a position and time to `wmc why` |
| the goal | `wmc scenario f` (`seeds N`, `pass M`, `expect always\|never\|eventually\|mean ... from T to T`) | does it hold, on how many seeds, what each expectation saw; seeds run in parallel |
| the knobs | `wmc lab f --set K=a,b,c [--set ...] [--seeds N] [--full] [--csv f]` | which values hold the goal; each expectation's spread across seeds (median: mean of the middle two for an even count) |
| the rules | `wmc lint <packs> --scenario f --strict` | names, unreachable rules, unused needs, wrong layers |

Goal metrics work for any world: `count [only] K [in STATE]`, `min|max|sum|avg NAME of
[only] K [in STATE]` (a need or a memory), the counters `born|became|eaten|died|thinks|
traps [only] K` (with `eaten K by K2`, `died K of NEED`), each at a moment, over a window
(`always`, `never`, `eventually`), as a `mean`, or a counter `per day`.

## Cost and speed

- Build once: `cargo build --profile fast -p app --bin wmc` (about 3 minutes from cold, 10
  seconds after a small change), then run `target/fast/wmc`: about twice as fast as `make
  run`. Rebuild after changing Rust, not rules: but the built-in `rules/` are compiled in,
  so a goal file names its packs with a `rules` line (`rules ../../rules`, relative to the
  file) and edits take effect without a rebuild. Any command takes `--set NAME=V`.
- Know the price before choosing a size. Day one is the cheapest day: populations grow, and
  a 16-day 192x192 ecosystem run costs 2 to 4 times as much a day as its first day. Time one
  `wmc scenario` of the goal first. Measured: the fox goal (256x256, 16 days) is 158 s a lab
  variant (8 seeds at once, 8 cores), its 192x192 tier 96 s; 600 Life generations on
  128x128 (16 384 actors thinking every tick) about 2 s a seed; a 2-day village (64x64,
  about 120 villagers) a few hundredths of a second.
- A lab spends little on what fails: a run stops at its first sure failure, and a variant
  stops once more seeds failed than `pass` allows (a fox lab of 3 hopeless variants x 8
  seeds: 176 s down to 45 s). `--full` runs everything, for numbers rather than verdicts.
- One run is one world on one thread, `--jobs` at a time (default: every core).

## The protocol

Keep the notebook (`tuning/<goal>/notes.md`, template in `references/notebook.md`) as you go:
every step below writes in it. `tuning/<goal>/runs/` holds CSVs and is not committed.

1. **The goal as properties.** Write `tuning/<goal>/goal.scenario`: the world (`rules`,
   `seed`, `size`, terrain or a map, `start` lines), `seeds 8` and `pass 7` (or what the
   user's tolerance says), `run T`, and one `expect` per property. Every vague word becomes
   a number over a window:

   | asked | expect |
   |---|---|
   | "foxes hunt more" | `expect mean eaten chicken by fox per day >= X from 2d` |
   | "chickens never die out" | `expect always count chicken >= N from 2d` |
   | "settles at a fifth alive" (Life, 16384 cells) | `expect mean count alive >= 3000 from 300` and `<= 3600` |
   | "nobody poor" | `expect always min wealth of villager >= 100 from 1d` |
   | "most at work" | `expect always count villager in WORK >= 80 from 6h` |
   | "the epidemic burns out" | `expect eventually count infected == 0 from 5d` |
   | "stable" | a lowest and a highest (`always >=`, `always <=`) |

   A request for change ("more", "faster") takes its number from the baseline: outside the
   baseline's range across seeds (above its best seed), or noise passes it. A request that
   comes with a floor ("but never die out") needs that floor as its own line, with a margin
   under it (half the baseline's usual lowest, say), and often a floor for the actor of the
   change too (the hunters must not die out either). Windows start after the start-up
   transient: founders all born at once age, tire and die in step (`arithmetic.md`,
   cohorts). Count `only` what you mean: `eaten chicken` counts chicks too.
   Write a fast tier beside it, `goal-fast.scenario`, for iterating: a smaller world or a
   shorter run, each threshold taken from its own baseline by the same rule as the full
   one. Prefer a tier that is harsher than the full goal (a variant that passes it passes
   the full one), and never one too short to show what the goal turns on (the founders
   ageing out, the second generation). The full goal decides.
2. **The baseline.** Measure before judging: a copy of the goal with every threshold
   loosened to always hold (`>= 0`, `<= 1000000`), run with `wmc lab copy.scenario --full
   --csv runs/baseline.csv`, reports every expectation's number on every seed. Count the
   founders per seed too: `start K 1 / 5000` places a different number on each seed, and
   ratios between kinds vary more (a lab of `run 1` and `expect count K >= 0` lines lists
   them). Then `wmc observe <T> --scenario ... --csv runs/obs.csv --gates` on two or three
   seeds that differ (the median, the worst, the best). Write the story: per kind, births
   and deaths a day per head by cause, who kills or feeds whom (per head), what its thinks
   decide, which gate binds each rule that matters, what limits it (a resource, a cap, a
   gate, age, room, a transient). Check the rule numbers (`wmc rates`) against what observe
   measured: where they disagree you misread a rule. Mark what you inferred and what you
   counted.
3. **Check in with the user, once:** the goal file (are these the numbers they meant?), the
   baseline story, and the mechanism you mean to try first. After their go-ahead, work on
   your own within the budget (below).
4. **Predict, then run.** Before each experiment, write the change, the mechanism (from
   `references/cookbook.md`), and for every expectation the direction and rough size its
   number should move, with the arithmetic. Smallest change first. Put a number you want to
   vary in a `const` and sweep it: `wmc lab goal-fast.scenario --set K=a,b,c --csv
   runs/NN.csv`. A change of rule structure (a new gate, a new rule) is allowed; it needs
   the prediction like any other, and goes in the report.
5. **Explain every miss before the next change.** Compare the lab's numbers with the
   prediction. If they disagree, find out why first: observe the variant (`--set K=v
   --gates`), probe the mechanism alone, or follow an actor doing the surprising thing
   (`wmc why` at a position `--gates` printed, the inspector). Update the story. Never two
   unexplained tweaks in a row: a miss you don't understand is the most useful thing in the
   notebook.
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

- Model, predict, measure, explain; then change. Never the reverse.
- A rule fires (thinks a day) x (the share of thinks that reach it: no rule above acted) x
  (the share whose condition holds). `--gates` measures the last two and says which term
  stops the rest. Relax the gate that binds, not the one you suspect.
- A capped need clips transfers: a consumer that acts below `low` with a max `max` gains at
  most `max - low` an act, whatever the act offers. Acts a day follow from that.
- Same seed, same world: compare variants on the same seeds, and use several. A difference
  on one seed is real for that seed and nothing more. Founders vary by seed: count them.
- Founders born together move together: they tire, breed and die in step until something
  spreads them. Stagger them at birth, or start windows after the wave.
- Small worlds lie: a few dozen individuals die out by chance, and edges matter. Iterate
  small, decide big.
- Look at every cause of death before naming the limiter: thirst killed as many chickens
  as the foxes did.
- Family matching: `nearest chicken`, `count chicken` and `eaten chicken` include chicks.
  A count that gates births usually wants `only`.
- observe samples (an hour apart, 100 thinks a kind): what lasts less than a sample, or
  is rare, can be missed; the goal's checks run every game minute. Raise `--sample`, shorten
  `--every`, or write the property as an expectation and let the scenario watch it.
- A trapped think idles. If observe prints `TRAPS`, fix that before tuning anything.
- One mechanism at a time, and no blind search over many knobs: a value found by luck on
  eight seeds breaks on the ninth.
- Prefer a mechanism that makes the property hold for a reason (satiation, a refuge, a
  density gate, staggering) over a number that happens to land in the window.
