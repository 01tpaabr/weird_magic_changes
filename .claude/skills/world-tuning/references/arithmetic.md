# The arithmetic of a world

Rules are written per think; goals are about days or generations. Convert before you
predict. The numbers below are the engine's (`sim_core::time`, `docs/RULES.md` §2, §3, §8,
§13). `wmc rates [packs] --kind K` does the first conversions for every rule of a kind
(thinks a day, how long needs last, odds a think and times a day, days left past an age);
`wmc observe --gates` measures what rates cannot know: how often a rule is reached and
which term of its condition stops it.

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

A world whose kinds think every tick (a cellular automaton: `cadence 1`) steps one
generation a tick. Think in ticks there: windows in ticks (`from 300`), and a counter `per
day` is per 21 600 generations (divide to get per generation). The clock still prints game
time: generation 600 reads as day 0 06:40.

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
of thinks fires a tenth as often as its dice say. `wmc observe --gates` measures both
factors: `reached` is the first, and per term of the condition, `stopped n of m` is how many
of the thinks that got that far it stopped. A condition is short-circuited left to right,
so the terms read as a funnel: 4540 reach the rule, 6% stop at `wealth >= RICH`, half of
the rest find no neighbour, 94% of the rest see no poorer one: 54 fire.

Per-think odds to per-day odds, for a rule reached at every think:
- `chance(p)` holds with probability `p / 100`; `rand(n) < k` with `k / n`.
- Expected firings a day: `T x p`.
- The chance it fires at least once in a day: `1 - (1 - p)^T`, about `1 - e^(-T p)`.

Examples:
- A hen lays with `rand(1000) < 2` by day at cadence 4: 2 700 daylight thinks, so
  `1 - 0.998^2700`: 99.5% on a day she qualifies. What really limits eggs is the rest of the
  condition: `day + 1 > last_egg` (at most one every 3 days), food, water, feed around her,
  and `count meat within 6 < 3` (room to nest).
- A villager earns `WAGE` (2) with `chance(80)` at cadence 64, on the 93% of its 338 thinks
  a day it spends at work, and pays `COST` (1) at every think: wealth drifts by `338 x (0.93
  x 0.8 x 2 - 1)`, about +165 a day, before gifts. Measured: the mean rose from about 40 to
  288 by day 1.5.

## Lifespans: `mortal(after, odds)`

Past age `after`, each think is the last with `odds` chances in 100 000. With `q = odds /
100 000`, the daily hazard is `h = q x T`; the life left past `after` averages `1 / h` days,
and half of a cohort is gone `0.69 / h` days past `after` (if the rule is reached at every
think: the built-in kinds put `inherit mortal` first).

| kind | `mortal` | cadence | h a day | mean life past `after` |
|---|---|---|---|---|
| chicken | `(5d, 6)` | 4 | 0.32 | 3.1 days |
| fox | `(6d, 3)` | 2 | 0.32 | 3.1 days |
| bee | `(3d, 40)` | 2 | 4.3 | 5.6 hours |
| flower | `(4d, 300)` | 64 | 1.0 | 1 day |

## Needs

`need X max M` holds 0 to `M`. With the default `decay 1` it loses one a tick: `M` is the
time it lasts, full to empty (`max 1d` lasts a day). `decay 0` is points that only rules
and actions change (`health`, `wealth`, a crop of nectar). A `vital` need at 0 kills at the
next think: observe counts it as `no X`. A decaying vital need whose `max` is at most the
cadence empties between two thinks (the lint warns). A newborn or a founder starts with
every need full: a kind that should start elsewhere sets it at its first think (`when born
== 0 => { born = 1  wealth = 20 + rand(40) }`).

## Transfers, and the cap that clips them

Every way a need grows stops at its max:
- `eat`, `hit`, `graze` take `bite` (default 1) from the victim's `health`; `eat` and
  `graze` give the eater `food x taken / max health` of the victim kind's `food`, clamped at
  the eater's max; at `health` 0 the victim dies (observe: `eaten by K`).
- `take t NEED n` and `give t NEED n` move up to `n`, never past the receiver's max, never
  more than the giver has.
- A rule's own write (`energy += 1h`) is clamped to `0..max` too.

So a consumer that acts only below a threshold `low` absorbs at most `max - low` (plus
what it burnt since its last think) per act, whatever one act offers. Its acts a day are
about

    (what it burns a day) / (max - low)

not burn / (what a victim is worth). A fox (`food max 1d`, eats below `20h`) gains at most
about 4 hours a meal: 5 or 6 meals a day, a hen's 12-hour bite mostly lost. Kills are the
acts that finish a victim: a victim of `health H` bitten `b` at a time dies on bite
`ceil(H / b)` (a hen, health 20, to a fox's bite 10: two bites; a chick or an egg: one), so
kills a day = acts a day x the share of acts that kill. Measured: ~3 kills a fox-day (2.6 ..
3.6 on 8 seeds), the rest first bites on hens. To change how much a consumer takes, move
`low` (hungrier more of the day: more, smaller acts), `max`, or what one act takes (`bite`);
sight and speed only change how soon it eats, not how much.

A producer the same way: a hen burns `1d` of food a day; a tuft of grass (`food 2h`, `health
4`, a hen bites 1) gives 30 minutes a bite, so a hen needs about 48 bites a day; a seed
(`food 3h`, `health 1`) gives 3 hours at once. A villager resting gains `1h` of energy a
think, less the 64 ticks it burns meanwhile, from 6h to 22h: about 17 thinks of 64 ticks,
70 minutes of rest for every 16 hours of work.

## Per head, and equilibrium

For each population write, per individual and per day, births `b` and deaths `d` by cause,
as functions of the others; observe prints today's (`per head`, and `kills a day ... per
head`). It grows while `b > d`.
- A victim's predation hazard: `kills a day per killer x killers / victims`. More victims,
  same killers: each one's risk falls. That is what lets them recover, if the killers do
  not grow with them at once.
- Killers' births need victims (a gate like `count only chicken within 16 >= 15`); their
  deaths come from hunger when acts fall below the burn, and from age.
- A flock yields a surplus per head a day (births that grow up minus other deaths); it holds
  while killers x kills per killer stays under it. The fox goal's flocks yielded about 0.2 ..
  0.3 chickens per hen a day, so a flock held while foxes stayed under about 7 .. 10 per
  100 hens; the founders' ratio decided crash or boom.

## Densities and gates

`count K within r` counts the square of side `2r + 1` around the actor, its own cell
included: a fox counts itself, so `count fox within 16 < 2` means no other fox within 16.
The square holds `(2r + 1)^2` cells (radius 16: 1 089, of which the walkable share). Its
expected count is the density of K times that. So a gate `count K within r >= n` opens only
where K's local density is at least `n / area`: compare that with the world's mean. The fox
litter gate (15 hens within 16, ~885 walkable cells) needed about 7 times the starting
density of hens: where the foxes held the flock thinner, they never bred. A gate `< n`
caps a population at about (area / gate area) x n: territory.

## Cohorts and lockstep

Founders are all born at tick 0, with the same needs (full) and the same age. They do
everything age-gated together (a litter `age > 2d`, a hen `age > 2d`), tire together, and
die together: `mortal(6d, 3)` foxes went 11, 6, 2, 1 over days 6 to 9. The village's
villagers, all full at tick 0, all rested at once (none at work for about 70 minutes, 18
hours in and every ~17 hours after) until each started with `energy = 12h + rand(12h)`. Births spread the ages out over
generations; until then, a window that starts before the first wave has passed measures the
founders, not the world. Stagger founders at birth when lockstep is not the point.

Shares in `start` lines (`start fox 1 / 5000`) place a different number on each seed: 7 to
14 foxes on 256x256 in the fox goal, on 117 to 147 hens. The ratios between kinds vary more
than the counts. Count them per seed before blaming a rule.

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
  alive, and decisions, rules and gates from dry runs of up to `--sample` (100) of them.
  An hour between samples misses what lasts less (the village resting all at once for an
  hour); a rare rule needs more samples (`stopped n of m` with `m` small is a guess).
- Goals (`expect always|never|eventually|mean ...`): checks at the window's start, its end,
  and every whole minute between; a rate per day reads its counter at the two ends.
- `wmc lab`: a run stops at its first sure failure unless `--full`; its later expectations
  read `not reached` and have no number, and the number of the one that failed is where it
  stopped (`(59)` for `always count chicken >= 60`), not its lowest. A variant gives up once
  it cannot pass. For numbers, loosen every threshold and run `--full`.
