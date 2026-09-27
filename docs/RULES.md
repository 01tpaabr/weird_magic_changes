# Writing rules

Every creature and plant in the world is a **kind** declared in a rules file in `rules/`.
The files are compiled into bytecode when a world opens, and every actor of a kind runs
that program against its own needs and memory. This is the author's reference; the design
and the reasons behind it are in `ACTORS.md`.

```
make run ARGS="lint rules/"                 # compile, print the kind table
make run ARGS="why saves/dev 77 103"        # what the actor at (77, 103) is thinking
make run ARGS="play saves/try --rules rules --rules my_pack --scenario my.scenario"
```

In `wmc play`, `r` recompiles the world's packs (§15; with the built-in rules, `./rules`) and
swaps them into the running world. Live actors keep their kind, needs, memory and state by
name.

## 1. A first kind

```
kind seed {
  glyph ","
  color "#7a3b12"
  tags plant feed
  cadence 512                       # thinks every 512 ticks
  sight 2
  food 3h                           # what an eater gains
  need water  max 1d vital          # dries out in a day away from water
  need health max 1 decay 0 vital   # one bite
  mem lit

  when count water within 2 > 0 => water = min(water + 6h, 1d)
  when light > 0                => lit += 1
  when lit >= 20                => become tree
}
```

Files compile in file-name order. Kinds are numbered in pre-order: roots in file then
declaration order, each kind's children right after it, so a family is one range of ids
(`wmc lint` prints the numbering). Kind, sub, const, tag and scent names are global across all
files. `#` starts a comment. `;` between statements is optional. A kind says nothing about
where it starts in a new world: a scenario does (§14).

## 2. How a think runs

An actor thinks once every `cadence` ticks, staggered per actor, and also on the tick after
it was hurt or had something taken from it. The game runs 8 ticks a second at 1x speed. A
day is 21 600 ticks: 900 an hour, 15 a minute.

1. **Decay.** Each need declared without `decay 0` loses one per tick since the last think.
   If a `vital` need is at 0, the think is `die` and no rule runs.
2. **Reflexes.** The rules outside any `state` block are scanned top to bottom.
3. **Current state.** Then the rules of the current `state` block, if the kind has states.
4. **First match.** A rule whose condition holds runs its whole body. If the body did an
   action or a `next`, the think ends there. If it did neither, scanning goes on to the
   next rule. So a body that only writes memory "falls through":
   ```
   when nearest water within 6 as w => { knows_water = 1  water_x = x + w.dx }   # falls through
   when blocked => { heading = rand(8) + 1 }                                     # falls through
   when detour > 0 => { detour -= 1  move dir(heading) }                         # acts: the think ends
   ```
5. **Nothing matched.** Falling off the end is `idle`.

**One action per think.** A second action traps. Effects (`look =`, `signal =`, `mark`) don't
count as actions and ride along with whichever action happens.

A think reads the world **as it was at the start of the tick**. Nothing anyone does this
tick is visible until the next one. The action is only an intent: moves, bites and
transfers are settled afterwards, and the outcome is in `result` at the next think.

## 3. Declarations

Declarations come first in a kind, then the reflex rules, then any `state` blocks. A body
gives each declaration once; `tags`, `need` and `mem` may repeat, with different names.

| declaration | default | meaning |
|---|---|---|
| `glyph "c"` | `?` | one printable ASCII character |
| `color "#rrggbb"` | pale yellow | the glyph's colour |
| `cover` | no | ground cover (grass): lies under whoever stands on the cell, never blocks, never moves; eaten with `graze` |
| `tags a b ...` | none | names a predicate can match (`nearest meat within 8`); at most 64 tags in a rule set; the list ends at the next declaration, `sub`, `when` or `state`, so no tag is named `food` |
| `cadence N` | 8 | thinks every N ticks; a power of two |
| `sight N` | 4 | the largest radius any search reaches, 0 to 16 |
| `fuel N` | 512 | ops per think, 1 to 4096 |
| `food T` | 0 | what eating a whole one gives an eater (see `eat`), 0 or more |
| `bite N` | 1 | health taken per `eat`/`hit`/`graze`, 0 to 255 |
| `need NAME max M [decay 0] [vital]` | | a counter, at most 4 per kind |
| `mem a, b, ...` | | memory slots, at most 12 per kind, all 0 at birth |

**Needs.** A need is an integer from 0 to its max. By default it is *ticks until empty*: it
loses one per tick, and a value like `water < 30min` reads naturally. With `decay 0` it is
points that only rules and actions change, like `health`. A `vital` need at 0 kills. Rules
read a need by its name and can assign to it, clamped to `0..max`
(`water = min(water + 6h, 1d)`). Some needs have fixed meanings for actions: `water` for
`drink`, `health` for bites, and `food` for what eating gives.

**Time literals** are ticks: `30min`, `4h`, `2d`.

## 4. Rules, conditions and bindings

```
when <condition> => <statement or { block }>
```

A condition is an expression (nonzero is true), combined with `and`, `or` and `not`, which
short-circuit. Two conditions also bind a name to a cell:

```
when nearest fox within 5 as f => flee(f)       # f is the nearest cell holding a fox
when sniff trail within 4 as v => move toward v # v is the cell with the most `trail` scent
```

A binding is only visible in the body if its search is a top-level conjunct: allowed under
`and`, not under `or` or `not`. **Order matters for cost**: put cheap tests and dice before
searches, so `when chance(10) and nearest chicken within 6 as m ...` only searches one time
in ten.

A condition only tests: a sub it calls must not act or `next`, through the subs that one
calls too (§12). That is a compile error, since the action would stand even when the
condition turns out false. In a body, `v = step()` and `if step() > 0 { }` are fine.

## 5. States

```
kind bee {
  ...
  when food < 2h and nectar > 0 => { nectar -= 2min  food += 30min }   # a reflex, in every state
  state FORAGE {
    when nectar >= LOAD => next HOME
    when true           => move random free
  }
  state HOME {
    when nearest hive within 1 as h => { give h nectar 1h  next FORAGE }
    when true => move toward at(home_x, home_y)
  }
}
```

An actor starts in the first state; in a kind that extends others, the first in the merged
order: the parents' states, then the kind's own (§6). `next NAME` switches for the next
think and ends this one like an action; it can be combined with one action (`{ next FORAGE  move toward f }`), and
a second `next` in the same block is an error.
`become` starts the new kind in its first state. `next` isn't allowed inside a file sub (a
member sub may use it, §6).

## 6. Traits: sharing behaviour between kinds

A **trait** is a reusable piece of a kind: declarations, rules, states and subs, but no
glyph, no colour, and no actors of its own. A kind **extends** traits to include them, and
may extend one other kind. The built-in kinds are written this way, on the traits of
`rules/lib.rules` (§16); any pack can use them.

```
trait grazer(hungry) {                    # a parameter: a constant inside the trait
  need food max 1d vital
  mem meals
  sub munch() { meals += 1  graze here }  # a member sub: sees `food` and `meals`
  when food < hungry and is(here, grass) => munch()
  when food < hungry and nearest grass within 6 as g => move toward g
}

kind sheep extends grazer(20h), drinker(6, 2h, 3), mortal(8d, 5) {   # the last two: lib.rules
  glyph "S"
  sight 6                                 # its searches reach 6 cells
  tags animal meat
  need health max 30 decay 0 vital
  inherit mortal
  when nearest fox within 6 as f => flee(f)
  inherit drinker                         # its rules run exactly here: water, detours
  inherit grazer
  when true => wander()                   # walker's member sub (a drinker is a walker)
}

kind lamb extends sheep {                 # everything a sheep is, except what it changes
  glyph "s"
  need health max 8 decay 0 vital         # redeclared: same slot, new max
  when age > 3d => become sheep
  when nearest fox within 6 as f => flee(f)
  inherit drinker                         # takes the sheep's drinking and grazing,
  inherit grazer                          # but not the sheep's own rules
  when nearest only sheep within 6 as m => move toward m
  when true => wander()
}
```

**What a kind gets from its parents.**
- **Declarations:** its own, else what its parents declare. `cadence`, `sight`, `fuel`,
  `food`, `bite`, glyph and colour must agree between parents, or the kind declares its
  own. Tags add up.
- **Needs and memory:** by name, the parents' first, then the kind's. Redeclaring a need
  changes its max, decay or vital in place. Two parents that declare the same need
  differently must be settled by the kind. The 4-need and 12-mem limits apply after
  merging.
- **Subs:** a sub inside a trait or kind is a *member sub*. It sees that trait's needs,
  memory and states, and may `take`, `give` and `next`. A kind can redefine a member sub,
  and the trait's rules then call the kind's version.
- **States:** by name, the parents' first, then the kind's new ones, so a kind that
  extends a trait with states starts in the trait's first state, even if the kind writes
  another state first. A state the kind does not declare is inherited whole.

**Where inherited rules run.** Each rule list, the reflexes and each state, is resolved on
its own:
- **No `inherit` in the list:** the parents' rules come after the kind's own.
- **`inherit`** runs every parent's rules at that point.
- **`inherit NAME`** runs that ancestor's list as that ancestor runs it (its own rules and
  whatever it inherits), at that point; a splice never brings the same rules twice.
  Anything a list doesn't `inherit` is left out, which is how the lamb above skips the
  sheep's own rules.

A trait may name only the needs, memory, states and subs it or its own parents declare.
Every trait is compiled on its own to check this, even if no kind uses it yet, so a trait
that compiles works in any kind.

**Families.** A kind's name in a predicate matches that kind **and every kind that extends
it**: `nearest sheep` sees lambs too, and the built-in `nearest chicken` sees chicks. `only
sheep` matches sheep alone. `spawn` and `become`
always name one exact kind. A trait is never matched: to find "anything that drinks", give
the trait a tag (`tags drinker`) and match the tag.

## 7. Statements

| statement | |
|---|---|
| `name = expr`, `name += expr`, `name -= expr` | a need, a mem slot or a local |
| `let name = expr` | a local; lives until the end of its block: the rule or sub body, an `if` or `else` branch, a `while`, `repeat` or `for each` body, or a `choose` arm |
| `if cond { } else if cond { } else { }` | |
| `while cond { }`, `repeat n { }` | bounded by fuel |
| `for each pred within r as v { }` | once per matching cell in rings 1..r, in a fixed order (each ring clockwise from its top-left). The search's fuel is paid once. There is no `break`, so collect into locals and act after the loop |
| `choose { 3: stmt  2: { ... } }` | one weighted draw, runs that arm. A negative weight counts as 0, and each weight is capped at 2^31 / (number of arms) |
| `sub_name(args)` | call a sub (§12) |
| `return expr`, `return` | inside a sub: leave it with a value, or with none. The value must start on the `return` line: a `return` with nothing after it on its line returns nothing, and the next line is the next statement or `choose` arm |

A name reads a local first (a `let`, an `as` binding, a sub's parameter), then a need or mem
slot, then a trait parameter, then a constant. A local may hide a trait parameter or a
constant, but not a need or mem slot the code can name: `let food = 5` in a kind with `need
food` is an error, as is a binding or a member sub's parameter of that name.

## 8. Actions

Each action is one intent, settled after every actor has thought. The outcome is readable at
the next think as `result` (`OK`, `BLOCKED`, `MISSED`, `REFUSED`), or as the shorthands
`blocked`, `missed` and `refused`.

| action | what happens |
|---|---|
| `idle` | nothing; ends the rule |
| `die` | the actor is removed |
| `become K` | turns into kind K in place: needs carry by name (ticks-until-empty needs clamped to the new max, points needs reset to max), mem by name, look and signal kept, state reset, age from now; not between standing and cover kinds |
| `spawn K at t [with (m = a, n = b)]` | a new K on cell t, needs full, memory zero except the (at most two) slots `with` names. A standing kind needs a free cell; a cover kind needs a walkable cell without cover. `BLOCKED` if the cell is taken, not loaded or out of reach (at least 64 cells each way, §11) |
| `move t` | one step toward t; slides past a blocked cell by 45 degrees. Contested cells go to the actor with the lowest key this tick; a cell someone left or died on this tick can't be entered until the next. `BLOCKED` if it didn't move |
| `drink t` | t must be adjacent water: the need named `water` refills to max; else `REFUSED` |
| `eat t` | bites the standing actor at adjacent cell t, which needs `health`: takes up to `bite` of it, and gives the eater's `food` the same share of the victim's `food`. At 0 health the victim dies |
| `hit t` | like `eat`, without the food |
| `graze t` | like `eat`, on the ground cover at t, adjacent or `here` |
| `take t NEED n` | moves up to `n` of the adjacent standing actor's need named NEED into own NEED, never past own max; never ground cover. The target sees `taken` and wakes, if anything moved. `BLOCKED` if nothing moved (own NEED full, the target's empty, or `n` is 0) |
| `give t NEED n` | moves up to `n` of own NEED into the adjacent standing actor's NEED, never past its max; never ground cover. `BLOCKED` if nothing moved |

Bites land before anyone moves, on either side of a chunk border, in key order. Several
eaters of one victim each get the share they took. Transfers are settled after bites, also
in key order. A bite, `take` or `give` finds a decaying need as it stands that tick, not as
it was at its owner's last think.

## 9. Effects

Effects combine with the action and don't end the think.

| effect | |
|---|---|
| `look = v` | a public byte, 0 to 255, that other actors read (`kind:look` predicates, `look_of(t)`) and `wmc why` shows; it is not drawn |
| `signal = v` | a public 16-bit value others read with `signal_of(t)`, like a bee's dance |
| `mark CH v` | adds `v` (0 to 255, saturating) to scent channel CH on the actor's cell. Several marks in one think add up, per channel, even from rules that fall through. Scent fades by 1/32 every 16 ticks: gone in about 1.5 game hours. At most 4 channels in a rule set, numbered by first use |

## 10. Senses

| sense | value |
|---|---|
| a need or mem name | its value |
| `age` | ticks since birth or `become` |
| `x`, `y` | world position |
| `kind`, `look`, `signal`, `state` | own public bytes and state index |
| `light` | daylight, 0 (night) to 255 |
| `hour`, `day` | clock, 0 to 23, and days since the world began |
| `hurt`, `hurt_dir` | damage taken since the last think; direction (1 to 8, clockwise from north) of the lowest-key attacker. Also the target `attacker` |
| `result`, `blocked`, `missed`, `refused` | the last action's outcome |
| `taken` | something was taken from it since its last think |
| `trapped` | its last think ran out of fuel or faulted |
| `scent(CH)`, `scent(CH, t)` | scent channel CH here, or at target t |
| `count pred within r` | matching cells in the square of radius r, **own cell included**. The radius is one term (a number, a name, a call or a parenthesised expression): `count fox within 3 - 1` is the count minus one; write `within (R - 1)` for a sum |
| `free(t)` | the cell is walkable and nobody stands there |
| `is(t, pred)` | the cell matches pred (`is(here, grass)`); `is(here, water)`, `is(here, rock)` ask about the ground and the feature |
| `look_of(t)`, `signal_of(t)` | the public bytes of whoever stands at t, else of its cover; 0 for nobody |
| `dist(t)` | Chebyshev distance to t |
| `v.dx`, `v.dy` | a bound target's offset |

Searches (`count`, `nearest`, `sniff`, `for each`) are capped by `sight` and see into the
neighbouring chunks. An unloaded chunk reads as rock with nobody on it, and so does any
cell out of reach (§11). `nearest` scans
rings 1 to r (not its own cell), and each ring starts at a random point so a flock doesn't
all pick the same target.

## 11. Targets and predicates

A **target** is a cell relative to the actor:

| target | |
|---|---|
| a binding (`f`, `w`), `here` | |
| `north`, `east`, `south`, `west`, `dir(h)` | one step; `dir(h)` for heading 1 to 8, clockwise from north |
| `toward t`, `away t` | one step toward or away from t |
| `at(x, y)` | a world position, within reach |
| `attacker` | where the lowest-key biter came from |
| `random free` | a free neighbour, if any: one of the 8, whatever the kind's `sight` (not a search) |

**Reach.** A target reaches the actor's chunk and the eight around it: at least 64 cells
each way (up to 127, depending on where the actor stands in its chunk). Beyond, a target
reads as rock with nobody on it, even where the world is loaded, and a spawn there is
`BLOCKED`.

A **predicate** says what a cell must hold:

| predicate | matches |
|---|---|
| a kind (`fox`) or a tag (`meat`) | whoever stands there, or the cover there; a kind matches its whole family (§6) |
| `only fox` | that kind exactly, not the kinds that extend it |
| `kind:look` (`flower:1`), `only kind:look` | that kind (family, or exactly) showing that look |
| `water`, `soil`, `rock` | the ground, the feature |
| `free` | walkable, nobody standing |
| `bare` | walkable, no ground cover |

## 12. Expressions, subs and constants

Every value is a 32-bit integer; there are no floats. Arithmetic wraps, `x / 0` and
`x % 0` are 0. Operators: `+ - * / %`, `< <= == != >= >`, `and or not`. Functions:
`min`, `max`, `abs`, `sign`, `clamp(x, lo, hi)`, `rand(n)` (0 to n-1), `chance(p)`
(p percent), `pack(a, b)` / `hi(v)` / `lo(v)` (two signed bytes in one value, for signals).
Randomness is drawn per actor per tick from the world seed, so a replay draws the same.

```
const LOAD = 30min                  # folded at compile time; visible everywhere

sub flee(t: target) {               # parameters are ints unless typed `target` or `pred`
  if free(away t) { move away t } else { move random free }
}
sub turn(h) {                       # returns a value: usable in expressions
  if h == 0 { return rand(8) + 1 }
  choose { 80: return h  20: return rand(8) + 1 }
}
```

A sub sees only its parameters, its locals and constants, never a kind's needs or memory,
so every kind can call it. It may act (`flee` moves); the think then ends when the calling
rule's body finishes. A sub that may act or `next` (itself, or through any sub it calls)
can't be called from a `when` condition (§4). Calls nest up to 8 deep. `take`, `give` and `next` belong to kinds, not
subs.

## 13. Cost

Each op costs 1 fuel, and a search costs `(2r + 1)² / 8` more. A think that runs out of fuel
becomes `idle`, sets `trapped`, and counts under `TRAPS` on the status bar and in
`wmc run`. So does any other trap (a second action, a fault). Writes to needs and mem made
before the trap stay; the action, `next`, `look`, `signal` and `mark` are dropped. Write
the memory a trap must not half-update last, or after the search that might run out.
The `ops/think` column of `wmc run` counts ops, not fuel: a search's extra
`(2r + 1)² / 8` is not in it, so a kind that searches a lot costs more than the column
shows. `wmc why` prints both. A chicken runs about 110 ops a think, grass about 20.

Ways to keep a kind cheap:
- **Think less often.** A higher `cadence` is the biggest lever. Plants think every 256 to
  512 ticks; chickens every 4.
- **Dice before searches.** `when chance(5) and count bee within 8 < 10 ...`
- **Remember instead of searching again.** Store a target in mem (`tx`, `ty`) and walk to
  it in a state that doesn't search; search again on arrival.
- **Search less while wandering.** A wandering actor can look only every few thinks
  (`rest` in the bee).
- **Keep radii small.** A radius-8 search reads 289 cells, radius 16 reads 1089.

## 14. Scenarios

Rules say what kinds are and how they behave. Where they start is the world's business: a
new world is made from a **scenario**, a small text file in `scenarios/`. A save keeps its
scenario, so every part of the map generates the same way whenever it is first visited.

```
# scenarios/meadow.scenario
seed 12
size 256 256                                   # the region generated at creation, in cells
terrain water_level 0.18 rock_on_soil 0.02     # knobs of the terrain noise
start chicken 1 / 400                          # this share of walkable cells
start grass   1 / 20
start hive at (77, 103)                        # exactly there
```

| statement | default | meaning |
|---|---|---|
| `rules PATH ...` | the built-in rules | the packs the world runs (§15), relative to the scenario file |
| `seed N` | 42 | the world's seed: terrain, placement and every actor's dice |
| `size W H` | 80 24 | the region generated at creation, rounded up to whole 64-cell chunks, at most 4096 of them (4096 x 4096 cells); the rest generates as the camera reaches it |
| `terrain NAME V ...` | below | `water_scale` 12 (lake size in cells), `water_level` 0.30 (roughly the share of water), `rock_on_soil` 0.04, `rock_on_water` 0.01 |
| `start K N / D` | | this share of walkable cells, everywhere in the unbounded world, starts as kind K |
| `start K at (X, Y) [with (NAME = V, ...)]` | | one K on that cell, which must be walkable; `with` sets its needs or memory by name (`food = 2h`, `heading = 3`) |
| `map { ... }` | | cells drawn from (0, 0), one ASCII character each, one row per line, every row as long as the first; the rows stand alone on their lines, with no comments |
| `legend { ... }` | | what each map character stands for, one entry per line: `soil`, `water`, `rock`, or a kind, which stands on soil and may take a `with`; a character the map never uses is an error (delete its line) |
| `outside noise` | `noise` | beyond the map: the seed's noise, or all `soil`, `rock` or `water` |

`seed`, `size`, `map`, `legend` and `outside` come once, and each terrain field once (several
`terrain` lines add up); `rules` and `start` lines add up.

Each walkable cell draws one number in [0, 1), and the shares cut that range into intervals
in the order written: a cell starts at most one kind, the shares add up to at most 1, and
reordering the lines moves who starts where. A share is exact to one part in 2^24 (the smallest is 1 / 16777216), so
halves, quarters and eighths add up exactly (`start alive 3 / 8` and `start dead 5 / 8` fill
every cell), while `1 / 3` and `2 / 3` leave about one cell in 16.7 million empty. An explicit start takes its cell, whatever the
shares would have put there. A cover kind starts in the cover layer. Everyone starts newborn,
needs full, but for what `with` sets.

**A drawn map.** A scenario can draw its ground instead of rolling it. The map's size is the
scenario's `size` unless you give a larger one (leave `size` out to use the map's; a smaller
one is an error), shares still apply to its walkable cells, and every kind character becomes
a `start K at (x, y)`. This is the fox and the
cornered hen of `scenarios/tests/fox_pen.scenario`, with the pen drawn:

```
seed 3
outside soil
map {
  .....
  .###.
  .FC#.
  .###.
}
legend {
  . soil
  # rock
  C chicken
  F fox with (food = 2h)     # starving: it hunts at once
}
```

In a legend, the first character on the line is the one it defines, so `#` can stand for
rock; after it, `#` starts a comment as usual.

**Testing a kind.** A scenario can end in a test: `run` steps the world, `expect` checks
it, as many times as you like, in the order written. `wmc scenario <file>` makes the world
(nowhere on disk), runs the lines, prints each expectation with what it found and the final
checksums, and fails if an expectation does. Every scenario in `scenarios/` and
`scenarios/tests/` runs in `make test`, at 1 and 8 threads, and must end with the same
checksum at both; one without `run` lines only makes its world. A test sees only the
initial region (`size`, in whole 64-cell chunks): nothing streams in, so a `start K at`
outside it is never placed.

```
# scenarios/tests/eggs_hatch.scenario
seed 4
terrain water_level 0 rock_on_soil 0 rock_on_water 0   # flat soil
start chicken at (30, 30) with (food = 6h)
start seed at (31, 30)
start seed at (33, 30)
start egg at (10, 10)
run 64
expect count seed == 0                  # both eaten
expect max food of chicken > 710min
run 7h
expect count egg == 1                   # not yet: an egg hatches past eight hours
run 2h
expect became chick == 1
```

| line | |
|---|---|
| `run T` | step T ticks (`64`, `90min`, `2h`, `1d`) |
| `expect count K OP N` | actors of K alive now |
| `expect born\|became\|eaten\|died K OP N` | the life counters so far: born of a spawn, became K, eaten, died |
| `expect thinks\|traps K OP N` | the thinks run so far, and those that trapped (out of fuel, a second action, a fault; `expect traps K == 0` guards against a rules bug), as `wmc run` prints them |
| `expect min\|max\|sum NAME of K OP V` | a need (as it stands now, decayed since the last think) or memory over every actor of K (no actor: the check fails) |
| `expect at (X, Y) K` | the standing actor there, else the cover, is a K; `nobody` for an empty cell |
| `expect checksum HEX`, `expect state HEX` | the world, with and without the rules hash |

`OP` is one of `== != < <= > >=`. A kind means its family, as in the rules; `only K` means
K alone (`expect count only chick == 1`).

A scenario names kinds, so it has to fit the rules: a start naming a kind the rules don't
define, or a trait, refuses the world, and so does one on water or rock, two on one cell, or
a `with` its kind lacks or past a need's range. The error gives the start's line (a drawn
kind's is its map row) and a need in its units: ``pen.scenario:5: `fox` at (1, 1) with
`food = 27000 (30h)`: `food` holds 0 to 21600 (1d)``. `wmc lint <rules> --scenario <file>` checks that,
and the kinds, needs and memories its `expect` lines name, without making one. `show`, `play`, `run` and `why` take `--scenario <file>` when they create
a world, and `[w h seed]` after the save directory override `size` and `seed` (held to the
same limits, and no smaller than a drawn map). Without one
they use `scenarios/default.scenario`, which starts the built-in kinds.

## 15. Packs

A **pack** is a directory of `*.rules` files (one with none is an error), or one file. A
world's rules are one or more packs compiled together, in the order given, files sorted by
name inside each. Every kind,
trait, file sub, const, tag and scent name is global across all of them: a kind in one pack
can extend a kind or trait of another, call its subs and use its constants, and a name
declared twice, in any two files, is an error naming both. A sub declared inside a kind or
trait is that item's own: two items may each have a `wander`, but it can't share a name with
a file sub, a kind or a trait. A pack given twice loads once. With several packs an error
names a file `pack/file.rules`, with more parent directories where two packs share a name.

```
wmc play saves/zoo --rules rules --rules mods/wolves     # the built-in kinds, then a mod's
WMC_RULES=rules:mods/wolves wmc run saves/zoo 1000       # the same, for any command
wmc lint rules mods/wolves                               # check them together
```

A scenario can name its world's packs, relative to the scenario file, so it runs like any
other with nothing but `--scenario`:

```
rules ../packs/life                 # in scenarios/life.scenario
```

A save remembers its packs, by absolute path. So a new world runs `--rules`, else
`WMC_RULES`, else its scenario's `rules`, else the built-in kinds; a saved one runs
`--rules`, else `WMC_RULES`, else the packs it was saved with. If one of those is gone it
says so and uses the built-in rules. `wmc lint --scenario <file>`, with no packs (no
`--rules`, positional pack or `WMC_RULES`), lints the ones the scenario names, the same order
as for a new world.

A pack need not build on the built-in kinds. `packs/life` is Conway's Game of Life in two
kinds, `dead` and `alive`, and nothing else. `scenarios/life.scenario` plays it on an
endless board of random soup; `scenarios/tests/life_soup.scenario` checks a drawn soup's
live count against a plain Life's up to generation 100, and `life_patterns.scenario` tests
a blinker, a block and a glider.

A save also opens under packs that number things differently, which is what adding a pack
does. Kinds, needs, memory, states and scent channels are matched **by name**, so every actor
keeps its kind, cell, needs (clamped to a lowered max) and memory. The first time the world
writes to the save, the whole directory moves over to the new numbering, and from then on it
belongs to the new set of packs. Rules that lack one of the save's kinds are refused with the
list, and so is a kind that turned from standing into ground cover, or back.

## 16. The vocabulary

The built-in rules define names that other packs can build on: a fox from another pack
hunts the same `meat`, a mod's flower feeds the same bees. Only the three needs below mean
anything to the engine; the rest are conventions, which is what lets kinds from different
authors meet.

**Needs the engine reads.** `water` is what `drink` fills, to its max. `health` is what bites
take (`eat`, `hit`, `graze` take `bite` points each), and a `vital` health at 0 kills. `food`
is what an eater gains: the victim's `food` declaration, by the share of its health the bite
took. Any other need belongs to the rules, like `nectar`, which flowers, hives and bees move
with `take` and `give`.

**Tags.** `animal` (chickens, chicks, foxes, bees), `meat` (what foxes hunt: chickens, chicks,
eggs), `plant` (flowers, grass, seeds, trees), `feed` (what hens eat: grass and seeds).

**Looks.** `chicken:1` roosting, `fox:1` asleep, `flower:1` rich in nectar, `bee:2` dancing (its
`signal` packs the offset to the flowers, read with `hi` and `lo`), `hive:1` has stores,
`hive:2` full.

**Scents.** `trail`, laid by laden bees on their way home, strongest near the flowers.

**Families.** `chick extends chicken`: `nearest chicken` finds chicks too, `only chicken` does
not.

**Subs** (`rules/lib.rules`): `flee(t)` away from a target, else anywhere free; `peck(what,
r)` walks to the nearest standing `what` within `r` and eats it; `forage(what, r)` grazes
cover `what` underfoot, else walks onto the nearest; `turn(h)` the next heading of a
meandering walk.

**Traits** (`rules/lib.rules`, and `fowl` in `rules/animals.rules`):

| trait | what a kind gets |
|---|---|
| `walker(steps)` | `mem heading, detour` and the member sub `wander()`, one step of a walk that keeps its heading and turns now and then. Rules: a `blocked` step picks a new heading for `steps` steps (a detour), and a detour runs before anything else. `inherit walker` above the rules that move toward something |
| `drinker(seen, thirsty, steps)` | a walker (`walker(steps)`), `need water max 4h vital` unless the kind declares its own, `mem knows_water, water_x, water_y`. Rules: remember the nearest water within `seen`; the walker's; below `thirsty` drink water next to it, else walk back to the water it remembers |
| `rooted(reach, low, full)` | `need water max full vital`. Rule: below `low`, water within `reach` fills it to `full` |
| `mortal(after, odds)` | Rule: past age `after`, each think is its last with `odds` chances in 100 000 |
| `fowl` | a `drinker(6, 90min, 3)` with `need food max 1d vital`. Rules: flee when hurt, drink when desperate (below 30 minutes, fox or not), flee a fox within 5, the drinker's, eat seeds and grass when below 20 hours |

The built-in kinds, as a reading list: `chicken extends mortal(5d, 6), fowl`; `chick extends
chicken` (fowl's rules, not a hen's); `fox extends mortal(6d, 3), drinker(8, 6h, 2)`; `flower
extends rooted(6, 1d, 1d), mortal(4d, 300)`; `bee extends mortal(3d, 40), walker(2)`; `grass
extends rooted(8, 1d, 2d)`; `seed extends rooted(2, 1d, 1d)`; `tree extends rooted(2, 3d,
3d)`; `egg` and `hive` stand alone.

## 17. Debugging

- **`wmc lint rules/`** compiles and prints the kind table: numbering, needs, memory, entry
  points, each kind's parents and family. Errors come as `file:line:col: message`, and
  stop the compile. Warnings and notes come as `file:line:col: warning: message` and don't.
  `wmc lint --strict` fails on any warning, for CI. A world prints its rules' warnings once
  on stderr when it opens, and a hot reload says `N warnings (log)`. What the lint checks:
  - a rule that can never run; an ancestor whose rules no `inherit` splices (a note);
  - a predicate tag no kind carries (`wolf looks for meat, but no kind ... is tagged meat`);
  - a `sniff` or `scent()` of a channel nothing marks, `signal_of` when no rule sets
    `signal`, `K:n` (n above 0: every actor starts at look 0) when no rule of K's family,
    nor of a kind that becomes one, sets `look`;
  - `eat`, `hit` or `graze` of a target whose predicate the lint can follow (the `nearest`
    that bound it, or a sub's `pred` argument at each call) where a matching kind has no
    `health`; an eater with no `food` need; a drinker with no `water` need;
  - the wrong layer: `eat`, `hit`, `take` or `give` of a target that only ground cover
    matches, `graze` of one that only standing kinds match, `become` between a standing
    and a cover kind (always `REFUSED`);
  - a search radius that is a constant above the kind's `sight` (it is clamped);
  - a decaying vital need whose max is at most the kind's cadence (it empties by the next
    think, even when refilled);
  - an action inside `for each`, or a call there to a sub that acts (a second cell means a
    second action: a trap; `next` is not an action);
  - what is never used: a mem, a need the engine does not read (it reads `health`,
    `water`, `food`), a sub, a const, a state no `next` reaches (the first is where actors
    start), a tag no predicate names (a note);
  - with `--scenario`, a kind that never appears: the scenario starts none, and nothing
    that appears spawns or becomes one; a `start K at` outside the initial region (a note).
- **Two actions in a row** are a compile error when the compiler can see both: a statement
  that acts on every path, then another action in the same block.
- **`wmc why [-v] <dir> <x> <y> [ticks [w h seed]]`** steps `ticks`, waits for the actor at
  (x, y) to think (up to its cadence, at least a day), and prints that think. It shows the
  actor's needs and memory, and every rule it checked: `FIRED`, `no` (condition false),
  `TRAPPED` (the think trapped in its condition) or blank (not reached). Then the decision
  with its effects, what it wrote, the fuel spent, and after the real step where it went and
  its result. `-v` adds every op, up to the one that trapped.
- **`TRAPS b3`** on the play status bar means three bee thinks ran out of fuel or faulted.
  `wmc why` on one of them shows where.
- **`r` in `wmc play`** reloads the rules. A compile error shows on the status bar and
  changes nothing. Actors keep kind, needs, memory, state and scent by name. Rows of a kind
  you removed are dropped: the one way to take a kind out of a save. Saved chunks are
  rewritten to match.

## 18. Limits

| | |
|---|---|
| needs, mem slots per kind | 4, 12 |
| tags, scent channels per rule set | 64, 4 |
| states per kind | 64 |
| sight | 16 |
| fuel per think | 4096 |
| sub call depth | 8 |
| value stack per think | 64 (shared by a rule and the subs it calls; a rule or sub that needs more by itself is an error: nest less deeply or split with `let`) |
| parameters and locals in one rule or sub | 16 slots (a target takes 2, a `for each` 5, a `choose` 1 per arm plus 1, a `repeat` 1, and `toward`, `away` and `random free` 2 while evaluated) |
| nesting in one rule or sub | 128 levels: each nested statement, parenthesis, call argument, operand, `-`, `not`, `toward` and `away` is one, and so is each operator in a chain (`(m) > 0` is three: the parenthesis, `m` and `>`) |
| compiled size of one rule, one state's rules, one sub | 32767 ops (jumps are 16 bits; split what is longer) |
| distinct constants outside -32768..32767 per rule set | 65536 |
| kinds, subs per rule set | 65534, 65536 (each kind's member subs count once per kind that has them) |
| chunks a scenario's `size` generates at creation | 4096 (4096 x 4096 cells) |

Reserved words can't name a need, mem, local, kind, sub or constant. They are every keyword
in this document, the built-in functions among them (`min`, `max`, `abs`, `sign`, `clamp`,
`rand`, `chance`, `dist`, `free`, `is`, ...), plus the sense names (`x`, `y`, `age`,
`light`, `hour`, `day`, `kind`, `look`, `signal`, `state`, `hurt`, `hurt_dir`, `result`,
`taken`, `trapped`). `wmc lint` says so when you hit one. `water`,
`soil`, `rock`, `bare` and `food` are not reserved: a kind may have `need water`. But
`water`, `soil`, `rock` and `bare` always mean the predicate where one is read, so they can't
name a kind or a tag.
