# Cookbook: from a property to a mechanism to the rules

Ecology, economics, epidemiology and automata theory have names for most of what a world
here can do, and the rules language says each in a line or two. For each mechanism: what it
does to the dynamics, the idiom, the knob, and the number that should move (an `expect`, or
an observe line). Predictions come from `arithmetic.md`; confirm each with a lab. The
snippets compile inside a kind that declares the needs, memories and consts they name
(`rules/*.rules`, `packs/life`, `packs/village` show them in context).

## First, find what limits each population

Read `wmc observe`'s per-day table, its kills table and the per-kind blocks, then
`--gates` on the rules that matter, before choosing anything:

| symptom | observe shows | look at |
|---|---|---|
| dies out, of a need | `causes of death: no X`; X low in `needs` | how it refills X: its search radius, its gates (`--gates` on the rules that refill X), the X around it, competitors |
| dies out, killed | `eaten by K`; K's line in `kills a day` (per head) | the brakes on consumption below |
| dies out, old | `own die`, few `born` or `became` | its births: `--gates` on its `spawn` or `become` rules names the term that stops them |
| explodes | births per head far above deaths per head | the density gates on its births |
| a state empties or floods | `states WORK 100%  REST 0%`; `count K in STATE` | the transition rules' gates; lockstep (everyone switches at once) |
| stuck, idle | `decides idle` high; `results BLOCKED` high | a gate that never opens, a walker boxed in by rock or water |
| a rule never fires | missing from `fires`; `--gates`: few `reached`, or one term stopping nearly all | low `reached`: a rule above it acts first (order); else the binding term |
| needs identical in everyone | `p10` equals `p90`; counts that jump together | founders in lockstep: stagger them at birth |

## Brakes on consumption

### Satiation: act only below a threshold

A consumer that acts only below `low`, with its need capped at `max`, gains at most `max -
low` an act (`arithmetic.md`, transfers). That bounds what it takes a day, whatever its
sight or speed: those change how soon it eats, not how much.

```text
when food < 20h and nearest meat within 1 as m => eat m
when food < 20h and nearest meat within 8 as m => move toward m
```
Knob: the threshold. Raising it (`20h` to `22h`) makes the consumer hungry more of the day
and each act smaller (2 hours instead of 4): more acts, more kills a day. Lowering it does
the reverse. The cap (`max`) and the bite are knobs too. Watch the killer's `per head` in
observe's kills table, and its need's median (it lives between `low` and `max`).

### Switching: consume only where it is plentiful

A consumer that ignores scarce prey (or turns to something else) eases off exactly when the
prey is rare: a floor under the prey that no tuning of rates gives you. Without it,
consumers find the last of their prey as easily as the first.

```text
when food < 20h and count only chicken within 8 >= 3 and nearest only chicken within 8 as m
     => move toward m
when food < 20h and nearest egg within 8 as e => move toward e     # rare hens: eggs instead
```
Knob: the count and the radius. Watch `expect always count chicken >= N`; the consumer's
`no food` deaths rise if it has nothing else to eat.

### Refuges: some of the prey out of reach

Part of the prey out of reach, by state or by place, keeps a remnant through any pressure.
By state, with looks: hens roost at night showing `look 1`; a fox that takes only hens
awake (`chicken:0`) leaves the roost alone.

```text
when food < 20h and nearest chicken:0 within 8 as m => move toward m
```
By place: prey that keeps to where the consumer does not go. Watch the prey's lowest count;
the consumer's kills a day fall a little.

### Vigilance: prey that flee

```text
when nearest fox within 5 as f => move away f
```
Knob: the radius, and the rule's place (above the eating rules, fear wins over hunger). A
radius past the kind's `sight` is cut to `sight` (the lint warns): raise `sight` with it.
Watch the prey's `eaten by` share and its `no food` deaths together.

## Births and growth

### Births that need a resource: the numerical response

Consumers that breed only where their resource is dense do not breed where it is thin. The
fox's litter rule needed 15 hens within 16 cells, a kit every 5 days at most:

```text
when age > 2d and food > 20h and day + 1 > last_litter and chance(2)
     and count fox within 16 < 2 and count only chicken within 16 >= 15
     and nearest free within 1 as c => { last_litter = day + 5  spawn fox at c }
```
A gate on a density opens only above that density (`arithmetic.md`, densities): 15 hens in
the ~885 walkable cells of the square was about 7 times the starting density, so where the
foxes held the flock thinner they never bred and died out of old age. Fox births were 0 a
day at a mean of 30 hens, 0.06 at 229, 0.75 .. 1.8 at 390 .. 480. Before relaxing such a
rule, `--gates` it: the term that stops the thinks that pass the dice is the one to move.
Knobs: the count (in a `const`, to sweep), the interval, the dice. Count `only` the
resource that matters (counting chicks too once let foxes boom and crash the hens).

Such a gate does not stop a boom: while the resource booms, every consumer stands where it
is dense, so any count passes, and the gate closes only once the resource is thin, with
the consumers already many. The interval caps the rate instead. Over 32 days, foxes with a
kit every 5 days rose to 40 and fell under 3; with one every 10 (past most of a fox's
~9-day life: about one kit each) every seed held, with more kills, since the flock grew
ahead of the foxes. Lifetime young near 1 is enough when nearly every adult gets its one.

### Carrying capacity: births that need room

A `spawn` gated on room (`count K within r < n`, `nearest free within 1`) slows growth as a
population fills its space: logistic growth instead of a boom.

```text
when growth >= 12 and count grass within 2 < 8 and nearest bare within 1 as c
     => { growth = 0  spawn grass at c }
```
Knob: `n` and `r`. Watch `expect mean count K`, and births a day in observe.

### Territory: a cap per area

A gate that counts the actor's own kind (`count fox within 16 < 2` above) caps a population
at about one per gate area, whatever the food: interference. The same idiom caps colonies
(`count bee within 8 < 10`) and clumps. Check it binds before blaming it: 11 foxes on ~53 000
walkable cells see ~0.2 other foxes in their square, and that gate never closed.

## Supply

### Regrowth: a resource that grows back

A consumed resource holds only if it regrows faster than it is taken. Grass heals a bite
with `chance(50)` at each think it is left alone (84 thinks a day).

```text
when health < 4 and hurt == 0 and chance(50) => health += 1
```
Knob: the chance and the cadence (`chance(10)` let the hens graze the meadow down to 15%).
Watch `expect mean count grass >= K` and grass `eaten by` in observe.

### A resource in the terrain

Most sudden die-offs in an ecosystem are thirst: thirst killed as many chickens as foxes
did in the fox goal. A drinker remembers water within `seen` and walks back below
`thirsty`; a rooted kind needs water within `reach`.

```text
kind sheep extends drinker(8, 2h, 3) { glyph "s"  sight 8 }   # sees water 8 off, drinks below 2h
```
Knobs: the trait's arguments, the terrain (`terrain water_level`, `water_scale`), where the
`start` lines put them. Watch `expect never died K of water > 0`.

Access to water is a resource too, and standing actors take it: a drinker drinks only from
a free cell next to water. In the built-in world seeds that land on a shore never sprout or
dry out, and the trees kept raining them, so the woods filled the shores within weeks
wherever the hens (who eat seeds) were few: thirst deaths per hen rose tenfold and the
flock died after its foxes. Watch thirst deaths *per head over time*: a cause that only
grows is a stock filling up. Change its inflow (`TREE_SUN`: fewer seeds per tree), not its
lifetime (rotting seeds changed nothing: the rain refilled the shore faster).

## Time

### Delays and oscillations

Time between cause and effect (an egg hatches in 8h, a chick is a hen at 2 days, a kit every
5 days) lets a population overshoot before its brake bites: boom, then bust. Shorter delays,
brakes that act on the present (density gates, satiation), or a cap on the birth rate (the
interval above) damp it. Watch both ends: `expect always count K >= low` and `expect always
count K <= high`, over a run longer than the cycle.

### Cohorts and lockstep: stagger at birth

Actors born at once with the same needs do everything at once (`arithmetic.md`, cohorts).
Spread them at birth when lockstep is not the point:

```text
when born == 0 => { born = 1  energy = 12h + rand(12h) }
```
The village rested all at once (none at work for 70 minutes every ~17 hours) until this
line; after it, 96 to 117 of 117 were at work at every check. Watch `p10`/`p90` of the need
in observe, and `expect always count K in STATE >= N`.

### Hysteresis: two thresholds, not one

One threshold makes an actor flip between two states at every think near it; two make long
spells, and a gap between them that is a knob on how long each spell lasts.

```text
state WORK {
  when energy < 6h => next REST
  when true => move random free
}
state REST {
  when energy >= 22h => next WORK
  when true => { energy += 1h  idle }
}
```
Watch the share of each state (observe's `states`, `expect mean count K in STATE`).

### Timing windows

Activity windows gate who meets whom: a fed fox sleeps from 09:00 to 17:00, hens roost from
20:00 to 05:00. Their overlap is a knob on every encounter.

```text
when hour >= 9 and hour < 17 and food >= 12h => { look = 1  idle }
```

### Age

`mortal(after, odds)` sets a lifespan with a spread: a cohort does not die at once, but it
dies in a wave. Longer lives mean more births for the same birth rate, and older
populations. `arithmetic.md` turns the two numbers into a mean life.

## Exchange and status

### Transfers: `give` and `take`

`give t NEED n` and `take t NEED n` move up to `n` between neighbours, never past the
receiver's max: trade, theft, feeding young, a hive's stores. Each needs the other to be
adjacent (`nearest K within 1 as t`, or a walk toward it first).

### Visibility: what a neighbour can see

Others read only `look` (a byte) and `signal` (16 bits); needs and memory are private.
Publish what others need to decide, at the resolution they need:

```text
when true => look = clamp(wealth / 10, 0, 255)
```
The resolution is a knob: at a tenth, villagers within 20 or 30 wealth of each other look
the same.

### Redistribution: give to a poorer neighbour

```text
when wealth >= RICH and nearest villager within 2 as v and look_of(v) + 2 < look and gave == 0
     => { gave = 4  give v wealth SHARE }
```
Knobs: who gives (`RICH`), how much (`SHARE`), how much poorer the other must look (`+ 2`),
how often (`gave`). A gift larger than the gap it closes makes the giver the new poorest:
see the worked example.

### Roles as states

A state is a role (`WORK`, `REST`, `SICK`), and goals can count them: `expect always count
villager in WORK >= 80`, `expect mean avg wealth of villager in REST >= 50`.

## Neighbourhood rules: cellular automata

A cell that thinks every tick against the board as it stood is a cellular automaton; its
birth and survival counts are its whole dynamics. Keep them in consts and sweep them:

```text
const BIRTH = 3
const LONELY = 2
const CROWDED = 3
kind dead { glyph "."  cadence 1  sight 1
  when count alive within 1 == BIRTH => become alive }
kind alive { glyph "O"  cadence 1  sight 1
  when count alive within 1 < LONELY + 1 or count alive within 1 > CROWDED + 1 => become dead }
```
The start share sets the soup; the rules set where it settles. Measured on 128x128, 4 seeds,
generations 300 to 600: B3/S23 (as written) about 5% alive, B3/S123 (`LONELY=1`) 35%,
B3/S234 (`CROWDED=4`) 51% and nearly frozen. Watch `expect mean count alive ... from 300`
(windows in generations) and `became alive per day` (births per 21 600 generations).

## Contagion

Spread by contact, through a public look; recovery after a while.

```text
const INFECT = 20
kind person {
  glyph "p"
  cadence 64
  mem ill
  state WELL {
    when nearest person:1 within 1 as s and chance(INFECT) => { look = 1  ill = 0  next SICK }
    when true => move random free
  }
  state SICK {
    when ill >= 300 => { look = 0  next WELL }
    when true => { ill += 1  move random free }
  }
}
```
Each sick person infects about (sick neighbours met a think) x (`INFECT` / 100) x (thinks
sick: 300, nearly a day) others: above 1 it spreads, below 1 it dies out. Watch `expect
eventually count person in SICK == 0 from T` and `expect mean count person in SICK`.

## Rarity

"Now and then" is a per-think chance times thinks a day (`arithmetic.md`). For about once a
day at cadence 64 (337 thinks): `rand(1000) < 3`.

## Worked examples

### An ecosystem: "foxes hunt more chickens, but chickens never die out"

From `tuning/foxes-hunt-more/notes.md` (2026-10-29).
1. Goal on 256x256 for 16 days, 8 seeds, 7 to pass: kills `>= 32` a day from day 2 (twice
   the baseline median of 16.2, above its best seed's 26.6) and `>= 19` from day 9 (after
   the founders age out); `always count chicken >= 100` (half the usual lowest);
   `always count fox >= 3` (the hunters must last too). Baseline: 0 of 8 seeds.
2. The story, from the baseline's numbers per seed. Each fox kills ~3 a day (2.6 .. 3.6),
   not one hen: it eats below 20h with a 1d cap, so ~4 hours a meal, 5 or 6 meals, ~3 of
   them kills. A flock yields ~0.2 .. 0.3 chickens per hen a day, so the founders' ratio
   decided each seed: 10.4% foxes to hens crashed the hens, 8.3% held, 4.9 .. 6.8% boomed.
   Foxes died of age from day 6 and rarely bred: the litter's prey gate (15 hens within 16)
   needed 7 times the starting density; the territory gate never closed. Thirst killed as
   many chickens as the foxes.
3. So kills a day are (foxes) x (~3), and foxes are few because the birth gate binds.
   Experiment 1: `>= LITTER_HENS`, swept 15, 10, 6 (the numerical response), predicted to
   raise late kills and keep the foxes past day 9, and to leave the floor failing on the
   high-ratio seeds. Then the floor: switching (hunt hens only with `HUNT_FLOCK` or more
   within 8). Then, if rates are short, a hungrier fox (eating below 21h or 22h: smaller,
   more meals).
4. What happened: a lower litter gate alone crashed every flock (lockstep litters);
   switching plus the gate at 10 and a starving exception met the 16 days on 8 of 8. Run
   for 32 days, the same rules died on every seed: a fox boom on some, and on all a flock
   dying of thirst as the woods filled the shores. Extended to 32 days: fewer seeds per tree
   (`TREE_SUN` 4 to 16) and about one kit a fox (`LITTER_GAP` 5 to 10) held 16 of 16 seeds
   (lessons.md, both 2026-10-30 entries).

### A society: "the poorest keep up"

`packs/village`, 64x64, 2 days, 4 seeds, the goal `expect always min wealth of villager >=
230 from 1d`. Baseline: the poorest's lowest 174 .. 183 (median 183), the mean 286.
1. `--gates` on the sharing rule: of 4540 thinks that reach it, 6% stop at `wealth >=
   RICH`, half of the rest find no neighbour within 2, and 94% of those see no one poorer
   (`look_of(v) + 2 < look`): 57 gifts. Neighbours look alike at a tenth's resolution.
2. Prediction for `--set SHARE=10,40 --set RICH=60,250`: bigger gifts lift the poorest.
   Result: SHARE 40 lowered the poorest's median from 183 to 154, RICH 250 to 159; the mean
   did not move. The miss, explained: a gift of 40 between neighbours whose looks differ by
   3 (30 to 39 wealth) leaves the giver poorer than the receiver was: the giver becomes the
   new poorest. (With RICH 250 nobody gives before day 1.)
3. Gifts sized to the gap (`give v wealth (look - look_of(v)) * HALF`): at most 186.5
   (`HALF=3`; no gifts at all: 157). A finer look (`wealth / 3`) relaxed the binding gate
   (78% stopped instead of 94%) and tripled the gifts (167), and the cooldown (`gave == 0`)
   became the next gate: the poorest rose to 180.5.
4. So the floor is not made by sharing. Every villager drifts up about 165 a day
   (`arithmetic.md`), and the poorest founder started at 20: about 185 by day 1, when the
   window opens, whatever the gifts. When every lever of a mechanism barely moves a number,
   find what sets the number before sweeping more: here the start (`20 + rand(40)`) and the
   window. The goal needs revisiting with the user (a window from day 2, or `>= 180`).

### An automaton: "a Life variant that settles at a fifth alive"

`packs/life` on 128x128 for 600 generations, 4 seeds, the goal `expect mean count alive >=
3000 from 300` and `<= 3600` (of 16 384). `wmc lab --set CROWDED=3,4 --set LONELY=2,1
--full`: means from generation 300 were B3/S23 788 .. 907 alive (5%), B3/S123 35%, B3/S234
51%, B3/S1234 50%. No variant passes, and none can: a fifth lies between S23 and S123, and a
count is an integer. When the goal falls between two values of an integer knob, add a die
to interpolate: a lonely cell (one neighbour) that survives with `chance(SURVIVE)`, a rule
above the death rule:

```text
when count alive within 1 == 2 and chance(SURVIVE) => idle
```
Predicted: the density moves from 5% (`SURVIVE 0`) to 35% (`100`, which is S123), faster
than linearly at first (survivors seed births). Swept 0, 25, 50, 75, 100: 5.3%, 20.5%,
27%, 31%, 35%. `SURVIVE=25` held on 4 seeds of 4 (3307 .. 3401 alive): a quarter of the die
gave half the change.
