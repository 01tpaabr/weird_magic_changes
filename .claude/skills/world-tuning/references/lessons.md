# Lessons

What tuning this project's worlds has taught, newest first. Each entry: what was tried, what
happened (with numbers), why, and the rule of thumb. Add one at the end of every tuning
(protocol step 8); keep the numbers, they are what makes a lesson reusable.

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
