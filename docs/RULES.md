# Writing rules

Every creature and plant in the world is a **kind** declared in a rules file in `rules/`.
The files are compiled into bytecode when a world opens, and every actor of a kind runs
that program against its own needs and memory. This is the author's reference; the design
and the reasons behind it are in `ACTORS.md`.

```
make run ARGS="lint rules/"                 # compile, print the kind table
make run ARGS="why saves/dev 77 103"        # what the actor at (77, 103) is thinking
WMC_RULES=my_rules make run ARGS="play saves/try --scenario my.scenario"
```

In `wmc play`, `r` recompiles the rules directory (`WMC_RULES`, else `./rules`) and swaps it
into the running world. Live actors keep their kind, needs, memory and state by name.

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

Files compile in file-name order, kinds in declaration order, and that order numbers the
kinds (`wmc lint` prints it). Kind, sub, const, tag and scent names are global across all
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

Declarations come first in a kind, then the reflex rules, then any `state` blocks.

| declaration | default | meaning |
|---|---|---|
| `glyph "c"` | `?` | one printable ASCII character |
| `color "#rrggbb"` | pale yellow | the glyph's colour |
| `cover` | no | ground cover (grass): lies under whoever stands on the cell, never blocks, never moves; eaten with `graze` |
| `tags a b ...` | none | names a predicate can match (`nearest meat within 8`); at most 64 tags in a rule set |
| `cadence N` | 8 | thinks every N ticks; a power of two |
| `sight N` | 4 | the largest radius any search reaches, 0 to 16 |
| `fuel N` | 512 | ops per think, 1 to 4096 |
| `food T` | 0 | what eating a whole one gives an eater (see `eat`) |
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

An actor starts in the first state. `next NAME` switches for the next think and ends this one
like an action; it can be combined with one action (`{ next FORAGE  move toward f }`).
`become` starts the new kind in its first state. `next` isn't allowed inside a sub.

## 6. Traits: sharing behaviour between kinds

A **trait** is a reusable piece of a kind: declarations, rules, states and subs, but no
glyph, no colour, and no actors of its own. A kind **extends** traits to include them, and
may extend one other kind.

```
trait drinker(thirsty) {                    # a parameter: a constant inside the trait
  need water max 4h vital
  mem knows_water, water_x, water_y
  when water < 30min and nearest water within 1 as w => drink w
  when nearest water within 6 as w => { knows_water = 1  water_x = x + w.dx  water_y = y + w.dy }
  when water < thirsty and knows_water == 1 => move toward at(water_x, water_y)
}

trait walker {
  mem heading, detour
  sub wander() { heading = turn(heading)  move dir(heading) }   # a member sub: sees heading
  when blocked    => { heading = rand(8) + 1  detour = 3 }
  when detour > 0 => { detour -= 1  move dir(heading) }
}

kind hen extends drinker(90min), walker {
  glyph "C"
  need food max 1d vital
  when nearest fox within 5 as f => flee(f)
  inherit drinker                           # the trait's rules run exactly here
  when food < 20h => forage(grass, 6)
  inherit walker
  when true => wander()
}

kind chick extends hen {                    # everything a hen is, except what it changes
  glyph "c"
  need food max 8h vital                    # redeclared: same slot, new max
  when age > 2d => become hen
  inherit drinker                           # takes the hen's drinking and walking,
  inherit walker                            # but not the hen's own rules
  when nearest only hen within 6 as m => move toward m
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
- **States:** by name. A state the kind does not declare is inherited whole.

**Where inherited rules run.** Each rule list, the reflexes and each state, is resolved on
its own:
- **No `inherit` in the list:** the parents' rules come after the kind's own.
- **`inherit`** runs every parent's rules at that point.
- **`inherit NAME`** runs that ancestor's rules, and only those, at that point. Anything a
  list doesn't `inherit` is left out, which is how the chick above skips the hen's rules.

A trait may name only the needs, memory, states and subs it or its own parents declare.
Every trait is compiled on its own to check this, even if no kind uses it yet, so a trait
that compiles works in any kind.

**Families.** A kind's name in a predicate matches that kind **and every kind that extends
it**: `nearest hen` sees chicks too. `only hen` matches hens alone. `spawn` and `become`
always name one exact kind. A trait is never matched: to find "anything that drinks", give
the trait a tag (`tags drinker`) and match the tag.

## 7. Statements

| statement | |
|---|---|
| `name = expr`, `name += expr`, `name -= expr` | a need, a mem slot or a local |
| `let name = expr` | a local; lives until the end of the rule body |
| `if cond { } else if cond { } else { }` | |
| `while cond { }`, `repeat n { }` | bounded by fuel |
| `for each pred within r as v { }` | once per matching cell in rings 1..r, in a fixed order (each ring clockwise from its top-left). The search's fuel is paid once. There is no `break`, so collect into locals and act after the loop |
| `choose { 3: stmt  2: { ... } }` | one weighted draw, runs that arm |
| `sub_name(args)` | call a sub (§12) |
| `return expr` | inside a sub |

## 8. Actions

Each action is one intent, settled after every actor has thought. The outcome is readable at
the next think as `result` (`OK`, `BLOCKED`, `MISSED`, `REFUSED`), or as the shorthands
`blocked`, `missed` and `refused`.

| action | what happens |
|---|---|
| `idle` | nothing; ends the rule |
| `die` | the actor is removed |
| `become K` | turns into kind K in place: needs carry by name (ticks-until-empty needs clamped to the new max, points needs reset to max), mem by name, state reset, age from now; not between standing and cover kinds |
| `spawn K at t [with (a, b)]` | a new K on cell t, needs full, mem zero or `a, b` in its first two slots. A standing kind needs a free cell; a cover kind needs a walkable cell without cover |
| `move t` | one step toward t; slides past a blocked cell by 45 degrees. Contested cells go to the actor with the lowest key this tick; a cell someone left or died on this tick can't be entered until the next. `BLOCKED` if it didn't move |
| `drink t` | t must be adjacent water: the need named `water` refills to max; else `REFUSED` |
| `eat t` | bites the standing actor at adjacent cell t, which needs `health`: takes up to `bite` of it, and gives the eater's `food` the same share of the victim's `food`. At 0 health the victim dies |
| `hit t` | like `eat`, without the food |
| `graze t` | like `eat`, on the ground cover at t, adjacent or `here` |
| `take t NEED n` | moves up to `n` of the adjacent actor's need named NEED into own NEED, never past own max. The target sees `taken` and wakes |
| `give t NEED n` | moves up to `n` of own NEED into the adjacent actor's NEED, never past its max |

Bites land before anyone moves, on either side of a chunk border, in key order. Several
eaters of one victim each get the share they took. Transfers are settled after bites, also
in key order.

## 9. Effects

Effects combine with the action and don't end the think.

| effect | |
|---|---|
| `look = v` | a public byte, 0 to 255: the palette variant, `kind:look` predicates, `look_of(t)` |
| `signal = v` | a public 16-bit value others read with `signal_of(t)`, like a bee's dance |
| `mark CH v` | adds `v` (0 to 255, saturating) to scent channel CH on the actor's cell. Scent fades by 1/32 every 16 ticks: gone in about 1.5 game hours. At most 4 channels in a rule set, numbered by first use |

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
| `ground`, `feature` | own cell's ground (`water`/`soil`) and feature (`rock`) |
| `scent(CH)`, `scent(CH, t)` | scent channel CH here, or at target t |
| `count pred within r` | matching cells in the square of radius r, **own cell included** |
| `free(t)` | the cell is walkable and nobody stands there |
| `is(t, pred)` | the cell matches pred (`is(here, grass)`) |
| `look_of(t)`, `signal_of(t)` | the public bytes of whoever stands at t, else of its cover; 0 for nobody |
| `dist(t)` | Chebyshev distance to t |
| `v.dx`, `v.dy` | a bound target's offset |

Searches (`count`, `nearest`, `sniff`, `for each`) are capped by `sight` and see into the
neighbouring chunks. An unloaded chunk reads as rock with nobody on it. `nearest` scans
rings 1 to r (not its own cell), and each ring starts at a random point so a flock doesn't
all pick the same target.

## 11. Targets and predicates

A **target** is a cell relative to the actor:

| target | |
|---|---|
| a binding (`f`, `w`), `here` | |
| `north`, `east`, `south`, `west`, `dir(h)` | one step; `dir(h)` for heading 1 to 8, clockwise from north |
| `toward t`, `away t` | one step toward or away from t |
| `at(x, y)` | a world position |
| `attacker` | where the lowest-key biter came from |
| `random free` | a free neighbour, if any |

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
rule's body finishes. Calls nest up to 8 deep. `take`, `give` and `next` belong to kinds, not
subs.

## 13. Cost

Each op costs 1 fuel, and a search costs `(2r + 1)² / 8` more. A think that runs out of fuel
becomes `idle`, sets `trapped`, and counts under `TRAPS` on the status bar and in
`wmc run`. The `ops/think` column of `wmc run` shows what each kind costs; a chicken runs
about 110 ops a think, grass about 20.

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
| `seed N` | 42 | the world's seed: terrain, placement and every actor's dice |
| `size W H` | 80 24 | the region generated at creation, rounded up to whole 64-cell chunks; the rest generates as the camera reaches it |
| `terrain NAME V ...` | below | `water_scale` 12 (lake size in cells), `water_level` 0.30 (roughly the share of water), `rock_on_soil` 0.04, `rock_on_water` 0.01 |
| `start K N / D` | | this share of walkable cells, everywhere in the unbounded world, starts as kind K |
| `start K at (X, Y)` | | one K on that cell, which must be walkable |

Each walkable cell draws one number in [0, 1), and the shares cut that range into intervals
in the order written: a cell starts at most one kind, the shares add up to at most 1, and
reordering the lines moves who starts where. An explicit start takes its cell, whatever the
shares would have put there. A cover kind starts in the cover layer. Everyone starts newborn,
needs full.

A scenario names kinds, so it has to fit the rules: a start naming a kind the rules don't
define, or a trait, refuses the world. `wmc lint <rules> --scenario <file>` checks that
without making one. `show`, `play`, `run` and `why` take `--scenario <file>` when they create
a world, and `[w h seed]` after the save directory override `size` and `seed`. Without one
they use `scenarios/default.scenario`, which starts the built-in kinds.

## 15. Debugging

- **`wmc lint rules/`** compiles and prints the kind table: numbering, needs, memory, entry
  points, each kind's parents and family. Errors come as `file:line:col: message`, and
  stop the compile. Warnings and notes come as `file:line:col: warning: message` and don't:
  a rule that can never run, or an ancestor whose rules no `inherit` splices.
- **Two actions in a row** are a compile error when the compiler can see both: a statement
  that acts on every path, then another action in the same block.
- **`wmc why [-v] <dir> <x> <y> [ticks [w h seed]]`** steps `ticks`, waits for the actor at
  (x, y) to think, and prints that think. It shows the actor's needs and memory, and every
  rule it checked: `FIRED`, `no` (condition false) or blank (not reached). Then the decision
  with its effects, what it wrote, the fuel spent, and after the real step where it went and
  its result. `-v` adds every op.
- **`TRAPS b3`** on the play status bar means three bee thinks ran out of fuel or faulted.
  `wmc why` on one of them shows where.
- **`r` in `wmc play`** reloads the rules. A compile error shows on the status bar and
  changes nothing. Actors keep kind, needs, memory, state and scent by name. Rows of a kind
  you removed are dropped. Saved chunks are rewritten to match, so reopen the save with the
  same rules (`WMC_RULES=...`).

## 16. Limits

| | |
|---|---|
| needs, mem slots per kind | 4, 12 |
| tags, scent channels per rule set | 64, 4 |
| states per kind | 64 |
| sight | 16 |
| fuel per think | 4096 |
| sub call depth | 8 |
| parameters and locals in one rule or sub | 16 slots (a target takes 2, a `for each` 5) |

Reserved words can't name a need, mem, local, kind, sub or constant. They are every keyword
in this document plus the sense names (`x`, `y`, `age`, `light`, `hour`, `day`, `kind`,
`look`, `signal`, `state`, `hurt`, `hurt_dir`, `result`, `ground`, `feature`, `taken`,
`trapped`). `wmc lint` says so when you hit one.
