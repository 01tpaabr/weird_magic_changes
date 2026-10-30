# Foxes hunt more chickens, but chickens never die out

**Asked:** "foxes hunt more chickens, but chickens never die out" (2026-10-29), in the
built-in world: the kinds in `rules/`, the world of `scenarios/default.scenario` (its
terrain and `start` lines), on a size quick enough to iterate on.
**Goal:** `goal.scenario` (256x256, 16 days, seeds 42..49, pass 7); fast tier
`goal-fast.scenario` (192x192, 16 days, the same seeds and pass).
**Budget:** the skill's default, 20 lab sweeps or 2 hours, from the user's go-ahead.
**Cost:** a lab variant of `goal.scenario` is 158 s (8 worlds at once, `target/fast/wmc`,
8 cores); of `goal-fast.scenario` 96 s. A 3-value sweep: ~8 min full, ~5 min fast, less
when variants stop at their first sure failure. `wmc scenario goal.scenario` runs the
seeds one after another, each on 8 threads (6 min 58 s for the 8): use
`wmc lab goal.scenario --full` instead, the same verdicts in 2.6 min.

## Properties

`chicken` is the family, as in the rules: hens and chicks (eggs are their own kind). Kills
are `eaten chicken by fox`: more than half of them are chicks.

| expect line (`goal.scenario`) | why this number |
|---|---|
| `expect mean eaten chicken by fox per day >= 32 from 2d` | "hunt more": twice the baseline's median, 16.2 a day (13.2 .. 26.6 on the 8 seeds), so above even its best seed |
| `expect mean eaten chicken by fox per day >= 19 from 9d` | "more", and not only while the founding foxes live: they die of age from day 6 (half gone by day 8); twice the baseline's median of 9.4 a day from day 9 (1.9 .. 33.9) |
| `expect always count chicken >= 100 from 2d` | "never die out", with a margin: half the baseline's usual lowest (200: 163 .. 223 on 7 seeds; 0 on seed 43, gone by day 12). More hunting must thin the flock; 100 says never to the brink (the world starts with ~133 hens) |
| `expect always count fox >= 3 from 2d` | the hunters must not die out either, or "hunting more" ends with them: the baseline's lowest is 0 .. 7, median 2.5 (they age out on half the seeds) |

`goal-fast.scenario` takes each number from its own baseline by the same rule: kills
`>= 21 from 2d` (twice 10.4), `>= 9 from 9d` (twice 4.4), `always count chicken >= 60`
(half its usual lowest, 122), `always count fox >= 2` (the full goal's 3 scaled by the
area, 0.56). It is harsher than the full goal: fewer founders, so hens die out on 2 seeds
of 8 (not 1) and foxes on 3; a variant that passes it should pass the full one, not always
the reverse. A 10-day run on 256x256 was the other candidate (81 s) and was rejected: it
stops before the founders age out, so it rewards hungrier foxes and hides whether kits
replace them, which is what this goal turns on.

Windows start at day 2 (the first eggs hatch and grow up from day 1); the late window at
day 9, when half the founding foxes are dead (`mortal(6d, 3)`: past 6 days, 3.1 more on
average).

## Baseline (2026-10-29)

Rules as written (`hash 5d3890132d8077a0`, the built-in set). Measured with
`wmc lab runs/measure-256-16d.scenario --full` (every line passes, so every number is
reported; `runs/baseline-256-16d.csv`) and the same at 192x192 (`runs/baseline-192-16d.csv`);
founders from a one-tick lab (`runs/founders-256.csv`). Per seed, 256x256, 16 days:

| seed | founders fox / hens | kills a day from 2d | from 9d | kills a fox-day | fox births a day | lowest chickens | lowest foxes | mean hens |
|---|---|---|---|---|---|---|---|---|
| 42 | 11 / 133 (8.3%) | 14.6 | 2.7 | 3.6 | 0.06 | 163 | 1 | 229 |
| 43 | 14 / 134 (10.4%) | 21.2 | 1.9 | 3.6 | 0 | **0** | **0** | 30 |
| 44 | 8 / 117 (6.8%) | 13.2 | 7.0 | 3.0 | 0.38 | 170 | 2 | 296 |
| 45 | 8 / 139 (5.8%) | 15.1 | 11.7 | 2.8 | 0.75 | 211 | 3 | 438 |
| 46 | 8 / 134 (6.0%) | 20.1 | 20.0 | 2.9 | 1.12 | 198 | 4 | 387 |
| 47 | 7 / 139 (5.0%) | 16.6 | 19.1 | 2.6 | 1.25 | 218 | 3 | 478 |
| 48 | 7 / 143 (4.9%) | 26.6 | 33.9 | 2.9 | 1.81 | 223 | 7 | 428 |
| 49 | 10 / 147 (6.8%) | 15.9 | 6.6 | 2.9 | 0.44 | 202 | 2 | 366 |

(kills a fox-day: chickens and eggs eaten by foxes a day over the mean fox count, from
day 2.)

`goal.scenario` on the baseline: **0 of 8 seeds hold every line.** Line by line: kills from
2d 0/8; kills from 9d 3/8 (46, 47, 48); chickens >= 100 7/8 (seed 43 first below 100 at
5d 4h 53min); foxes >= 3 4/8 (42 below at 7d 19h, 43, 44, 49).
`goal-fast.scenario`: 0 of 8; kills from 2d 0/8 (7.9 .. 16.7); from 9d 3/8; chickens >= 60
6/8 (0 on 42 and 43); foxes >= 2 3/8.

`wmc observe 16d --scenario runs/measure-256-16d.scenario` on three seeds that differ
(`runs/obs-256-s{42,43,48}.{txt,csv}`): 42 the middle, 43 the crash, 48 the boom.

**Fox** (cadence 2: 10 800 thinks a day; `food max 1d`; hunts below 20h; sleeps 9:00 to
17:00 when fed). Thinks decide `move` 75%, `idle` 25%: patrol (`wander`, 71%), sleep (24%);
the hunting rules (`animals.rules:83/84/86`) fire on under 1% of sampled thinks (0 of 354,
3 of 475, 4 of 698): when it gets hungry prey is close, and a hunt takes minutes. Its
`food` median is 21h43m (p10 21h11m) on seed 42, 21h55m on 48, 19h18m on 43 where hens
got scarce: it spends the day between 20h and its 1d cap. Births: 0.06, 0 and 1.8 a day
on seeds 42, 43, 48. Deaths: old age 79 .. 100%, the rest thirst (42) or hunger (43,
after the hens were gone). Founders die from day 6: on seed 42, 11 foxes at day 6, 6 at
day 7, 2 at day 8, 1 from day 9 to the end. Past age 6, old age took 9 foxes in 15
fox-days on seed 42 and 11 in 28 on seed 43: 0.46 a fox-day pooled against `rates`'
0.324 (20 deaths where 14 were expected: within chance at these numbers; `rand` draws
are unbiased, `vm.rs` `draw_below`).

**Chickens.** Hens lay at the rule's interval, one egg every 3 days (eggs per hen-day
0.36 .. 0.48, the excess being new hens' first eggs: `last_egg` starts at 0), so the egg
rule's other gates (fed, watered, feed around, fewer than 3 `meat` within 6) rarely bind
at these densities. Eggs hatch at 8h (3 .. 10% eaten by foxes); chicks grow up at 2 days.
Per day over the run:

| seed | eggs laid | chicks dead (foxes / thirst) | hens dead (age / thirst / foxes) |
|---|---|---|---|
| 42 | 85 | 18 (51% / 49%) | 28 (41% / 32% / 26%) |
| 43 | 20 | 12 (88% / 12%) | 15 (3% / 10% / 87%) |
| 48 | 140 | 26 (71% / 29%) | 43 (55% / 27% / 17%) |

Thirst kills 10 .. 21 chickens a day on 7 seeds of 8 (2d .. 16d), about as many as the
foxes do: half the chick deaths on seed 42. Hens' health median is 20, but p10 is 10 .. 13
(15 median on seed 43): many hens walk around after one fox bite.

`wmc rates rules/` agrees with what observe measured where it can say anything: the hen's
egg rule at 0.2% a think (10.8 a day if reached and gated open; the 3-day interval caps it
at 1/3 a day); the fox's litter rule at 2% a think (216 a day if reached), so its dice
never limit it: its gates do (below); the fox's and hen's mortal at 0.324 a day past 6 and
5 days (3.1 days left on average).

## The story

**Each fox kills about 3 chickens and eggs a day** (2.6 .. 3.6 on the 8 seeds, mean 3.0),
whatever a prey is worth. It eats below 20h and its food is capped at 1d (the engine
clamps the gain, `exchange` in `actors/systems.rs`), so it absorbs only ~4h a meal however
big the prey: a hen's 12h bite is mostly lost. So it eats 5 or 6 times a day, of which ~3
kill (a chick or an egg dies of one bite; a hen of two: fox `bite 10`, hen `health 20`;
the first bite feeds the fox and leaves her at 10, and about as many hens are bitten once
as are killed). Inferred from the kill rate and the `food` medians, not counted bite by
bite.

**A flock yields ~0.2 .. 0.3 chickens per hen a day.** Seed 42, days 2 .. 9: the foxes
took 26.6 a day and the hens still went from 107 to 212 (with ~90 chicks): ~0.29 a hen-day
while the flock is young. Days 9 .. 16, with old age at work (hens die of it from day 5):
~0.20 (seed 42, one fox left: the family grew 60 a day on ~320 hens; seed 48: 34 eaten and
84 grown a day on ~600 hens). So a flock holds while the foxes number under ~0.2 .. 0.3 / 3
= 7 .. 10 per 100 hens, and the **founders' ratio decides each world**: 10.4% (seed 43)
and 10 .. 11% (192x192 seeds 42, 43) eat the flock out by day 8 .. 12; 8.3% (seed 42) and
9.1% (192x192 seed 44, down to 70) hold it; 4.9 .. 6.8% let it grow 8 to 11 times by day
16.

**Two feedbacks are missing.**
1. No functional response: foxes kill as many when hens are scarce as when they are
   common. On seed 43 they are hungrier (`food` median 19h18m) and still find the last
   hens (`sight 16`, prey within 8, the chase memory); hens have nowhere to hide. So
   nothing stops a flock that starts shrinking: that is how chickens die out.
2. The numerical response comes too late. A litter needs 15 hens within 16 cells (a 33x33
   square, ~885 walkable cells): ~7 times the starting density (133 hens on ~53 000
   walkable cells: 2.2 a square). Births track the flock: 0 a day at a mean of 30 hens,
   0.06 at 229, 0.4 at 300 .. 370, 0.75 .. 1.8 at 390 .. 480. Where the foxes hold the
   flock below ~400 they never breed and die of age from day 6 (seeds 42, 43, 44, 49: 1 ..
   3.9 foxes on average from day 9), and the second week's hunting fades (1.9 .. 7 kills a
   day from 9d). The territory gate (`count fox within 16 < 2`) is not what holds them:
   ~11 foxes on ~53 000 walkable cells see ~0.2 other foxes in their square on average;
   it can start to bind only late on the boom seeds (seed 48: 26 foxes at day 16). Inferred
   from the per-seed births against the flock's size, not yet probed rule by rule.

**What the goal takes, in numbers.** 32 kills a day is ~11 foxes at ~3 each, sustained to
day 16. At ~0.2 a hen-day, a flock of ~160 hens or more yields that (the baseline's flocks
average 230 .. 480 on 7 seeds: the food is there). Old age takes ~0.11 of the foxes
a day (mean life ~9.1 days), so 11 foxes need ~1.2 kits a day, at a flock of ~200 .. 300
hens: 2 .. 5 hens a square on average, perhaps 5 .. 8 around a fox, which hunts where the
hens are. The litter's prey gate has to open there, not at 15. And the floor needs the
foxes to ease off when hens are scarce, or the founders' ratio still decides some seeds.

## Experiments

### 1. Litters at the flock the foxes keep: the numerical response (proposed, 2026-10-29)
- **Change:** `count only chicken within 16 >= 15` in the fox's litter rule becomes
  `>= LITTER_HENS`, a const (15 as written); sweep 15, 10, 6.
- **Mechanism:** births that need prey (cookbook), at a lower density: foxes breed at the
  flock size they hold, replace the founders as they age, and stop breeding where the
  flock thins.
- **Prediction** (factors against the baseline; the same on both tiers):
  - fox births: median 0.75 a day becomes ~1.5 at 10 and ~2.5 at 6, starting at day 2 .. 3
    (the founders' age gate) instead of day ~5;
  - kills from 2d: x1.2 at 10, x1.6 at 6; fails at both (the founders do most of the first
    week's hunting whatever the gate);
  - kills from 9d: x1.8 at 10, x2.5 at 6; holds on most seeds at 6 where the flock holds;
  - chickens >= 100: seed 43 still dies out at every value (its 14 founders are past the
    ratio before any kit is born); at 6 the extra kits tip 42 (8.3%), maybe 44 and 49
    (6.8%) over too: fails on 2 .. 4 seeds; at 10, on 1 .. 2;
  - foxes >= 3: holds on 6 .. 7 seeds at 6 (no ageing out; fails where the flock crashes
    and the foxes starve), 5 .. 6 at 10;
  - verdict: no value passes; around 6 .. 8 the rates come closest, and the floor is what
    fails: experiment 2's job.
- **Run:** `wmc lab goal-fast.scenario --set LITTER_HENS=15,10,6 --csv runs/01.csv`

**Go-ahead 2026-10-30** (the user: "Yes, go ahead"), budget from 00:33. First counted with
the new `observe --gates` on the baseline (`runs/gates-base-s{42,45,48}.txt`): of the
sampled litter thinks that passed the dice, the prey gate stopped all (19 of 19, 6 of 6,
11 of 11), the territory 2 of 21, 1 of 7, 1 of 12. The inference holds, now counted.
The const at 15 compiles to the same hash (`5d3890132d8077a0`), so 15 is the baseline and
the lab ran 10, 8, 6 instead (`runs/measure-fast.scenario`: the goal's four lines plus
loosened ones; `--full`).

- **Result** (lab 1, `runs/01.csv`, 157 s; medians over the 8 seeds; baseline in brackets):

  | LITTER_HENS | kills 2d (>= 21) | kills 9d (>= 9) | chickens >= 60 | foxes >= 2 | fox births a day | highest foxes | seeds held |
  |---|---|---|---|---|---|---|---|
  | (15) | (10.4) | (4.4) | (6/8) | (3/8) | (0.17) | | 0 |
  | 10 | 18.1, 2/8 | 10.8, 5/8 | 5/8 | 4/8 | 0.44 | 9.5 | 1 |
  | 8 | 16.4, 3/8 | 4.4, 3/8 | 0/8 | 1/8 | 0.47 | 13 | 0 |
  | 6 | 15.1, 2/8 | 0, 2/8 | 0/8 | 0/8 | 0.56 | 16 | 0 |

- **Against the prediction:** births rose as predicted (x2.6 .. x3.3). Everything else
  missed: kills from 2d rose more at 10 (x1.75, predicted x1.2), and below 10 the flock
  died on every seed (predicted 2 .. 4 at 6), and the foxes starved after it (`died fox
  of food` 0.5 .. 1.2 a day at 6), so the late kills fell to 0.
- **Explained** (`wmc observe ... --set LITTER_HENS=8` on seed 45 and `=10` on seed 48,
  `runs/obs-01-*.txt`): the founders turn 2 days old together and breed in lockstep: foxes
  5, 7, 8 on days 2 .. 4, 12 by day 6 (seed 45 at 8); 3, 5, 9, 13 on days 2, 3, 6, 8, 19 by
  day 13 (seed 48 at 10). Each kit kills from its first hours (3.4 and 3.5 a fox-day
  measured), so one or two litters push past the flock's 7 .. 10 foxes per 100 hens: 12 per
  85 on seed 45 at day 6, 19 per 103 on seed 48 at day 13. The gate closes only once the
  flock is thinner than the gate, with 2 .. 6 times the founders alive, and nothing slows
  their killing: the flock is eaten out, the foxes starve. The prediction assumed births
  spread over time and a gate that closes in time; the cohort made them a wave, and the
  missing functional response did the rest. At 10 the territory gate starts to bind at the
  peak (5 of 8 sampled thinks past the dice, seed 48). So: no lower birth gate is safe
  without a brake on killing when hens get scarce. That brake comes first.

### 2. Prey switching, the floor (2026-10-30)
- **Change:** both hunting rules of the fox (eat what is next to it, walk toward meat
  within 8) also need `count only chicken within 8 >= HUNT_FLOCK`, a const (0 = always,
  the rules as written). The adjacent `eat` is gated too: a fox walks ~5 000 .. 8 000
  cells a day, so a fox that ignored scarce hens but still ate the ones it bumped into
  would find most lone hens of a 192x192 world within a day.
- **Mechanism:** switching (a search image): a fox hunts only where a flock is, so a
  thinned flock is out of reach, and foxes in thin country go hungry instead of eating the
  last hens. Hens drift toward each other (within 6) and chicks follow hens, so groups form
  where hens are dense and fall apart where they are thin.
- **Prediction**, in one lab: `HUNT_FLOCK` 2, 3, 4, each at `LITTER_HENS` 15 (switching
  alone) and 10 (with the stronger numerical response). Hens within 8 (~234 walkable
  cells) average 0.6 .. 1.2 at the flocks of the fast tier (~80 .. 150 hens), so 2 is a mild
  gate, 3 a strong one, 4 rare: foxes hunt only in groups.
  - chickens >= 60: at 15, 7 .. 8/8 at 3 and 4 (seeds 42 and 43 no longer eaten out), 6 .. 7
    at 2; at 10, 6/8 at 2, 7 .. 8 at 3 and 4.
  - kills from 2d: lower than without switching, more so as K rises: at 15 about 9, 7, 5
    a day for 2, 3, 4 (10.4 without); at 10 about 15, 12, 9 (18.1 without). Below 21: fails.
  - kills from 9d: at 10, about 10 at 2 and 3 (the flock holds, so do the foxes), less at 4.
  - foxes >= 2: hunger deaths rise with K; at 15 the founders still age out unless the
    flock booms: 2 .. 3/8; at 10, 4 .. 5/8 at 2 and 3, fewer at 4.
  - verdict: the floor holds at 3 or 4, the rates fail everywhere: switching makes the
    floor, and the rates need a third lever (more foxes held in check by switching, or
    hungrier foxes). If instead switching at 3 does not lift the floor on seeds 42 and 43,
    the leak is elsewhere (chases, groups of chicks around a hen): follow a fox with `wmc why`.
- **Run:** `wmc lab runs/measure-fast.scenario --set LITTER_HENS=15,10 --set HUNT_FLOCK=2,3,4
  --full --csv runs/02.csv`
- **Check first:** `HUNT_FLOCK=0` leaves the world bit for bit as the rules were (same
  `state` hash after 3 days on seed 45, against the rules of HEAD as a scratch pack).
- **Result** (lab 2, `runs/02.csv`, 591 s; medians; seeds that held each line):

  | LITTER_HENS, HUNT_FLOCK | kills 2d (>= 21) | kills 9d (>= 9) | chickens >= 60 (lowest) | foxes >= 2 (lowest) | fox births a day | foxes from 9d | kills 2d .. 9d | seeds held |
  |---|---|---|---|---|---|---|---|---|
  | 15, 0 (baseline) | 10.4, 0/8 | 4.4, 3/8 | 6/8 (0 .. 137) | 3/8 | 0.17 | 2.4 | 13.2 | 0 |
  | 15, 2 | 9.8, 1/8 | 5.4, 3/8 | 8/8 (69 .. 156) | 4/8 | 0.31 | 2.7 | 13.2 | 1 |
  | 15, 3 | 10.0, 1/8 | 8.7, 4/8 | 8/8 (101 .. 157) | 4/8 | 0.44 | 3.9 | 12.6 | 1 |
  | 15, 4 | 11.1, 1/8 | 17.3, 7/8 | 8/8 (116 .. 166) | 7/8 | 1.16 | 8.8 | 6.1 | 1 |
  | 10, 2 | 23.3, 5/8 | 26.8, 6/8 | 5/8 (19 .. 124) | 6/8 | 0.78 | 8.8 | 20.4 | 3 |
  | 10, 3 | 25.3, 5/8 | 27.9, 6/8 | 8/8 (70 .. 157) | 6/8 | 1.09 | 10.8 | 19.8 | 5 |
  | **10, 4** | **29.4, 7/8** | **42.2, 8/8** | **8/8 (109 .. 136)** | **8/8 (2 .. 6)** | 2.09 | 18.4 | 13.4 | **7, passes** |

- **Against the prediction:** the floor came as predicted (8/8 at 3 and 4, both gates).
  The rest went the other way: kills and foxes *rose* with `HUNT_FLOCK` (at 10: 23.3,
  25.3, 29.4 a day; foxes from day 9 8.8, 10.8, 18.4), and fox hunger deaths stayed small
  (0 .. 0.38 a day). I had counted the brake on each fox and not what it does to the flock.
- **Explained** (`wmc observe 16d 192 192 42 --set LITTER_HENS=10 --set HUNT_FLOCK=4
  --gates`, `runs/obs-02-L10K4-s42.txt`): the starting flock (79 hens on 192x192) is too
  thin for groups of 4, so the founders can barely hunt: 4 of 9 starve by day 3 (fox
  deaths: 21% `no food`). Relieved, the flock grows from 70 hens on day 2 to 220 on day 9
  and groups up; kits come where it is dense (2.2 a day) and the foxes rise to 26 by day
  16. Each kills 2.47 a day (chicken 14.9, chick 12.9, egg 4.4 a day in all: 32), hungry on
  27% of its thinks (`food` median 20h15m, p10 16h22m), the switching term stopping 98% of
  its hungry thinks. So switching is a brake on each fox and a lift for the flock, and the
  numerical response turns a denser flock into more foxes: total kills go up. The flock
  never went below 109 chickens on any seed: foxes cannot hunt it thinner than groups of 4.
  At 15 the same happens later (the flock must reach ~300 hens before litters), so the
  first week's kills fall to 3 .. 10 a day.
- **Note:** the gain is in the second week. Kills from 2d to 9d stay at the baseline's level
  (13.4 against 13.2): the founders, braked, hunt little; their kits do the hunting.

### 3. Confirm on the full goal, with the margin (2026-10-30)
- **Plan:** `runs/measure-full.scenario` (`goal.scenario`'s four lines plus loosened ones):
  `--set LITTER_HENS=10 --set HUNT_FLOCK=3,4,5`, then `--set LITTER_HENS=8,12 --set
  HUNT_FLOCK=4` (each knob at about -20% and +20%), then `--seeds 16` at 10 and 4.
- **Prediction** at 10 and 4 (the fast tier scaled by the area, x1.78, and more founders):
  kills from 2d ~50 a day (>= 32), from 9d ~70 (>= 19), lowest chickens ~190 .. 240 (>= 100),
  lowest foxes ~4 .. 10 (>= 3): 8/8 or 7/8. At `HUNT_FLOCK=3`: seed 43 (10.4% founders) is
  not braked enough: the floor or the foxes fail there and maybe on 42: 6 .. 7/8. At 5: more
  founders starve, the flock grows more, kits later: early kills down, the fox floor at
  risk (lowest 2 .. 4): passes or fails on that line. `LITTER_HENS=8`: more kits, kills up,
  switching still holds the floor: passes; 12: fewer kits, the kill lines thinner: 6 .. 8/8.

- **Ran 32 days by mistake** (lab 3, `runs/03.csv`, 1497 s): `runs/measure-full.scenario`
  took the goal file's first 19 lines, its `run 16d` among them, and then a second `run
  16d`: the windows were 2d .. 32d and 9d .. 32d. (The fast copy was right: labs 1 and 2
  ran 16 days.) Not the goal, but a long-run test of the gated design, worth keeping:

  | LITTER_HENS, HUNT_FLOCK (32 days) | kills 2d .. 32d | kills 9d .. 32d | chickens >= 100 (lowest) | foxes >= 3 (lowest) | highest foxes | seeds held |
  |---|---|---|---|---|---|---|
  | 10, 3 | 24.7, 1/8 | 22.6, 5/8 | 0/8 (0 .. 30) | 0/8 (0) | | 0 |
  | 10, 4 | 36.7, 5/8 | 38.7, 8/8 | 5/8 (0 .. 239) | 2/8 (0 .. 8) | 34 .. 89 | 2 |
  | 10, 5 | 0, 3/8 | 0, 3/8 | 8/8 (233 .. 284) | 0/8 (0 .. 1) | | 0 |

  At (10, 4) every failure but one came after day 19: foxes rose to 34 .. 89 on 256x256 and
  then fell below 3 between days 19.9 and 26.1 on 5 seeds; the flock fell below 100 at day
  23, 25 and 30 on seeds 42, 46, 43 (to 0 on 42). The one early failure: seed 44's
  founders starved to a single fox by day 2 (it recovered to 89). So the design is a
  boom-bust with a period longer than the goal's 16 days: switching delays the bust, it
  does not prevent it. At 5 the founders starve before any kit: foxes gone on 5 seeds. At
  3 the flock is eaten out. Not yet explained: how seed 42's flock reached 0 with the
  gate at 4.

### 4. Switching on the stalk only (2026-10-30, while lab 3 ran)
- **Why:** gating the adjacent `eat` too means a starving fox next to a lone hen leaves her
  alone. `scenarios/tests/fox_pen.scenario` and `fox_pen_watched.scenario`, the CLI and
  pack tests that run them, unit tests in `scenario.rs` and `checks.rs`, and the executable
  examples of `docs/RULES.md` §14 all have a starving fox eat a single cornered hen. If
  gating the stalk alone holds the goal, none of that changes.
- **Change:** the adjacent rule takes its own const, `EAT_FLOCK` (0: eats what is next to
  it); `HUNT_FLOCK` gates only the walk toward prey within 8.
- **Mechanism:** the same switching, with a leak: a fox still eats what it stumbles on. Hens
  and chicks flee a fox within 5 at half its pace (cadence 4 against 2), roosting or not
  (the flee rule is above the roost); eggs cannot flee.
- **Prediction** at `LITTER_HENS=10`, `HUNT_FLOCK` 4 and 5, `EAT_FLOCK=0`, fast tier,
  against lab 2's gated row (10, 4): fewer founders starve (they eat eggs and the hens they
  meet), so kills from 2d .. 9d rise (13.4 to ~16) and the fox floor improves; the flock's
  lowest falls a little (109 .. 136 to ~90 .. 130), still >= 60 everywhere. Passes 7 .. 8 of
  8 at both. If seeds 42 and 43 lose their floor instead, the leak is large: keep the gated
  eat and rewrite the pen tests and the docs' examples.
- **Run:** `wmc lab runs/measure-fast.scenario --set LITTER_HENS=10 --set HUNT_FLOCK=4,5
  --set EAT_FLOCK=0 --full --csv runs/04.csv`

- **Result** (lab 4, the full goal, 16 days now; `runs/04.csv`, 328 s):

  | full goal, (10, 4) | kills 2d (>= 32) | kills 9d (>= 19) | chickens >= 100 (lowest) | foxes >= 3 (lowest) | fox births a day | foxes from 9d | kills 2d .. 9d | seeds held |
  |---|---|---|---|---|---|---|---|---|
  | stalk only (`EAT_FLOCK=0`) | 42.0, 5/8 | 47.1, 6/8 | 8/8 (127 .. 208) | 7/8 (0 .. 10) | 1.59 | 18.1 | 30.9 | 5 |
  | both gated (`EAT_FLOCK=4`) | **55.3, 7/8** | **80.0, 7/8** | **8/8 (223 .. 259)** | **7/8 (1 .. 10)** | 3.38 | 34.6 | 28.2 | **7, passes** |

- **Against the prediction:** the stalk-only fox ate more in the first week (30.9 against
  28.2) and its floor sat lower (127 .. 208 against 223 .. 259), as predicted; but it
  failed on 3 seeds, by its *late* kills. **Explained** by experiment 2's story: the leak
  keeps the flock thinner (lowest hens 108 against 131), so kits come at half the rate
  (1.59 against 3.38 a day) and the second week has half the foxes (18 against 35). The
  gated design passes the full goal: the first result that does.

### 5. A starving fox takes what is at its feet (2026-10-30)
- **Why:** with both rules gated, a starving fox penned with one cornered hen never eats
  her: `fox_pen.scenario` and `fox_pen_watched.scenario` fail (checked: 3 of 5 and 0 of 4
  seeds), and so would the unit tests in `scenario/checks.rs` and `observe.rs` that build
  that pen from the built-in rules and assert the kill, and the docs' executable examples.
  A fox that starves next to its prey is also odd content.
- **Change:** the adjacent `eat` holds where there is a flock *or* when `food <
  DESPERATE`; the walk toward prey stays gated. At 16h the penned fox (food 2h) bites to
  14h, still below 16h, and takes the second bite: both pen tests pass again, and the
  lone-hen probe still holds (no chase).
- **Prediction** at (10, 4), `DESPERATE=16h`, full goal, against lab 4's gated row: the
  leak opens only after 4 hours of failing to find a flock, so it sits between the two
  rows of lab 4: fewer founders starve (seed 44's fox floor may hold), the flock a little
  thinner, births ~2.5 .. 3 a day, kills from 2d ~50, from 9d ~65; floors 8/8; passes 7 ..
  8/8. If it fails like the stalk-only row, the leak matters more than its 4-hour delay
  suggests, and the gated design stays with the pen tests rewritten.
- **Run:** `wmc lab runs/measure-full.scenario --set LITTER_HENS=10 --set HUNT_FLOCK=4
  --set DESPERATE=16h --full --csv runs/05.csv`
- **Result** (lab 5, `runs/05.csv`, 141 s): **8 of 8 seeds hold every line.** Kills from 2d
  34.1 .. 60.7 (median 51.5), from 9d 38.3 .. 94 (72.3); lowest chickens 188 .. 239 (210);
  lowest foxes 7 .. 14 (8); fox births 2.1 .. 4.4 a day; foxes from day 9 18.5 .. 38.6;
  kills from 2d to 9d 23.6 .. 37 (29.4, against the baseline's ~20). Matched the
  prediction's direction and size, and beat it on the fox floor: no founder starves now
  (seed 44 kept 8 foxes where the gated design left 1), since a starving fox eats the egg
  or hen at its feet. The weakest number: seed 43 (10.4% founders), 34.1 kills a day
  against 32.

### 6 .. 9. Confirm: the margin and 16 seeds (2026-10-30)
Each knob at about -20% and +20% of the chosen value, the others as chosen, on the full
goal (labs 6, 7, 9: `runs/06.csv`, `07.csv`, `09.csv`); 8 more seeds, 50 .. 57 (lab 8,
`runs/measure-full-s50.scenario`, `runs/08.csv`):

| variant (full goal) | seeds held | kills 2d (lowest .. median) | kills 9d | lowest chickens | lowest foxes |
|---|---|---|---|---|---|
| **10, 4, 16h, seeds 42 .. 49** | **8/8** | 34.1 .. 51.5 | 38.3 .. 72.3 | 188 .. 239 | 7 .. 14 |
| **10, 4, 16h, seeds 50 .. 57** | **7/8** | 26.7 (seed 55) .. 41.6 | 25 .. 45.9 | 184 .. 222 | 8 .. 14 |
| `HUNT_FLOCK` 3 | 8/8 | 32.0 .. 44.5 | 19.6 .. 57.2 | 136 .. 216 | 4 .. 11 |
| `HUNT_FLOCK` 5 | 7/8 | 31.3 .. 49.8 | 35.1 .. 70.9 | 143 .. 258 | 7 .. 14 |
| `LITTER_HENS` 8 | **3/8** | 30.4 .. 41.5 | 20.7 .. 47.4 | **5 .. 166 (5 seeds < 100)** | 2 .. 10 |
| `LITTER_HENS` 12 | 7/8 | 20.9 .. 39.9 | 19.6 .. 60.3 | 188 .. 257 | 5 .. 11 |
| `LITTER_HENS` 12, seeds 50 .. 57 | 7/8 | 26.9 .. 44.5 | 31.3 .. 60.5 | 196 .. 268 | 8 .. 14 |
| `DESPERATE` 13h | 8/8 | 34.1 .. 49.8 | 35.6 .. 69.1 | 172 .. 257 | 7 .. 14 |
| `DESPERATE` 19h | 7/8 | 23.1 .. 51.9 | 18.4 .. 71.6 | 122 .. 199 | 6 .. 11 |

- 16 seeds at the chosen values: 15 hold (14 needed at the goal's 7 in 8). Seed 55 fails
  the kill line from day 2 (26.7) at both 10 and 12: a seed where the first week's hunting
  is thin, not a floor problem.
- Robust: `HUNT_FLOCK` 3 .. 5 and `DESPERATE` 13h .. 19h all pass. **Fragile:**
  `LITTER_HENS` toward lower values: at 8 the extra kits push the flock under 100 on 5
  seeds (to 5 on one); at 12 the first week's kills thin out (one seed at 20.9). The goal
  holds from 10 to 12; 10 is at its lower edge, chosen for its kills (15 of 16 seeds
  against 14 at 12).
- At 19h the leak of experiment 4 shows again: one seed fails both kill lines.

### Not done
- **A world-level test in `make test`.** The smallest world where this goal means anything
  (192x192, 16 days) costs ~96 s a seed in the fast build, more in the dev build `make test`
  uses, at 1 and 8 threads; 128x128 for 12 days (28 s a run) has 1 .. 3 foxes, and the old
  and new litter gates gave the same numbers on half its seeds (`runs/10.csv`). So the
  goal stays here (`goal.scenario`: `wmc lab tuning/foxes-hunt-more/goal.scenario`, ~2.5 ..
  6 min), and `scenarios/tests/` gets the mechanism: `fox_lone_hen.scenario` (a starving
  fox leaves a lone hen 6 cells off alone: fails with the old rules) and
  `fox_flock.scenario` (it catches one of a flock of four). Nothing in `make test` fails if
  `LITTER_HENS` goes back to 15.
- **The long run.** Lab 3 (32 days, the gated design) boom-busted after day 19; the chosen
  design was not run past 16 days.

### Later, if needed
2. **Prey switching, the floor:** the fox walks toward chickens only where there are at
   least `HUNT_FLOCK` within 8 (`count chicken within 8 >= HUNT_FLOCK and nearest meat
   within 8 ...`), sweep 2, 3, 4: lone hens are safe, so no flock can be eaten out, and
   foxes in thin country go hungry and fewer of them breed.
3. **If the rates are still short: hungrier foxes.** Kills a fox-day ~ 1d / (1d - the
   threshold): from 20h to 21h or 22h, each meal is smaller, so more of them. A knob on
   kills per fox, not on how many foxes.
4. **Kept in reserve:** fewer chicks dying of thirst (half the chick deaths on seed 42)
   raises the flock's surplus, so the same flock feeds more foxes.

## Decision

In `rules/animals.rules` (2026-10-30): `LITTER_HENS = 10` (a kit needs 10 hens within 16,
not 15), `HUNT_FLOCK = 4` (a fox walks toward prey, or eats what is next to it, only with
4 hens within 8), `DESPERATE = 16h` (below 16h of food it eats whatever is at its feet).
Labs used: 9 of 20 (plus a cost check), 00:33 to ~02:00.

| full goal, 16 days | kills 2d (>= 32) | kills 9d (>= 19) | chickens >= 100 | foxes >= 3 | seeds held |
|---|---|---|---|---|---|
| rules as they were | 16.2 median, 0/8 | 9.4, 3/8 | 7/8 (0 on 43) | 4/8 | 0 of 8 |
| tuned, seeds 42 .. 49 | 51.5, 8/8 | 72.3, 8/8 | 8/8 (188 .. 239) | 8/8 (7 .. 14) | **8 of 8** |
| tuned, seeds 42 .. 57 | | | | | **15 of 16** |

Why it works: a fox hunts only where hens are in groups of four, so a thinned flock is out
of its reach and can always recover: the floor holds for a reason, not by a number. With
the flock kept dense, the litter gate at 10 hens opens and foxes multiply where the flock
is, so the second week has ~30 foxes hunting instead of the founders' few survivors: kills
from day 9 go from 9.4 to 72 a day.

Fragile: `LITTER_HENS` below 10 (the floor fails at 8); seed 55's first week; anything past
16 days (not run with this design; the gated design boom-busted after day 19 in 32 days).

## Lessons (so far)

- A predator capped at its food max absorbs only about (max - its hunger threshold) a
  meal: meals a day are about burn / (max - threshold), not burn / prey's food, and kills
  are the meals that kill. Here 5 or 6 meals and ~3 kills a fox-day, not one hen.
- With shares like `start fox 1 / 5000`, the founders per seed vary 7 .. 14 on 256x256, and
  their ratio to the prey decided crash or boom. Count the founders first (a `run 1` lab).
- A birth gate set on prey density acts only above that density: a predator that holds its
  prey below it never breeds. Check which gate of a birth rule binds before relaxing one.
- (2026-10-30) Relaxing the predator's birth gate alone overshoots: founders breed in
  lockstep and every kit kills from birth, so one or two litters pass the tipping ratio
  and the prey is eaten out (gate 8 or 6: every seed). Put a brake on killing at low prey
  density first.
- Switching (hunt only where 4 hens are within 8) raised the floor *and* the kills: the
  flock it protects stays dense, and the predator's births follow the flock. A brake on
  consumption changes the resource, and the consumer's numbers follow the resource.
- A switching gate that leaks (a fox eating whatever it bumps into) undoes much of it: the
  flock stays thinner, half the kits. One that never leaks breaks content (a starving fox
  beside a cornered hen) and starves founders. A starving exception (below 16h) kept both.
- Copying a goal file's head for a measurement copy copied its `run` line: 32 days instead
  of 16. `grep -n '^run'` a copy before running it.
