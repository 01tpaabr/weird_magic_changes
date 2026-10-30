# Lessons

What tuning this project's worlds has taught, newest first. Each entry: what was tried, what
happened (with numbers), why, and the rule of thumb. Add one at the end of every tuning
(protocol step 8); keep the numbers, they are what makes a lesson reusable.

## 2026-10-29: the first goal followed to its check-in, and the tools on other worlds

From `tuning/foxes-hunt-more/notes.md` ("foxes hunt more chickens, but chickens never die
out": 256x256, 16 days, 8 seeds; the baseline held on 0 of 8):
- **A capped consumer's take is set by its threshold, not by its prey.** A fox eats below
  20h with `food max 1d`: about 4 hours a meal whatever it eats, 5 or 6 meals a day, ~3 of
  them kills (2.6 .. 3.6 a fox-day on 8 seeds; a hen takes two bites). The skill's first
  arithmetic said one hen a day: wrong by the cap. Rule of thumb: acts a day ≈ burn /
  (max - low).
- **Founders vary by seed, and their ratio decided each world.** `start fox 1 / 5000` put
  7 .. 14 foxes on 117 .. 147 hens: 10.4% foxes to hens ate the flock out by day 8 .. 12,
  8.3% held it, 4.9 .. 6.8% let it grow 8 to 11 times. A flock yielded ~0.2 .. 0.3 chickens
  per hen a day. Count the founders per seed before reading anything else.
- **A density gate acts only above its density.** The litter's `count only chicken within
  16 >= 15` needed ~7 times the starting density: fox births were 0 a day at a mean of 30
  hens, 0.06 at 229, 0.75 .. 1.8 at 390 .. 480. Where foxes held the flock thinner they never
  bred and died of age. The territory gate (`count fox within 16 < 2`) never closed (~0.2
  other foxes per square). Find the binding gate (`--gates`) before relaxing one.
- **Every cause of death, not the obvious one.** Thirst killed 10 .. 21 chickens a day, as
  many as the foxes did.
- **Founders die in a wave.** `mortal(6d, 3)`: 11, 6, 2, 1 foxes on days 6 to 9. A window
  that starts before the wave measures the founders, not the world.
- **Measure with loosened thresholds.** A copy of the goal with every line made to hold,
  run with `lab --full`, gave every number on every seed. (`wmc scenario` then ran seeds one
  after another, 7 minutes for 8; it runs them in parallel now.)
- **A fast tier that is harsher, and long enough.** 192x192 for 16 days (96 s a variant
  against 158 s) had fewer founders, so harsher: a variant passing it should pass the full
  goal. A 10-day 256x256 run (81 s) was rejected: it ends before the founders age out,
  which is what the goal turned on.

The tools on worlds that are not ecosystems (this session):
- **Life variants by lab.** 128x128, 600 generations, 4 seeds, means from generation 300:
  B3/S23 5% alive, B3/S123 35%, B3/S234 51% (nearly frozen). A goal between two values of
  an integer knob needs a die: a lonely cell surviving with `chance(SURVIVE)` gave 5.3%,
  20.5%, 27%, 31%, 35% at 0, 25, 50, 75, 100. Concave: a quarter of the die, half the change.
- **Lockstep founders.** Villagers all born full rested at once (none at work for ~70
  minutes, every ~17 hours; observe's hourly samples missed it, the goal's minute checks
  did not). `energy = 12h + rand(12h)` at birth: 96 .. 117 of 117 at work at every check.
- **A transfer bigger than the gap it closes.** Gifts of 40 instead of 10 lowered the
  poorest villager from 183 to 154: givers became the new poorest.
- **When no lever of a mechanism moves a number, something else sets it.** Gap-sized gifts
  and three times as many gifts moved the poorest only from 157 (no gifts) to 186: the
  poorest founder started at 20 and drifts up ~165 a day, so ~185 at day 1, when the
  window opened.
- **A lab need not finish what cannot pass.** Once a variant has more failed seeds than
  `pass` allows, its other seeds do not run: the fox lab of 3 hopeless variants x 8 seeds
  went from 176 s to 45 s, same verdicts.

## 2026-10-29: the tools' first runs, on the built-in world

- **Trees die of thirst within a day of growing.** `wmc observe 2d 256 256 12`: trees went
  from 396 to 33 between day 0 18:00 and day 1 09:00, every death `no water`, with
  `became tree` about 198 a day. Likely cause, not yet confirmed: `start seed 1 / 100`
  puts seeds anywhere; a seed with no water within 2 cells holds a day of water and turns
  into a tree after about 20 lit thinks (about a day), just before it would die; `become`
  carries `water` by name, so the young tree starts nearly dry, and a tree's roots also
  reach only 2 cells. Check with `wmc why` on a young tree. Rule of thumb: `become` keeps
  a decaying need's level, so a kind born late in a dry life inherits the drought.
- **Who kills whom, at a glance.** Same run: hens died 80% `eaten by fox`, 20% `no water`;
  every bee death was `no food`; grass was 79% `eaten by chicken`, 21% by chicks. Reading
  the causes first would have saved the earlier sessions most of their guessing.
- **A first lab.** Bee goals on a 192x192 world for a day, 4 seeds, 3 to pass (`mean count
  bee >= 15 from 12h`, `mean born bee per day >= 20`, `mean died bee of food per day <=
  10`): `--set LOAD=15min,30min,1h` passed 1, 2 and 3 seeds of 4: only `LOAD=1h` met it. A
  bee carrying more per trip starves less. 12 runs in 6.5 s (dev build, 8 jobs).

## 2026-09-25/26: content tuning while building the actors (`docs/ACTORS.md` §11)

- **Family matching changes balance.** Letting the fox's litter rule count chicks (`count
  chicken`, which includes `chick`) made foxes boom: seed 12 at day 16 had 114 hens
  instead of 1004. Fixed with `count only chicken`. Rule of thumb: a count that gates
  births counts `only` the kind meant; check 16-day populations on several seeds after any
  content change.
- **Room, not food, capped the hens.** On 256x256 seed 12 the flock grew slowly to about
  1100 hens by day 16, held by the nesting gate (`count meat within 6 < 3`), not by grass.
- **Regrowth decides a meadow.** Grass healing with `chance(10)` a think let the hens graze
  it down to 15% by day 16; `chance(50)` holds it at about 24 000 tufts.
- **Behaviour before balance.** Hens that got stuck never laid, and foxes never hunted,
  until walking kept a heading with detours round rocks (`walker`), eggs came by day from day
  0, and foxes chased to where they last saw prey (`chase` memory). A population number
  means nothing while the actors are stuck: check `decides` and `results BLOCKED` first.
- **Bees.** A full hive refused every `give` and the colony deadlocked: the hive now shows
  `look 2` when full and bees spill elsewhere. Bees that burn food every tick need nectar
  about 15 times more energy-dense than a first guess. Cap colonies (`count bee within 8 <
  10`) and let bees remember a target instead of searching again: a bee think costs about
  1.4 µs.
- **Cost.** In a condition, dice before searches (`chance(10) and nearest ...`): the
  search runs only when the dice pass, -12% to -28% on the tick.
