# Cookbook: from a property to a mechanism to the rules

Population ecology has names for most of what a world here can do; the rules language can
express each of them in a line or two. For each: what it does to the dynamics, the idiom
(the built-in rules use most of these; `rules/*.rules` show them in context), the knob, and
the number that should move (an `expect` or an `observe` line). Predictions come from
`arithmetic.md`; confirm each with a lab.

## First, find what limits each population

Read `wmc observe`'s per-day table and per-kind blocks before choosing anything:

| symptom | observe shows | look at |
|---|---|---|
| dies out, hungry | `causes of death: no food`; `food` low in `needs`; few `eat`/`graze` in `decides` | how far it searches, its eating gates, the food around it, competitors |
| dies out, thirsty | `no water` | `drinker(seen, thirsty, steps)` or `rooted(reach, low, full)`; water in the terrain (`water_level`) or near the starts |
| dies out, eaten | `eaten by K` | predator pressure: the mechanisms below |
| dies out, old | `own die` and few `born`/`became` | births: their gates (`spawn` conditions), intervals, maturation |
| explodes | births far above deaths | the density gates on its `spawn` rules |
| stuck, idle | `decides idle` high; `results BLOCKED` high | a gate that never opens (read `fires`), a walker boxed in by rock or water |
| odd behaviour | a rule you expected missing from `fires` | rule order: a rule above it acts first |

## Satiation: a hunter stops when full

Hunting only below a hunger threshold bounds kills per predator at its energy budget
(`arithmetic.md`: about one hen a day for a fox), whatever its sight or speed. It is the
first brake on predation; without it, a predator kills whatever it meets.

```text
when food < 20h and nearest meat within 1 as m => eat m
when food < 20h and nearest meat within 8 as m => move toward m
```
Knob: the threshold (`20h`). Higher means hungrier more of the day and more kills, up to
the budget. Watch `expect mean eaten chicken by fox per day` and the fox's `food` median in
observe.

## Hunt only where prey is plentiful (prey switching)

A predator that ignores scarce prey (or turns to other food) eases off exactly when the prey
is rare: a floor under the prey that no tuning of rates gives you.

```text
when food < 20h and count only chicken within 8 >= 3 and nearest only chicken within 8 as m
     => move toward m
when food < 20h and nearest egg within 8 as e => move toward e     # rare hens: eggs instead
```
Knob: the count and the radius. Watch `expect always count chicken >= N`; the predator's
`no food` deaths rise if it has nothing else to eat.

## Births that need prey (the numerical response)

Predators that breed only where prey is dense cannot outgrow it. The fox's litter rule does
this: a kit every 5 days at most, only with 15 hens within 16 cells.

```text
when age > 2d and food > 20h and day + 1 > last_litter and chance(2)
     and count fox within 16 < 2 and count only chicken within 16 >= 15
     and nearest free within 1 as c => { last_litter = day + 5  spawn fox at c }
```
Knobs: the prey count, the interval (`day + 5`), the chance. Count `only` the prey that
matters: counting chicks too once let foxes boom and crashed the hens (`lessons.md`). Watch
`expect mean count fox` and `expect always count chicken >=`.

## Territory: a cap per area

A gate that counts its own kind (`count fox within 16 < 2` above) caps a population at
about one per gate area, whatever the food: interference. The same idiom caps colonies
(`count bee within 8 < 10`) and clumps (`count flower within 2 < 5`).

Knob: the number and the radius (the area grows with the square of the radius). Watch
`expect always count fox <= M`, and `mean count fox`.

## Refuges: prey a predator cannot take

Some of the prey out of reach, by place or by state, keeps a remnant through any hunting
pressure. With looks: hens roost at night (`look = 1`); a fox that takes only hens awake
(`chicken:0`) leaves the roost alone.

```text
when food < 20h and nearest chicken:0 within 8 as m => move toward m
```
Or by place: a predator that keeps off the shore leaves the hens drinking there alone
(`when count water within 2 > 0 => move away ...` in the predator, with the hens' water
rules above). Watch the prey's lowest count; the predator's kills a day fall a little.

## Vigilance: prey that flee

A prey that flees a predator within `r` meets it less, at the price of time not eating.

```text
when nearest fox within 5 as f => flee(f)
```
Knob: the radius (and the rule's place in the list: above the eating rules it wins over
hunger). A radius past the kind's `sight` is cut to `sight`, silently but for a lint
warning: raise `sight` with it. Watch the prey's `eaten by` share and its `no food` deaths
together.

## Density-dependent births: a carrying capacity

A `spawn` gated on room (`count K within r < n`, `nearest free within 1`) makes growth slow
as a population fills its space: logistic growth instead of a boom. Hens lay only with
fewer than 3 `meat` within 6; grass spreads only with fewer than 8 tufts within 2.

```text
when growth >= 12 and count grass within 2 < 8 and nearest bare within 1 as c
     => { growth = 0  spawn grass at c }
```
Knob: `n` and `r`. Watch `expect mean count K`, and births a day in observe.

## Renewal: a resource that grows back

A grazed resource holds only if it regrows faster than it is eaten. Grass heals a bite with
`chance(50)` at each think it is left alone (84 thinks a day): back to full in a few hours.

```text
when health < 4 and hurt == 0 and chance(50) => health += 1
```
Knob: the chance (and the cadence). `chance(10)` let the hens graze the meadow down to 15%
(`lessons.md`). Watch `expect mean count grass >= K` and grass `eaten by` in observe.

## Water

Most sudden die-offs are thirst. A drinker remembers water within `seen` and walks back
below `thirsty`; a rooted kind needs water within `reach`.

```text
kind sheep extends drinker(8, 2h, 3) { ... }      # sees water 8 cells off, drinks below 2h
kind reed extends rooted(3, 1d, 2d) { ... }       # water within 3 cells
```
Knobs: the trait's arguments; the terrain (`terrain water_level`, `water_scale`); where
`start` lines put them. Watch `expect never died K of water > 0`.

## Delays and oscillations

Time between cause and effect (an egg hatches in 8h, a chick is a hen at 2 days, a kit every
5 days) lets a population overshoot before its brake bites: boom, then bust. Shorter delays,
or brakes that act on the present (density gates, satiation), damp it.

Knobs: maturation ages (`when age > 2d => become chicken`), intervals (`last_x = day + k`).
Watch both ends: `expect always count K >= low` and `expect always count K <= high`.

## Age

`mortal(after, odds)` sets a lifespan with a spread: a cohort does not die at once. Longer
lives mean more births for the same birth rate, and older populations.

Knob: `after` and `odds` (`arithmetic.md` turns them into a mean life). Watch `own die`
deaths a day in observe.

## Timing

Activity windows gate who meets whom: a fed fox sleeps from 09:00 to 17:00, hens roost
from 20:00 to 05:00. Overlap of the waking hours is a knob on predation.

```text
when hour >= 9 and hour < 17 and food >= 12h => { look = 1  idle }
```

## Rarity

"Now and then" is a per-think chance times thinks a day (`arithmetic.md`). For about once a
day at cadence 64 (337 thinks): `rand(1000) < 3`.

## Worked example: "foxes hunt more chickens, but chickens never die out"

1. Goal: `expect mean eaten chicken by fox per day >= 2X from 2d` (X from the baseline),
   `expect always count chicken >= N from 2d`, `expect always count fox >= 2 from 2d`, on 8
   seeds, 7 to pass.
2. The energy budget says kills a day are about (foxes) x (one hen a day each). Sharper
   hunting alone (more sight, longer chases) barely moves the total: it makes foxes full
   sooner, not hungrier.
3. So: more foxes, held in check by prey. Relax the territory (`count fox within 16 < 3`),
   keep the litter's prey gate (`count only chicken within 16 >= 15`, perhaps higher).
   Predict: foxes up by about the territory ratio, kills a day up with them; hens' lowest
   count down.
4. Then a floor under the hens: prey switching (hunt hens only with 3 or more nearby, eggs
   otherwise) or the roost refuge (`chicken:0`). Predict: hens' lowest count back up, kills a
   day down a little; the fox's `no food` deaths up if eggs are scarce.
5. Sweep the territory number and the prey threshold together in one lab; confirm the
   winner on 16 seeds and at -20% and +20%.
