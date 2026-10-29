# The arithmetic of a world

Rules are written per think; goals are about days. Convert before you predict. The numbers
below are the engine's (`sim_core::time`, `docs/RULES.md` §2, §3, §8, §13). `wmc rates
[packs] --kind K` does this for every rule of a kind (thinks a day, how long needs last,
odds a think and times a day, days left past an age); the reasoning below is how to read
it and what it cannot know (how often a rule is reached, how often its gates hold).

## Time

| unit | ticks |
|---|---|
| a game minute | 15 |
| a game hour | 900 |
| a game day | 21 600 |

A world starts at day 0, 06:00. The sun is up from 06:00 to 18:00, ramping over an hour at
each end: `light` is 0 at night and 255 at full day; `hour` is 0 to 23. At 1x speed the game
runs 8 ticks a second, so a game day is 45 real minutes. Time literals in rules and
scenarios are ticks: `30min` is 450, `2h` is 1800, `1d` is 21 600.

## Thinks a day

An actor thinks every `cadence` ticks (a power of two), plus once after it was hurt or had
something taken. Thinks a day, `T = 21 600 / cadence`:

| cadence | 1 | 2 | 4 | 8 | 16 | 32 | 64 | 128 | 256 | 512 | 1024 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| T | 21 600 | 10 800 | 5 400 | 2 700 | 1 350 | 675 | 337.5 | 168.8 | 84.4 | 42.2 | 21.1 |

## How often a rule fires

A think scans the rules top to bottom and stops at the first that acts (`docs/RULES.md` §2).
So a rule fires about

    T x P(reached: no rule above it acted) x P(its condition holds)

times a day. Its dice are only part of it: a `chance(2)` rule under a rule that acts on 90%
of thinks fires a tenth as often as its dice say. `wmc observe` measures the last two
factors for you: the share of sampled thinks that fire each rule (`fires`).

Per-think odds to per-day odds, for a rule reached at every think:
- `chance(p)` holds with probability `p / 100`; `rand(n) < k` with `k / n`.
- Expected firings a day: `T x p`.
- The chance it fires at least once in a day: `1 - (1 - p)^T`, about `1 - e^(-T p)`.

Examples from the built-in rules:
- A hen lays with `rand(1000) < 2` by day at cadence 4: 2 700 daylight thinks, so
  `1 - 0.998^2700`: 99.5% on a day she qualifies. What really limits eggs is the rest of the
  condition: `day + 1 > last_egg` (at most one every 3 days), food, water, feed around her,
  and `count meat within 6 < 3` (room to nest).
- A hive spawns with `chance(5)` at cadence 64: up to `337.5 x 0.05 = 17` a day when its
  other gates (`nectar > 1d`, `count bee within 8 < 10`) hold; those gates decide.

## Lifespans: `mortal(after, odds)`

Past age `after`, each think is the last with `odds` chances in 100 000. With `q = odds /
100 000`, the daily hazard is `h = q x T`; the life left past `after` averages `1 / h` days,
and half of a cohort is gone `0.69 / h` days past `after` (if the `mortal` rule is reached at
every think: the built-in kinds put `inherit mortal` first).

| kind | `mortal` | cadence | h a day | mean life past `after` |
|---|---|---|---|---|
| chicken | `(5d, 6)` | 4 | 0.32 | 3.1 days |
| fox | `(6d, 3)` | 2 | 0.32 | 3.1 days |
| bee | `(3d, 40)` | 2 | 4.3 | 5.6 hours |
| flower | `(4d, 300)` | 64 | 1.0 | 1 day |

## Needs

`need X max M` holds 0 to `M`. With the default `decay 1` it loses one a tick: `M` is the
time it lasts, full to empty (`max 1d` lasts a day). `decay 0` is points that only rules
and actions change (`health`). A `vital` need at 0 kills at the next think: observe counts
it as `no X`. A decaying vital need whose `max` is at most the cadence empties between two
thinks (the lint warns).

`drink` fills `water` to its max. `eat`, `hit`, `graze` take `bite` (default 1) from the
victim's `health`; `eat` and `graze` give the eater `food x taken / max health` of the
victim kind's `food`; at `health` 0 the victim dies (observe: `eaten by K`).

## Energy budgets

A need that decays one a tick burns its own max a day per "1d": an eater with `need food
max 1d` spends one day of food every day. So a predator that eats only when hungry kills
about

    (food it burns a day) / (food a kill gives)

prey a day, whatever its hunting skill: skill decides how hungry it gets first, not how much
it eats. A fox (`food max 1d`, eats below `20h`) eats a hen (`food 1d`) a day, or three
chicks (`8h`), or six eggs (`4h`). Total predation is then about (predators) x (that rate):
to take more prey a day, change the number of predators, or what a kill is worth, or how
much of their day they spend full (the satiation gate), not just how far they see.

A grazer the same way: a hen burns `1d` of food a day; a tuft of grass (`food 2h`, `health
4`, a hen bites 1) gives 30 minutes a bite, so a hen needs about 48 bites a day; a seed
(`food 3h`, `health 1`) gives 3 hours at once.

## Equilibrium, in a few lines

For each population write, per individual and per day, births `b` and deaths `d` by cause,
as functions of the others (observe gives today's values). It grows while `b > d`.
- Prey dying of predation: `d_pred = kills per predator a day x predators / prey`. More
  prey, same predators: each hen's risk falls. That is what lets prey recover.
- Predators: births need prey (a gate like `count only chicken within 16 >= 15`); deaths
  come from hunger when kills fall below the budget.
- A gate on births that counts its own kind (`count fox within 16 < 2`, `count bee within 8
  < 10`) caps a population per area: the cap is roughly (area / gate area) x the gate's
  number. `count K within r` counts the square of side `2r + 1` around the actor, its own
  cell included: a fox counts itself, so `< 2` means no other fox within 16.
- Delays (an egg takes 8h to hatch and 2 days to a hen; a kit every 5 days) make a
  population overshoot before the brake bites: booms and busts. See the cookbook.

## Searches and fuel

`count`, `nearest`, `sniff` and `for each` within `r` cost `(2r + 1)^2 / 8` fuel beyond their
op (radius 8: 36; radius 16: 136). A think has `fuel` (512 by default, at most 4096): out of
fuel, it traps and idles (observe: `TRAPS`). `sight` (at most 16) caps every radius.
Conditions short-circuit left to right: dice first, searches last.

## Scent

`mark CH v` adds to a cell's channel; every 16 ticks each scented cell loses about 1/32 of
it: a full mark is gone in about 1.5 game hours.

## What the tools measure, and how often

- `wmc observe`: a sample every `--every` (1h); per kind, counts and needs over all the
  alive, and decisions and rules from dry runs of up to `--sample` (100) of them.
- Goals (`expect always|never|eventually|mean ...`): checks at the window's start, its end,
  and every whole minute between; a rate per day reads its counter at the two ends.
- `wmc lab`: a run stops at its first sure failure unless `--full`; its later expectations
  read `not reached` and have no number.
