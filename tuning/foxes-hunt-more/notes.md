# Foxes hunt more chickens, but chickens never die out

**Asked:** "foxes hunt more chickens, but chickens never die out" (2026-10-29), in the
built-in world: the kinds in `rules/`, the world of `scenarios/default.scenario` (its
terrain and `start` lines), on a size quick enough to iterate on.
**Goal:** `goal.scenario` (256x256, seeds 42..49, pass 7); fast tier `goal-fast.scenario`
(192x192, the same seeds and pass). 16 days until experiment 10, 32 days since (the user,
2026-10-30: "Yes, extend to 32d"; section "32 days" below, with its own lines).
**Budget:** the skill's default, 20 lab sweeps or 2 hours, from the user's go-ahead (each
time).
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
- **The long run:** now run, below (10).

### 10. The long run: the tuned rules for 32 days (2026-10-30, after the commit)
- **Run:** `runs/measure-full-32d.scenario` (the full goal's world, `run 32d`, the same
  lines plus floors from day 16), the rules as committed; `runs/11-32d.csv`, 223 s.
- **Result: both kinds die out after the goal's window, on every seed.** Foxes rise to 26 ..
  52, then fall below 3 between days 20.2 and 28.2 and die out on all 8 seeds. The flock
  falls below 100 between days 17.7 and 31.1 (seeds 47, 44, 46, 48, 42, 45, 43, 49), to 0
  on five seeds, 10 and 11 on two, 86 on one. Kills from day 16 to 32: 5.5 .. 19.3 a day.
  The 16-day goal holds because the bust starts just after it.
- **Inferred, not yet probed:** switching keeps the flock dense, so litters go on until
  there are 26 .. 52 foxes, more than the flock can feed once it thins; foxes live ~9 days,
  so their numbers lag the flock's decline. Then many foxes go hungry at once, fall below
  `DESPERATE` and eat whatever is at their feet, and the eggs and chicks around the last
  groups of hens go too: the flock stops renewing, then the foxes starve. The fully gated
  design (lab 3: no starving exception) also boom-busted after day 19, but the flock fell
  below 100 on 3 seeds of 8, not 8: the exception deepens the bust; the fox boom causes it.
- **Next, if the goal is extended to 32 days** (it should be: "never die out" means more
  than 16 days): a brake on fox numbers rather than on hunting. Candidates, each with a
  prediction before it runs: a longer litter interval (`day + 5` as a const, 8 .. 10:
  fewer kits a fox, a lower peak); litters only when well fed (`food > 22h`); or a
  starving exception that spares eggs and chicks, so the flock keeps renewing through the
  bust. Watch the peak fox count and the flock's lowest from day 16.

## 32 days (2026-10-30, from 04:32)

**Asked:** "Yes, extend to 32d" (the user, after experiment 10). Budget: 20 labs or 2 hours,
from 04:32; the session's process died at ~05:05 (labs 12 and the baseline done, lab 13
lost), and the budget restarted at 09:38 with the go-ahead relayed again.
**Goal change:** `run 32d` in both files; the floors as agreed, over the whole month; the
kill lines re-derived by the skill's rule ("above the baseline's best seed") from a 32-day
baseline of the rules as of 1b76f0c (lab 11, `runs/11-32d.csv`): `>= 37 from 2d` (its
seeds 22.2 .. 36.7) and `>= 40 from 9d` (19.4 .. 39.6). The fast tier's kill lines are
those scaled by the area (21 and 23): by the same rule its own baseline would ask 32 and
39, numbers set by one seed (46) whose foxes binge on the flock and then have none, and
which no variant has come near.

**The baseline over 32 days** (the rules as they were: `--set HUNT_FLOCK=0 --set
DESPERATE=0 --set LITTER_HENS=15`, which compiles to them; `runs/base-32d-full.csv`, 489 s):

| seed | kills 2d (>= 32) | kills 9d (>= 19) | lowest chickens | lowest foxes | highest foxes | mean hens from 16d |
|---|---|---|---|---|---|---|
| 42 | 10.7 | 5.8 | 163 | 1 | 11 | 642 |
| 43 | 9.9 | 0.6 | **0** | 0 | 14 | 0 |
| 44 | 33.7 | 38.0 | 170 | 2 | 63 | 906 |
| 45 | 71.6 | 87.8 | **5** | 3 | 88 | 994 |
| 46 | 67.4 | 81.7 | **0** | 0 | 78 | 504 |
| 47 | 72.4 | 90.2 | **0** | 0 | 85 | 697 |
| 48 | 64.5 | 78.2 | **0** | 0 | 68 | 372 |
| 49 | 29.8 | 31.2 | 202 | 2 | 57 | 1220 |

0 of 8 seeds hold every line (per line 5, 6, 3, 1 of 8). Over a month the old world
boom-busts too, later: where the founders are few its flock booms to ~1000 hens, the
litter gate at 15 opens at last, foxes rise to 57 .. 88 and eat the flock out (seeds 45 ..
48). So over a month the old world kills more than the 1b76f0c rules do (median 49 a day
from 2d, best 72), by eating its flock out: a "more" taken from it would ask for a binge,
not a world where both kinds last. The kill lines take the 1b76f0c rules as the baseline
instead (above their best seed: 37 and 40). Those rules (lab 11): 0 of 8 (kills 30 and 29
a day, both floors 0 of 8).

### 11. Why the tuned world dies after day 16 (observe, seeds 42 and 47)
`wmc observe 32d 256 256 {42,47} --scenario runs/measure-full-32d.scenario --gates`
(`runs/obs-32d-tuned-s{42,47}.{txt,csv}`), per 2 days:
- **Seed 47, a fox boom:** 7 founders, 48 foxes by day 16 .. 18. From day 10 to 16 about 7
  kits a day: each adult breeds every 5 days, the gates that stop its litter thinks are the
  kits' age and the interval; the territory starts to bind only at the peak. 48 foxes kill
  120 .. 135 a day against a flock that peaked at 385 hens on day 11: eaten out by day 20,
  then 13 foxes a day starve.
- **Seed 42, thirst:** a smaller boom (11 to 25 foxes, ~65 kills a day at the peak), but the
  flock falls after the foxes are gone too. From day 18 the foxes kill under 11 a day and
  the hens still go from 90 to 0 by day 28, of thirst. Hen thirst deaths per 100 hens a day:
  1 .. 3 on days 0 .. 8, 6 .. 10 on days 8 .. 14, 21 .. 26 on 14 .. 20, 39 .. 93 on 20 .. 28.
  Seed 47 too: 2, 11, 24, 35. Foxes die of thirst as well (29% of fox deaths on seed 42).
- **Thirst follows the forest.** Seeds standing on seed 42: 190 on day 1, 2040 on day 13,
  3300 on day 19, 5100 on day 27; trees 133, 480, 780, 1210. A seed next to water never
  sprouts (`count water within 1 == 0`) and never dries out; trees grow only within 2 of
  water and drop ~5 seeds a day each on free cells within 2 (`sun >= 4`). So the shores fill
  with standing plants, and a thirsty hen walking to the water it remembers finds no free
  cell beside it. Hens eat seeds: a big flock keeps the forest down. The old rules on seed
  42 (`runs/obs-32d-base-s42.txt`), where the flock grows from 140 to 840 hens, hold seeds
  at ~2400 and trees at ~690 from day 14, and hen thirst at 9 .. 11 per 100 a day. Under
  the tuned foxes the flock stays at 130 .. 260 hens, the forest grows without end, and
  thirst runs away as the flock shrinks: fewer hens, more seeds, more thirst.
- **Check 1, a miss:** seeds that rot after 3 days (a scratch pack: `when age > SEED_ROT =>
  die`) changed nothing on seed 42: 5967 seeds at day 32 against 5951, the same thirst. The
  stock is not old seeds but the rain (~2000 a day by day 16), which re-seeds a shore cell
  faster than a seed rots.
- **Check 2:** a quarter of the rain (scratch pack, `sun >= 16`), seed 42: seeds 30 .. 110 and
  trees 56 .. 122 all month, hen thirst 0.2 .. 8 per 100 a day (2 .. 5 mostly), and **both
  kinds live to day 32**: foxes 11, 31 on day 17, 9 on day 25, 25 on day 32; hens 130, 250,
  156 on day 21, 431; kills 50 .. 80 a day at the peak, 18 at the trough. A cycle that damps.

So two things end the tuned world after day 16: the forest takes the water of a small
flock, everywhere; and where the founders are few, the kits outgrow the flock (seed 47).
The forest comes first: it kills the flock even after the foxes are gone, and with it
fixed seed 42 cycles instead of dying.

### 12. Fewer seeds per tree (lab 12, fast tier, 32 days)
- **Change:** `sun >= 4` in the tree's seed rule becomes `sun >= TREE_SUN` (a const, 4 as
  written: the same hash); sweep 4, 8, 16.
- **Mechanism:** the forest is the hens' rival for the shore. A slower rain keeps it small,
  so shore cells stay free whatever the flock's size, and a thinned flock can still drink
  (a resource in the terrain, kept open).
- **Prediction** (medians over the 8 seeds of `goal-fast.scenario`):
  - seeds from day 16: thousands at 4, about a third at 8, under 150 at 16; trees likewise;
  - hen thirst deaths from day 16: ~15 .. 25 a day at 4, a few at 16;
  - chickens >= 60: 0 .. 2 of 8 at 4 (the control busts), 3 .. 5 at 8, 5 .. 7 at 16: a fox
    boom like seed 47's can still eat a flock out;
  - foxes >= 2: 0 .. 1 at 4, 4 .. 6 at 16 (they drink better too, and keep prey);
  - kills from 2d (>= 21): up with the value, since foxes persist: ~15 .. 20 at 4, 25+ at 16;
  - verdict: 16 holds on most seeds if the cycle damps as on seed 42; where it does not, the
    fox boom is the next lever (fewer kits: the interval, or the litter gate).
- **Run:** `wmc lab runs/m32-fast.scenario --set TREE_SUN=4,8,16 --full --csv runs/12.csv`
- **Result** (lab 12, 698 s; judged against the fast tier's lines as they were then, 21 and
  9; medians; seeds that held each line):

  | TREE_SUN | kills 2d | kills 9d | chickens >= 60 (lowest) | foxes >= 2 (lowest) | highest foxes | seeds from 16d | trees from 16d | hen thirst a day from 16d | seeds held |
  |---|---|---|---|---|---|---|---|---|---|
  | 4 (as written) | 15.9, 3/8 | 15.8, 6/8 | 0/8 (0 .. 41) | 0/8 (0 .. 1) | 13 .. 39 | 540 .. 1870 | 131 .. 463 | 0.7 .. 41 | 0 |
  | 8 | 24.0, 6/8 | 26.2, 8/8 | 4/8 (0 .. 139) | 3/8 (0 .. 6) | | | | | 3 |
  | 16 | 25.5, 7/8 | 28.6, 8/8 | 4/8 (0 .. 123) | 4/8 (0 .. 8) | 18 .. 46 | 15 .. 181 | 15 .. 77 | 1.6 .. 12.6 | 4 |

- **Against the prediction:** the forest shrank as predicted (seeds from day 16 ~20 times
  fewer at 16, trees ~8 times) and every line rose with the value, as predicted. The
  floors held on 4 seeds at 16, at the low end of the 5 .. 7 predicted. Seeds 43, 44, 45, 49
  hold everything (highest foxes 18 .. 22); 46, 47, 48 lose the flock at days 17 .. 21 and
  then the foxes (highest foxes 40, 29, 46); 42 dips under 60 at day 25 (42 hens) and loses
  its foxes at day 30 (highest 28). So what is left is the fox boom: where the foxes pass
  ~25 on this tier, they eat the flock out. Its kills from 9d (20.9 .. 35.8) also clear the
  new line (23) on 6 seeds.

### 13. Fewer kits a fox: the litter interval (lab 13, full goal, 32 days)
- **Change:** `last_litter = day + 5` becomes `day + LITTER_GAP` (a const, 5 as written:
  the same hash); sweep 5, 7, 10 with `TREE_SUN=16`.
- **Why not the litter gate:** a booming flock is dense everywhere a fox is, so it passes
  any hens-within-16 count (10 or 15): a higher gate only stops births in a thin flock,
  after the boom. And the territory (`count fox within 16 < 2`, which counts the fox
  itself) is already as strict as `sight 16` allows: no other fox in its square. The
  interval caps the rate: an adult breeds at most once in LITTER_GAP days, so a boom grows
  more slowly and the flock has time to thin before the foxes are many.
- **Arithmetic:** a fox breeds from age 2 and lives ~9.1 days (`mortal(6d, 3)`): kits a life
  if the gates are open ~1 + P(alive at 7.5) + P(alive at 12.5) ≈ 1.75 at 5 days, ~1.3 at 7
  (ages 2.5 and 9.5), ~1.1 at 10. So at 10 foxes barely replace themselves even when fed;
  at 7 they grow at about half the rate of 5.
- **Prediction** (full goal; kills against 37 and 40):
  - 5: like lab 12's 16 on more founders: booms on the seeds with few founders (45 .. 48 in
    the baseline): 4 .. 6 of 8 hold the floors; kills from 2d ~40 .. 55;
  - 7: highest foxes about 0.7 times as many, booms slower: the floors on 6 .. 8 seeds; kills
    ~35 .. 45 from 2d: the kill lines may fail on some seeds;
  - 10: no boom, foxes at about the founders' number (7 .. 14): kills ~25 .. 35: the kill
    lines fail on most seeds; foxes may age out where the founders are few: 2 .. 5 of 8;
  - verdict: 7, if any.
- **Run:** `wmc lab runs/m32-full.scenario --set TREE_SUN=16 --set LITTER_GAP=5,7,10 --csv
  runs/13.csv` (no `--full`: a variant stops once it cannot pass; started 09:39 with the
  copy's old kill lines, 32 and 19, so its verdicts are re-read against 37 and 40 from the
  CSV's numbers)
- **Result** (lab 13, 863 s; against the goal's lines as they are now; medians):

  | LITTER_GAP (TREE_SUN 16) | kills 2d (>= 37) | kills 9d (>= 40) | chickens >= 100 (lowest) | foxes >= 3 (lowest) | highest foxes | foxes from 16d | fox births a day | hens from 16d | seeds held |
  |---|---|---|---|---|---|---|---|---|---|
  | 5 | 43.7, 44.4 | 46.0, 47.8 | 205, 220 | 2 of 4: 8, 14; **2** on 44 and 45 at days 26 .. 28 | 31 .. 40 | 18.8, 24.3 | 2.3 .. 2.4 | 189 .. 236 | 2 of 4, stopped |
  | 7 | 45.3 .. 48.6 (46.7) | 49.1 .. 54.5 (50.2) | 124 .. 243 (205) | 4 .. 14 (8) | 31 .. 45 | 17.8 .. 25.8 | 2.1 .. 2.6 | 144 .. 230 | **8/8** |
  | **10** | 41.5 .. 62.2 (53.0) | 45.3 .. 71.3 (57.4) | 171 .. 243 (224) | 5 .. 13 (8) | 29 .. 42 | 18.3 .. 30.6 | 2.2 .. 3.1 | 208 .. 302 | **8/8** |

  No fox starved in any run (`died fox of food` 0).
- **Against the prediction:** 5 failed as predicted, on the foxes: they rose to 40 and fell
  under 3 late (seeds 44, 45). 7 held as predicted, its kills clear of the lines. 10 was the
  miss: predicted to fail the kill lines with foxes at the founders' number, it held 8 of 8
  with the most kills (53 a day against 47) and the highest floors.
- **Explained** (from the lab's numbers, not followed in time): fox births are the same at 7
  and 10 (2.1 .. 2.6 against 2.2 .. 3.1 a day), so once the foxes level off it is not the
  interval that limits births but the other gates (prey, territory, a fed fox); the
  interval limits how fast a boom can run. At 10 a fox raises about one kit (its next would
  come at age ~12, past most lives), so the foxes can only grow slowly, and the flock grows
  ahead of them: more foxes on average from day 16 (25 against 20, median) on a bigger flock
  (hens from day 16: 208 .. 302 against 144 .. 230), whose lowest stays higher (113 .. 195
  against 82 .. 151). My arithmetic counted the kits a life and forgot that the flock grows
  meanwhile: R0 near 1 is enough when nearly every fox gets its kit, and it did.

### 14 .. 17. Confirm: TREE_SUN 16, LITTER_GAP 10
Both 7 and 10 pass; 10 holds the floors further from their lines (lowest hens 171 against
124) and kills more. Confirmed with each knob at about -20% and +20% (labs 14, 15), seeds
50 .. 57 (lab 16, `runs/m32-full-s50.scenario`), and the long run at twice the goal (lab 17,
64 days, `runs/m64-full.scenario`, `--full`). Full goal, against its lines as they are now
(lowest .. median over the seeds that ran to the end):

| variant (32 days) | seeds held | kills 2d (>= 37) | kills 9d (>= 40) | lowest chickens (>= 100) | lowest foxes (>= 3) |
|---|---|---|---|---|---|
| **16, 10, seeds 42 .. 49** (lab 13) | **8/8** | 41.5 .. 53.0 | 45.3 .. 57.4 | 171 .. 243 | 5 .. 13 |
| **16, 10, seeds 50 .. 57** (lab 16) | **8/8** | 45.0 .. 48.1 | 46.7 .. 50.9 | 179 .. 263 | 8 .. 14 |
| `TREE_SUN` 13 (lab 14) | 7/8: seed 46's foxes fall to 2 | 48.2 .. 49.9 | 51.1 .. 54.9 | 194 .. 238 | 7 .. 14 |
| `TREE_SUN` 19 (lab 14) | 7/8: seed 47's flock touches 99 | 44.4 .. 54.2 | 47.4 .. 60.4 | 198 .. 249 | 7 .. 11 |
| `LITTER_GAP` 8 (lab 15) | 8/8 | 42.3 .. 47.9 | 46.2 .. 51.9 | 155 .. 243 | 4 .. 11 |
| `LITTER_GAP` 12 (lab 15) | 8/8 | 46.1 .. 51.5 | 51.2 .. 55.4 | 197 .. 247 | 7 .. 14 |
| (`LITTER_GAP` 7, lab 13) | 8/8 | 45.3 .. 46.7 | 49.1 .. 50.2 | 124 .. 243 | 4 .. 14 |
| (`LITTER_GAP` 5, lab 13) | 2 of 4, stopped: foxes fall to 2 | | | | |

- **The long run** (lab 17, 64 days, 1098 s): 7 of 8 seeds hold every line for 64 days.
  The flock never fell below 171 on any seed (from day 32: 199 .. 319); kills from day 32
  52.7 .. 76 a day on the 7. On seed 47 the foxes fell to 2 at day 33.3 and died out, just
  past the goal's window (its kills then end: 27 a day over the 64 days), and its flock
  grew to 2224 hens with nothing to eat it. **Explained** (`wmc observe 36d 256 256 47 ...
  --gates`, `runs/obs-36d-s47.txt`): a boom and a cohort. Foxes 7 to 38 by day 21 (2 .. 6
  kits a day while the flock held ~400 hens), and the flock fell to ~125 hens on days 26
  .. 30. Thin, it is out of reach (switching), so the foxes stayed hungry: of the litter
  thinks past the age term, `food > 20h` stopped 318 .. 346 per 2 days from day 22, and
  births stopped at day 26. The kits of the boom then died of age together (every fox
  death was age) between days 26 and 34, while the flock recovered from day 30, too late:
  3 foxes at day 33, none at 34. So a boom can still happen where the founders are few
  (here 7) and the flock grows large; the interval makes it slower and the floor holds, but
  one cohort can age out in the trough. Not yet tried: litters for a fox below 20h when it
  has never bred, or the interval at 12 (lab 15: lowest foxes 7 .. 14 over 32 days).
- The interval holds from 7 to 12 and fails at 5. The rain holds from 13 to 19, each end
  losing one seed (7 of 8, as the goal allows): 16 sits in a band whose lower end lies
  somewhere between 8 (3 of 8 on the fast tier, lab 12) and 13, not measured on the full goal.

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

## Decision (32 days)

In `rules/plants.rules`: `TREE_SUN = 16` (a tree drops a seed every 16 sunny thinks, ~1.2
a tree-day, not every 4, ~3.2). In `rules/animals.rules`: `LITTER_GAP = 10` (a fox's next
kit 10 days after its last, not 5). Labs in this part: 7 (12 .. 17 and the 32-day
baseline), from 04:32 to ~05:05 and 09:38 to ~10:50.

| full goal, 32 days | kills 2d (>= 37) | kills 9d (>= 40) | chickens >= 100 | foxes >= 3 | seeds held |
|---|---|---|---|---|---|
| the old rules | 49.1 median, 5/8 | 58.1, 6/8 | 3/8 (0 .. 5 on the other 5) | 1/8 | 0 of 8 |
| the 16-day rules (1b76f0c) | 30.0, 0/8 | 29.4, 0/8 | 0/8 (0 .. 86) | 0/8 (0) | 0 of 8 |
| **now**, seeds 42 .. 49 | 53.0, 8/8 | 57.4, 8/8 | 8/8 (171 .. 243) | 8/8 (5 .. 13) | **8 of 8** |
| **now**, seeds 50 .. 57 | 48.1, 8/8 | 50.9, 8/8 | 8/8 (179 .. 263) | 8/8 (8 .. 14) | **8 of 8** |
| now, 64 days, seeds 42 .. 49 | 55.7, 7/8 | 57.7, 7/8 | 8/8 (171 .. 243) | 7/8 (0 on 47) | 7 of 8 |

Why it works: the woods no longer fill the shores, so a flock the foxes keep small can
still drink, and the flock's floor (switching) is no longer undone by thirst. A fox
raises about one kit, so the foxes grow only about as fast as the flock lets them and do
not boom past it; the flock grows ahead and feeds more of them, all month.

Fragile: the rain has a narrow band (13 and 19 lose a seed each; its lower end is
unmeasured between 8 and 13); a boom can still form where the founders are few and the
flock grows big, and its cohort can age out in the trough after: seed 47's foxes died out
at day 33 of 64. Kill lines are measured against the 16-day rules' 32-day run (37, 40);
the old rules, over a month, kill more (49 a day) by eating their flock out.

## Decision (16 days)

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

Fragile: `LITTER_HENS` below 10 (the floor fails at 8); seed 55's first week; and above
all **the long run: past day 16 the foxes boom (26 .. 52) and bust, and both kinds die out
by day 20 .. 31 on all 8 seeds** (experiment 10). The goal as written (16 days) is met; the
wish behind "never die out" is not, beyond it. Next: extend the goal to 32 days and brake
the fox numbers (experiment 10's candidates).

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
