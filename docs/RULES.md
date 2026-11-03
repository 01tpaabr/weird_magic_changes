# Writing rules

Every creature and plant in the world is a **kind** declared in a rules file in `rules/`.
The files are compiled into bytecode when a world opens, and every actor of a kind runs
that program against its own needs and memory. This is the author's reference; the design
and the reasons behind it are in `ACTORS.md`.

```sh
make run ARGS="lint rules/"                 # compile, print the kind table
make run ARGS="why saves/dev 77 103"        # what the actor at (77, 103) is thinking
make run ARGS="play saves/try --rules rules --rules my_pack --scenario my.scenario"
```

In `wmc play`, `r` recompiles the world's packs (§15; with the built-in rules, `./rules`) and
swaps them into the running world. Live actors keep their kind, needs, memory and state by
name.

Every example here runs in `make test` (`crates/sim-core/tests/docs.rs`), so each fence says
what it holds. ` ```rules ` compiles with the built-in rules, without a warning: a kind,
trait, sub or const it declares replaces the built-in one of that name, and bare rules are
the rules of a kind that extends `drinker(6, 2h, 3)` with `sight 8`. ` ```scenario ` fits
the rules it names, else the built-in ones, and passes its `expect` lines; one headed with a
file's path is that file, abridged. A block that shows a mistake adds `error` to the fence
(` ```rules error `) and ends the line that fails with `# error: ` and part of the message.
` ```sh ` and ` ```text ` are not run.

## 1. A first kind

```rules
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
(`wmc lint` prints the numbering). Kind, sub, const, tag and field names are global across all
files. `#` starts a comment. `;` after a statement, a declaration or a `const` is optional. A kind says nothing about
where it starts in a new world: a scenario does (§14).
The formal grammar, token by token, is `GRAMMAR.md`.

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
   ```rules
   when nearest water within 6 as w => { knows_water = 1  water_x = x + w.dx }   # falls through
   when blocked => { heading = rand(8) + 1 }                                     # falls through
   when detour > 0 => { detour -= 1  move dir(heading) }                         # acts: the think ends
   ```
5. **Nothing matched.** Falling off the end is `idle`.

**One action per think.** A second action traps. Effects (`look =`, `signal =`, `add`) don't
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
| `layer NAME` | standing | the declared layer (§12, Layers) its actors live in, one a cell, apart from who stands there and the cover; not with `cover` |
| `solid` | no | drawn as a whole cell of its colour, no glyph (a solid cover: the background of whoever stands on it); for pictures and boards. Only the renderer reads it: `wmc show` and the inspector still use the glyph |
| `tags a b ...` | none | names a predicate can match (`nearest meat within 8`); at most 64 tags in a rule set, less one for each layer up to the last that rests on a kind (a resting layer's bit is fixed by its place among the layers); the list ends at the next declaration, `sub`, `when`, `inherit`, `state` or `}`, so no tag is named `food` |
| `cadence N` | 8 | thinks every N ticks; a power of two |
| `sight N` | 4 | the largest radius any search reaches, 0 to 64 |
| `fuel N` | 512 | ops per think, 1 to 32768 |
| `food T` | 0 | what eating a whole one gives an eater (see `eat`), 0 or more |
| `bite N` | 1 | health taken per `eat`/`hit`/`graze`, 0 to 255 |
| `reach N` | 1 | how far `eat`, `hit` and `graze` land: a target up to N cells away, 1 to 16. The rules check what is in the way (`sees`) |
| `climb N on F` | none | `move` takes no step that rises more than N in field F (a height): it slides past one, as past a taken cell, and is `BLOCKED` if every way is too steep |
| `need NAME max M [decay 0\|1] [vital]` | `decay 1` | a counter, at most 4 per kind; `decay 1` loses one per tick (ticks until empty), `decay 0` never decays (points). `decay` is a switch, not a rate: any other number is an error |
| `mem a, b, ...` | | memory slots, at most 32 per kind, all 0 at birth; `mem route[8]` is an array of 8 of them, `route[i]` (below) |

**Needs.** A need is an integer from 0 to its max. By default it is *ticks until empty*: it
loses one per tick, and a value like `water < 30min` reads naturally; `decay 1` says so
explicitly. With `decay 0` it is points that only rules and actions change, like `health`.
There is no faster decay: `decay 2` is an error; a need that should run out sooner takes a
smaller max, or a rule that subtracts from it. A `vital` need at 0 kills. Rules
read a need by its name and can assign to it, clamped to `0..max`
(`water = min(water + 6h, 1d)`). Some needs have fixed meanings for actions: `water` for
`drink`, `health` for bites, and `food` for what eating gives.

**Time literals** are ticks: `30min`, `4h`, `2d`.

## 4. Rules, conditions and bindings

```text
when <condition> => <statement or { block }>
```

A condition is an expression (nonzero is true), combined with `and`, `or` and `not`, which
short-circuit. Two conditions also bind a name to a cell:

```rules
when nearest fox within 5 as f => flee(f)       # f is the nearest cell holding a fox
when best trail within 4 as v => move toward v  # v is the cell with the most of the field `trail`
```

A binding is only visible in the body if its search is a top-level conjunct: allowed under
`and`, not under `or` or `not`. **Order matters for cost**: put cheap tests and dice before
searches, so `when chance(10) and nearest chicken within 6 as m ...` only searches one time
in ten.

A condition only tests: a sub it calls must not act or `next`, through the subs that one
calls too (§12). That is a compile error, since the action would stand even when the
condition turns out false. In a body, `v = step()` and `if step() > 0 { }` are fine.

## 5. States

```rules
kind bee {
  glyph "b"
  need food   max 4h vital
  need nectar max 1h decay 0          # the crop
  mem home_x, home_y                  # the hive sets them when it spawns the bee
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

```rules
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
| `choose { 3: stmt  2: { ... } }` | one weighted draw, runs that arm. A negative weight counts as 0, and each weight is capped at (2^31 - 1) / (number of arms) |
| `sub_name(args)` | call a sub (§12) |
| `return expr`, `return` | inside a sub: leave it with a value, or with none. The value must start on the `return` line: a `return` with nothing after it on its line returns nothing, and the next line is the next statement or `choose` arm |

A name reads a local first (a `let`, an `as` binding, a sub's parameter), then a need or mem
slot, then a trait parameter, then a constant. A need or mem can't be named like a constant (an error).
A local may hide a trait parameter or a constant, but not a need or mem slot the code can name: `let food = 5` in a kind with `need
food` is an error, as is a binding or a member sub's parameter of that name.

```rules error
kind hen {
  need food max 1d vital
  when true => { let food = 5  idle }   # error: has the name of a need or mem slot
}
```

## 8. Actions

Each action is one intent, settled after every actor has thought. The outcome is readable at
the next think as `result`, a number: 1 OK, 2 BLOCKED, 3 MISSED, 4 REFUSED (0 before the
first). Those names are not words of the language; the last three read as the shorthands
`blocked`, `missed` and `refused`.

| action | what happens |
|---|---|
| `idle` | nothing; ends the rule |
| `die` | the actor is removed |
| `become K` | turns into kind K in place: needs carry by name (ticks-until-empty needs clamped to the new max, points needs reset to max), mem by name, look and signal kept, state reset, age from now; not between layers (standing, cover, a declared one), not for or into a big kind (§12, Big actors), nor into a kind that holds fewer heads (`REFUSED`) |
| `spawn K at t [with (m = a, n = b)]` | a new K on cell t, needs full, memory zero except the (at most two) slots `with` names. A standing kind needs a free cell; a cover kind needs a walkable cell without cover; a kind in a declared layer a cell with nobody in that layer, walkable unless the layer flies. `BLOCKED` if the cell is taken, not loaded or out of reach (at least 64 cells each way, §11) |
| `move t` | one step toward t; slides past a blocked cell by 45 degrees. Contested cells go to the actor with the lowest key this tick; a cell someone left or died on this tick can't be entered until the next. `BLOCKED` if it didn't move |
| `drink t` | t must be adjacent water: the need named `water` refills to max; else `REFUSED` |
| `eat t` | bites the standing actor at adjacent cell t (from a declared layer also `here`: the one standing under it), which needs `health`: takes up to `bite` of it, and gives the eater's `food` the same share of the victim's `food`. At 0 health the victim dies |
| `hit t` | like `eat`, without the food |
| `graze t` | like `eat`, on the ground cover at t, adjacent or `here` |
| `take t NEED n` | moves up to `n` of the adjacent standing actor's need named NEED into own NEED, never past own max; never ground cover. The target sees `taken` and wakes, if anything moved. `BLOCKED` if nothing moved (own NEED full, the target's empty, or `n` is 0) |
| `give t NEED n` | moves up to `n` of own NEED into the adjacent standing actor's NEED, never past its max; never ground cover. `BLOCKED` if nothing moved |
| `build M at t` | lays ground or feature M (the rules' own, or soil, water, rock) on the adjacent cell t, which must be walkable and empty at tick start; claimed like a move, so the lowest key wins it. A feature goes only where there is none; whatever makes the cell unwalkable only where no ground cover lies and nobody is in a declared layer that does not fly. `BLOCKED` if it may not; `REFUSED` for a cell not adjacent. In a chunk or across its border, as `spawn` is |
| `dig t` | clears the feature of t, adjacent or underfoot: a rock, a wall. `BLOCKED` if there is none; `REFUSED` for a cell not adjacent |
| `harvest F n` | moves up to `n` of field F on the actor's own cell into its need of the same name, never past the need's max (§12, Fields). `BLOCKED` if nothing moved; a `fixed` field can't be harvested |
| `drop F n` | moves up to `n` of the need named F into field F on the actor's own cell, never past the field's max. `BLOCKED` if nothing moved |
| `split n at t` | a group (§12, Groups) puts `n` of its heads, at least one and fewer than it has, into a new group of its kind on the adjacent cell t, which needs room as a `spawn` does (and counts as one under a `budget`). The new group starts as the old one is: needs, memory, state, age. `REFUSED` for a cell not adjacent or an `n` out of range; `BLOCKED` if the cell is taken |
| `join t` | a group gives all its heads to the group of its kind on the adjacent cell t, in its layer, and is gone; the one joined keeps its own needs and memory. `MISSED` if no group of its kind is there; `REFUSED` if the heads would pass the kind's `group max` |

Bites land before anyone moves, on either side of a chunk border, in key order. Several
eaters of one victim each get the share they took. Transfers are settled after bites, also
in key order; `harvest` and `drop` with the other actions, in key order (a standing actor and
the cover under it share a cell). A bite, `take` or `give` finds a decaying need as it stands that tick, not as
it was at its owner's last think.

## 9. Effects

Effects combine with the action and don't end the think.

| effect | |
|---|---|
| `look = v` | a public byte, 0 to 255, that other actors read (`kind:look` predicates, `look_of(t)`) and `wmc why` shows; it is not drawn |
| `signal = v` | a public 16-bit value others read with `signal_of(t)`, like a bee's dance |
| `add F v` | adds `v` (negative takes away) to field F on the actor's cell, clamped to the field's `0..max`, in key order with the other actors on the cell. Several adds in one think add up; one think adds to at most 4 fields (a fifth traps). Not on a `conserved` or `fixed` field (§12, Fields) |

## 10. Senses

| sense | value |
|---|---|
| a need or mem name | its value |
| `age` | ticks since birth or `become` |
| `x`, `y` | world position |
| `kind`, `look`, `signal`, `state` | own public bytes and state index |
| `light` | daylight, 0 (night) to 255 |
| `hour`, `day` | clock, 0 to 23, and days since the world began (at most 2147483647) |
| `hurt`, `hurt_dir` | damage taken since the last think; direction (1 to 8, clockwise from north) of the lowest-key attacker. Also the target `attacker` |
| `result`, `blocked`, `missed`, `refused` | the last action's outcome |
| `taken` | something was taken from it since its last think |
| `trapped` | its last think ran out of fuel or faulted |
| `heads` | how many it stands for: 1, or a group's heads (§12, Groups) |
| `count pred within r` | matching cells in the square of radius r, **own cell included**. The radius is one term (a number, a name, a call or a parenthesised expression): `count fox within 3 - 1` is the count minus one; write `within (R - 1)` for a sum |
| `free(t)` | the cell is walkable and nobody stands there; for a kind in a declared layer, nobody is there in its layer (and it is walkable, unless the layer flies) |
| `is(t, pred)` | the cell matches pred (`is(here, grass)`); `is(here, water)`, `is(here, rock)` ask about the ground and the feature |
| `look_of(t)`, `signal_of(t)` | the public bytes of whoever stands at t, else of its cover, else of the first declared layer holding someone; 0 for nobody |
| `heads_of(t)` | the heads of the group at t, in whatever layer it is (a herd in a canopy over a tree); else how many whoever is at t (as `look_of`) stands for: 1; 0 for nobody |
| `fits(t)` | would the reader fit with its top-left cell at t: every cell of its `size` walkable (unless its layer flies) and empty in its layer, or its own (§12, Big actors); `free(t)` for a one-cell kind, and true `here`. It costs fuel by its cells, as a search does (W x H / 8) |
| `dist(t)` | Chebyshev distance to t |
| `v.dx`, `v.dy` | a bound target's offset |

Fields (§12) are read like this too: `F(t)` is field F at target t (`sugar(here)`), 0 where
nothing is loaded or beyond reach. A condition finds the richest cell:

| condition | |
|---|---|
| `sum F along t`, `min F along t`, `max F along t` | field F over the cells of the line from the actor (not its own cell) to t, at most `sight` of them: smoke in the way, the highest point between. 0 for no cell |
| `sees(t) over F [eye N]` | 1 if no cell between the actor and t rises above the straight line from the actor's F plus N to t's F plus N (N defaults to 0): line of sight over a height. 0 past `sight`, or with a cell between not loaded |
| `best F [on pred] within r as v` | binds `v` to the cell in rings 1 to r (not its own) with the most of field F, among the cells matching `pred` if `on` names one (`best sugar on free within 6 as s`); false if every such cell holds 0. Scanned as `sniff` scans, so ties go to a random start |

Searches (`count`, `nearest`, `best`, `for each`) are capped by `sight` and see into the
neighbouring chunks. An unloaded chunk reads as rock with nobody on it, and so does any
cell out of reach (§11). `nearest` scans
rings 1 to r (not its own cell), and each ring starts at a random point so a flock doesn't
all pick the same target. A big actor's `nearest`, `best` and `for each` skip all of its own
cells; `count` still counts them and `is(t, ...)` still sees them.

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
| a kind (`fox`) or a tag (`meat`) | whoever stands there, the cover there, or anyone in a declared layer there; a kind matches its whole family (§6) |
| `only fox` | that kind exactly, not the kinds that extend it |
| `kind:look` (`flower:1`), `only kind:look` | that kind (family, or exactly) showing that look |
| `water`, `soil`, `rock` | the ground, the feature |
| `free` | walkable, nobody standing; read by a kind in a declared layer: nobody there in its layer, walkable unless the layer flies |
| `bare` | walkable, no ground cover |
| `(F op v and ...)` | a **field condition**: each field of the cell compares true against its value (`(sugar < 20)`, `(trail > 100 and free)`); one plain predicate may join them; at most three fields. The values are worked out once, before the search, so `(sugar < ROOM - 1)` costs one sum, not one a cell. An unloaded cell never matches. It takes two local slots a field and one more while its search runs (`for each`: while the loop runs) |

```rules
field sugar max 20 { init 20 }
field trail max 255
kind ant {
  glyph "a"
  need sugar max 40 decay 0
  when sugar > 30 and nearest (sugar < 20 and free) within 3 as c => move toward c   # room to drop it
  when count (trail > 100) within 2 >= 3 => move random free                         # a crowded path
  when true => idle
}
```

## 12. Expressions, subs and constants

Every value is a 32-bit integer; there are no floats, and `true` is 1, `false` 0. Arithmetic wraps, `x / 0` and
`x % 0` are 0. Operators: `+ - * / %` and `< <= == != >= >` (1 if it holds, else 0). Comparisons don't
chain: `0 < v < 9` is an error; write `0 < v and v < 9`, or `(0 < v) < 9` if that is meant.
`and`, `or` and `not` are not operators on values: they join conditions, after `when`, `if`
and `while` only (§4). So `let both = x > 0 and y > 0` is an error; write
`let both = (x > 0) * (y > 0)`, or test it in an `if`. Functions:
`min`, `max`, `abs`, `sign`, `clamp(x, lo, hi)`, `rand(n)` (0 to n-1), `chance(p)`
(p percent), `pack(a, b)` / `hi(v)` / `lo(v)` (two signed bytes in one value, for signals).
Randomness is drawn per actor per tick from the world seed, so a replay draws the same.

**Memory arrays.** `mem route[8]` declares eight memory slots in a row, all named `route`:
`route[i]` reads one and `route[i] = v` (or `+=`, `-=`) writes one, for any expression `i`
from 0 to 7. Past the ends a read is 0 and a write changes nothing, as `x / 0` is 0. The
index is worked out once, so `route[rand(8)] += 1` adds to the element it read. Slots of an
array count toward the 32 a kind has; a save and a reload keep each element by name
(`route[3]`), so a longer array keeps what the shorter one held. An array has one length: a kind and
the traits it extends that declare it must agree.

**Searches.** A `search` explores choices before acting: from a starting **state** (up to 8
numbers), its `next` block offers the states one step away with `try`, each with a `cost`
(default 1) and a `via` (a number naming the step: what to do); the search keeps the
cheapest way to each state it reaches, expands the open state with the least cost so far
plus its `guess`, and stops at the first that meets its `goal`. What a state *is* is the
pack's: memory values to plan over, a position to find a way from, anything a rule can
compute. A search is declared like a sub (in a file, or a member of a kind or trait, with
parameters if it likes) and called in a condition:

```rules
const CHOP = 1
const SAW = 2
const BUILD = 3
kind builder {
  glyph "b"
  fuel 8192
  mem wood, planks
  search plan_house {
    state w, p, h = wood, planks, 0            # starts from what it holds
    next {
      try w + 1, p, h cost 3 via CHOP
      if w >= 2 { try w - 2, p + 1, h cost 1 via SAW }
      if p >= 4 { try w, p - 4, h + 1 cost 2 via BUILD }
    }
    goal h >= 1
    guess 4 - min(p, 4)                        # optional: a hint of what is left
    limit 500                                  # states it may expand
  }
  when plan_house() as plan and plan.via == CHOP => { wood += 1  idle }
  when plan_house() as plan and plan.via == SAW => { wood -= 2  planks += 1  idle }
  when true => idle
}
```

`NAME(args) as plan` holds when the goal was reached: `plan.via` is the first step's `via`
(what to do now), `plan.cost` the whole cost, `plan.steps` how many; `as plan into route`
also writes every step's `via`, first first, into the memory array `route` (0 past the
plan's end). A search used as a value (`if plan_house() { ... }`) is 1 when it found its
goal. Inside the search, the state's names are its values: `next`, `goal` and `guess` read
them and anything else a rule may (needs, memory, fields, senses, `free(at(px, py))`), so a
way across the cells is the same thing with the position as the state:

```text
search to_water {
  state px, py = x, y
  next {
    if free(at(px + 1, py)) { try px + 1, py via 3 }      # east
    ...                                                   # and the other three
  }
  goal is(at(px, py), water)
  guess abs(px - x) + abs(py - y)
}
when to_water() as p => move dir(p.via)
```

- **Exact or not.** With no `guess` (0) the plan is the cheapest; with a guess that never
  overestimates what is left, still. A guess that does (`3 * dist`) finds a plan sooner, not
  always the best: an impatient planner. Dice in a `cost` (`1 + rand(3)`) give each actor
  its own way, the same on a replay.
- **Bounded.** `limit` (any expression; default 256, at most 4096) caps the states it
  expands; past it, or with nothing left to try, it found nothing (`as` doesn't hold). It
  holds at most 32768 states. Each expanded state runs its code, which spends fuel as any
  think does, plus a little per state: give a planner the `fuel` (up to 32768).
- **Local.** A search reads only what a rule could, within `sight`; it doesn't act, `next`,
  `return` or call what acts, and a search's `next`, `goal` and `guess` start no search, not even through a call (a
  compile error).
- **The same everywhere.** Ties go to the state offered first, so a plan is the same on any
  number of threads.

`packs/maze` finds its way through a maze this way and keeps the route in a memory array;
`packs/workshop` plans its chores over what it carries.

```rules
kind ant {
  glyph "a"
  mem n, trail_x[4], trail_y[4]          # the last four places, round a ring
  when true => { trail_x[n % 4] = x  trail_y[n % 4] = y  n += 1  move random free }
}
```

```rules
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

**Constants from outside.** Any `wmc` command takes `--set NAME=V` (repeatable): the
constant `NAME` takes the value `V` (a number or a time) for that session, and the constants
declared after it fold with the new value. A name the rules don't declare is an error that
lists the ones they do. That makes constants the knobs of a pack: put a number you may want
to tune in a `const`, and `wmc lab --set NAME=a,b,c` tries several (§17).

### Grounds and features

A cell has one **ground** (under foot) and at most one **feature** (on it). The engine
generates three: the grounds `soil` (walkable) and `water` (to drink, not walkable) and the
feature `rock` (in the way). A pack declares more beside its kinds, and may redeclare these
three: a redeclaration gives the material its properties as written.

```rules
ground mud { walkable  glyph ","  color "#6b4f2a" }
ground water { walkable  drink }           # shallow: walked through, and drunk
feature wall { blocks  opaque  glyph "="  color "#9a9a9a" }
feature hedge { blocks }                   # in the way, but seen over

kind wader {
  glyph "w"
  when nearest mud within 4 as m => move toward m
  when count wall within 1 > 0 => idle
  when true => move random free
}
```

| part | default | meaning |
|---|---|---|
| `walkable` (a ground) | not | an actor can stand on it |
| `drink` (a ground) | not | `drink t` at an adjacent cell of it refills the need named `water` |
| `blocks` (a feature) | not | nobody enters a cell holding it |
| `opaque` (a feature) | not | `sees(t)` is false past a cell holding it (the target's own cell may hold one: a wall is seen) |
| `glyph "c"` | `.` a ground, `#` a feature | drawn on a bare cell of it |
| `color "#rrggbb"` | the palette's (the three) | a ground's background, a feature's glyph |

A material's name is a predicate (`nearest mud within 4`, `is(here, water)`, `count wall
within 1`), global like a kind's: never a kind's, a trait's, a sub's, a tag's, a constant's
or another material's. A world holds at most 16 grounds and 15 features, the engine's
among them. The terrain generator lays only the three; a drawn map's legend names them
(§14). A save keeps materials by name: reopened under rules that number them otherwise,
every cell keeps its material; one the rules no longer declare becomes the first ground,
or no feature.

### Layers

A cell holds one actor in each **layer**. The engine has two: who stands there and the
ground cover under them (`cover`). A pack declares more beside its kinds, and a kind names
the one it lives in:

```rules
layer air { flies }

kind gull {
  glyph "v"
  layer air
  sight 6
  need food max 2d vital
  when food < 40h and is(here, crab) => eat here     # stoops on the crab under it
  when food < 40h and nearest crab within 6 as c => move toward c
  when true => move random free                      # free: nobody in the air there
}

kind crab {
  glyph "c"
  need health max 1 decay 0 vital
  when true => move random free                      # free: nobody standing there
}
```

Each layer has its own occupants and its own claims: a gull and a crab share a cell
without contesting it, and two gulls contest it as two crabs do (the lowest key wins).
`flies` lets the layer's actors over any ground, water and rock too; without it, they
need walkable cells, as walkers do. `free` means free in the reader's own layer; a kind
or tag predicate (`nearest crab`, `is(here, gull)`) finds an actor in any layer; from a
declared layer, `eat` and `hit` reach the one standing under the biter (`here`) as well as
its neighbours. A kind inherits its parent's layer; ground cover can't be in a declared
layer. A layer's name is global like a kind's, never `standing` or `cover`; at most 6 a
rule set, so a chunk holds at most 32768 actors in all its layers (a birth or a move into
a chunk that full is `BLOCKED`). The renderer draws the topmost layer holding someone. A save keeps layers by
name; a kind that moved to another layer is refused (restart). `packs/sky` plays it.

**A layer that rests on a kind.** `layer canopy { on tree }` holds its actors only where a
`tree` (or a kind that extends it) stands, or with `on TAG`, a kind with that tag. A monkey
in the canopy shares the tree's cell without contesting it; `free` to the monkey means a
tree with nobody in its canopy, so `move random free` and `nearest free` keep to the trees,
and a move or spawn to a cell with no tree is `BLOCKED`. When a tree dies, is eaten or
`become`s something else, whoever was in its canopy falls at the end of the tick and dies
(`observe` names the cause `fell`). `on` names a kind that stands: not ground cover, nor a
kind in a declared layer. In a scenario, a kind of such a layer starts `on` its kind
(`start monkey 1 / 4 on tree`, §14), or `at` a cell where one starts. `packs/forest`
plays it:

```rules
layer canopy { on tree }

kind tree { glyph "T" }

kind monkey {
  glyph "m"
  layer canopy
  when true => move random free      # to a tree next to it, if its canopy is empty
}
```

### Groups

A kind declared `group max N [init M]` is a **group**: each actor of it stands for up to N
animals (its `heads`, 2 to 65535), and starts with M (1 if not given). A herd of a thousand
is one actor: one think, one row, one cell. That is how a world holds millions of animals
with only thousands of thinkers. A group is its own actor in every other way: it moves,
senses, eats and has needs as one; its needs are the group's.

```rules
field pasture max 9 { init 3  every 6h { pasture = min(pasture + 1, 9) } }

kind herd {
  glyph "H"
  group max 200 init 20
  food 1h
  need food max 2d vital
  need health max 1 decay 0 vital

  when food < 1d and pasture(here) > 0 => { add pasture -1  food = min(food + 6h / heads, 2d)  idle }
  when heads > 60 and nearest free within 1 as n => split heads / 2 at n
  when heads < 10 and nearest herd within 1 as h and heads_of(h) + heads <= 200 => join h
  when food > 1d and chance(1) => { grow max(heads / 20, 1)  idle }
  when true => move random free
}
```

- `heads` is its own count; `heads_of(t)` another's (1 for one who is not a group), so a
  wolf can pick the smallest herd in sight.
- `grow n` (an effect, like `look = v`: several add up) adds `n` heads, at most to the max;
  a negative `n` takes them away, and a group grown to none is gone. Heads gained are births
  (`born`), heads lost deaths (`died`, its own `die`). It comes before the rule's action:
  `grow 5  join h` joins with the grown count, and a group grown to none does nothing else.
- `split n at t` and `join t` (§8) move heads between groups of a kind.
- **Losses take heads.** Each head has the group's `health`: a bite that takes the last of
  it kills one head, and the rest of the bite goes on into the next, at full health, so one
  bite may take several. A vital need that runs out kills one head and refills for the
  rest. A rule's `die` ends the whole group, and so does a fall (§12, Layers). Every death
  is counted per head, by its cause.
- A group `become`s only a kind that holds its heads (a group of 5 can't become a kind that
  is not a group).
- `grow`, `split` and `join` are a group's: only in a group kind's rules or member subs
  (not a file sub). Ground cover is never a group. A kind inherits its parent's `group`.
- Scenarios: a group starts with its `init` heads (placed, or born of a `spawn`, each head
  counted `born`), or `start K at (X, Y) with (heads = N)`;
  `count K` counts heads, and `rows K` actors (§14). `observe` counts heads.

`packs/herds` plays it: herds that calve when fed, split at 60 and join under 10, under
wolves, level at about 50 000 animals in a few hundred rows.

### Big actors

A kind declared `size W H` covers W cells across and H down (1 to 16 each) from its own
cell, the top-left one, which is where it is (`x`, `y`, a target bound to it). It is one
actor: one row, one think, one set of needs.

```rules
kind mammoth {
  glyph "M"
  size 3 3
  need food max 3d vital
  when chance(30) and fits(west) => move west
  when true => move random free
}
```

- **It moves whole.** A `move` steps only where every one of its cells fits: walkable
  (unless its layer flies) and empty in its layer, or its own. Like any move it slides 45
  degrees when the straight step doesn't fit, and is `BLOCKED` when none does. `fits(t)`
  asks it of any target: would it fit with its top-left cell there? (For a one-cell kind,
  `fits(t)` is `free(t)`, and also true `here`.) `free` and `random free` still mean one
  free cell.
- **Every cell is it.** A sense sees its kind on each of its cells (`nearest mammoth`
  finds the nearest one, `count mammoth within r` counts cells); `eat`, `hit`, `take` and
  `give` on any of its cells reach it, and `look_of`, `signal_of`, `heads_of` read it
  (a `kind:look` predicate matches only its top-left cell). Nobody else enters its cells, and
  nothing is built on them. Its own searches skip its cells (§10), and its own `eat`, `hit`,
  `take` or `give` on one of them is `REFUSED`. Its senses, `reach` and `dist` measure from
  its top-left cell.
- Its moves and births are settled after everyone else's, one at a time in key order,
  across chunk borders: a one-cell actor that claimed a cell this tick wins it.
- A `spawn` of a big kind needs room for all of it. A big actor never `become`s another
  kind, nor does anyone become a big kind (`REFUSED`): its cells carry its kind. It can be a group (`grow`, heads), but a big group neither splits nor joins. It is
  never ground cover, nor in a layer that rests on a kind.
- Scenarios: a share places it only where it fits inside one chunk; `start K at (X, Y)`
  must fit in its chunk too (chunks generate apart), on cells it can stand on, overlapping
  no other start. Saves keep its size: rules that change it are refused (restart).
- While a chunk holding some of its cells is away (streaming), those cells stay its: an
  actor that can't fit there can't move there. It keeps its room, and nothing is lost.

`packs/giants` plays it: mammoths that never fit between the trees deer slip through.

### Fields

A **field** is a number in every cell of the world, declared by a pack beside its kinds:
sugar on the ground, a trail, a claim, heat. Every cell holds every field the rules declare.

```rules
field sugar max 4 conserved {
  init noise scale 12            # patches, 0 to 4, from the world's seed
  color "#e0c040"                # a gold wash, stronger where there is more
}
field trail max 255 { color "#80d0ff" }

kind gatherer {
  glyph "g"
  sight 6
  need sugar max 40 decay 0      # what it carries: the field's own name
  when sugar(here) > 0 and sugar < 40 => harvest sugar 4
  when best sugar on free within 6 as s => { add trail 20  move toward s }
  when true => move random free
}
```

| part | default | meaning |
|---|---|---|
| `max M` | | values run 0 to M, 1 to 65535 |
| `conserved` / `fixed` | neither | who may write it: `conserved` takes no `add`, so what it holds only moves (`harvest`, `drop`); `fixed` takes no writes from actors at all. The compiler refuses the rest |
| `init V`, `init noise scale S` | `init 0` | where its cells start when a chunk is made: V everywhere, or value noise S cells across mapped onto 0 to M (most cells near the middle), from the seed |
| `color "#rrggbb"` | not drawn | tints each cell toward the colour, by its value against the max |
| `every N { ... }` | no law | its law: run in every cell every N ticks (below) |
| `outside V` | the chunk's edge | what its law reads beyond the loaded world |
| `relief` | flat | drawn with its slopes shaded, lit from the north-west: for a height |

Actors read a field with `F(t)` and `best` (§10), and change it on their own cell only:
`add` (§9), `harvest` and `drop` (§8). A field's name is global, like a kind's: never a
kind's, a trait's, a sub's, a tag's or a constant's, but a need may share it, and that is
the need `harvest` and `drop` move into and out of. A sub can't `harvest` or `drop`: needs
belong to kinds.

```rules error
field ore max 9 conserved
kind miner {
  glyph "m"
  need ore max 9 decay 0
  when true => add ore 1   # error: the field is conserved
}
```

A save keeps every field by name. Reopened or reloaded (`r`) under rules that add a field,
its cells start at its `init`; a field the rules drop is gone; a lower max clamps.

**Laws.** A field's law is how it changes by itself: a block run in every cell of the
world, every N ticks (in each chunk at its own moment, staggered like actors' thinks).

```rules
field room max 20 fixed { init noise scale 10 }
field sugar max 20 conserved {
  init 20
  every 1h { sugar = min(sugar + 1, room) }               # grows back to what the cell holds
}
field heat max 255 { every 16 { heat = avg heat within 1 } }   # spreads out
field alive max 1 {
  every 1 {                                               # Conway's Life, one generation a tick
    let n = sum alive within 1                            # the cell itself counts
    if n == 3 { alive = 1 } else if n != 4 { alive = 0 }
  }
}
```

A law sees one cell at a time: its own value, the other fields of the cell, constants,
`hour`, `day` and `light`, `rand` and `chance` (the cell's own dice), and its neighbours
through **aggregates** over the 3 x 3 square, the cell included: `sum F within 1`, `avg`,
`min`, `max`, and `count (cond) within 1`, the cells where `cond` holds. It reads the
actors on its cell as a search sees them: `is(here, PRED)` is 1 where the cell matches a
kind (its family, or `only` it, or `kind:look`), a tag, a material, `free` or `bare`, and
`count (is(here, hen)) within 1` counts the cells round it that do (no other target: a law
stays on its square; beyond the loaded world, rock with nobody). It has `let`,
assignments to its own field and `if`/`else`; nothing that acts, searches, loops or calls,
and it writes no other field (the compiler says so). Inside an aggregate a field is read as
it stood before the law ran, and every law of a tick runs from the values the fields held
before any of them: the chunks may run in any order and come out the same. The result is
clamped to the field's `0..max`. Where the world isn't loaded, a law reads the chunk's own
edge, so an even field stays even, or the field's `outside` value if it has one. A law is a
column program: it runs over a whole chunk at once, as fast as hand-written code once it
reads its neighbours (`docs/WORLD.md` §8), so a law is much cheaper than an actor in every
cell. A law without dice or the clock that leaves 0 at 0 (a fade, a regrowth to a cap,
Life) skips every chunk where the fields it reads are all 0 (one that reads actors never
does): a trail that covers a few
chunks costs only there. `packs/gather` grows its sugar back with one.

```rules
field turf max 9 {
  init 9
  every 1h {
    if is(here, hen) { turf = turf - 1 }                  # trodden where a hen stands
    else if count (is(here, hen)) within 1 == 0 { turf = min(turf + 1, 9) }
  }
}
kind hen { glyph "h"  when true => move random free }
```

**Flow.** `flow(w, level, rate)` is what flows into a cell from its four neighbours (north,
east, south, west), net: each pair of neighbours moves `(level' - level) / rate` (rounded
towards zero) from the higher level to the lower, but never more than a quarter of what the
giver holds of `w`. Both cells of a pair work out the same amount, so what one gains the
other loses: `water = water + flow(water, height + water, 4)` keeps every drop, and water
runs downhill, fills the hollows and lies level there. A law with a flow runs in every chunk
at the same tick (not staggered), so a pair across a chunk's border moves together. A
mirrored edge moves nothing; an `outside V` edge is a sink or a source (`outside 0` drains
the world). The rate is a number above 0; the higher, the slower and smoother (with 4 or
more a cell never overshoots its neighbours). Give the field room above what it will pool
to, or the clamp to its max loses the rest. Inside a flow, as in an aggregate, a field is
read as it stood before the law ran.

```rules
field height max 4000 fixed { init noise scale 40  relief }
field water max 60000 conserved {
  init 200
  color "#3070e0"
  every 2 { water = water + flow(water, height * 4 + water, 4) }   # lakes fill the hollows
}
```

## 13. Cost

Each op costs 1 fuel, and a search costs `(2r + 1)² / 8` more. A think that runs out of fuel
becomes `idle`, sets `trapped`, and counts under `TRAPS` on the status bar and in
`wmc run`. So does any other trap (a second action, a fault). Writes to needs and mem made
before the trap stay; the action, `next`, `look`, `signal` and `add` are dropped. Write
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
- **Keep radii small.** A radius-8 search reads 289 cells, radius 16 reads 1089, radius 64
  reads 16641 (and costs about 2080 ops: give a far-sighted kind the `fuel` for it).

## 14. Scenarios

Rules say what kinds are and how they behave. Where they start is the world's business: a
new world is made from a **scenario**, a small text file in `scenarios/`. A save keeps its
scenario, so every part of the map generates the same way whenever it is first visited.

```scenario
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
| `player NAME PATH` | none | a player's pack (§15, Arenas and players), relative to the scenario file; its kinds are `NAME.KIND` |
| `seed N` | 42 | the world's seed: terrain, placement and every actor's dice |
| `size W H` | 80 24 | the region generated at creation, rounded up to whole 64-cell chunks, at most 4096 of them (4096 x 4096 cells); the rest generates as the camera reaches it |
| `terrain NAME V ...` | below | `water_scale` 12 (lake size in cells, above 0; below about 1 the lakes are finer than a cell and the terrain looks like noise), `water_level` 0.30 (roughly the share of water), `rock_on_soil` 0.04, `rock_on_water` 0.01 |
| `start K N / D` | | this share of walkable cells, everywhere in the world, starts as kind K |
| `start K N / D in (X0, Y0) to (X1, Y1)` | | this share of the walkable cells in that region, both corners included: X0 to X1 across, Y0 to Y1 down |
| `start K N / D on K2 [in ...]` | | a kind above the standing (ground cover or a declared layer): this share of the cells where a K2 (or a kind that extends it) starts, above it. The only way a share starts a kind whose layer rests on a kind (§12, Layers) |
| `start K at (X, Y) [with (NAME = V, ...)]` | | one K on that cell, which must be walkable and inside the world (x and y from -1000000000 to 999999999); `with` sets its needs or memory by name (`food = 2h`, `heading = 3`, an array's element `route[2] = 7`), and a group's `heads` |
| `map { ... }` | | cells drawn from (0, 0), one ASCII character each, one row per line, every row as long as the first; the rows stand alone on their lines, with no comments |
| `legend { ... }` | | what each map character stands for, one entry per line: `soil`, `water`, `rock`, or a kind, which stands on soil and may take a `with`; a character the map never uses is an error (delete its line) |
| `outside noise` | `noise` | beyond the map: the seed's noise, or all `soil`, `rock` or `water` |

`seed`, `size`, `map`, `legend` and `outside` come once, and each terrain field once (several
`terrain` lines add up); `rules` and `start` lines add up. A kind has one share everywhere and
one in each region at most.

**The edge of the world.** The world is a billion cells each way from (0, 0): x and y run
from -1000000000 to 999999999. A position past that in a `start` or `expect` line is an
error on its line, and so is one in a save. Nothing past the edge ever loads, so to an actor
standing at it the cells beyond read like an unloaded chunk: rock, nobody there, and a move
or spawn there is `BLOCKED`. The camera stops at the edge too.

Each walkable cell draws one number in [0, 1), and the shares cut that range into intervals
in the order written: a cell starts at most one kind, the shares add up to at most 1, and
reordering the lines moves who starts where. A share with a region takes part only on the
cells inside it, in its place among the lines: on any cell, the shares that hold it are cut in
the order written, and they must add up to at most 1 there. So two regions that do not meet
can each be filled whole, and several worlds share one board side by side:

```scenario
# two boards on one world: a Life soup on the left, every cell of the right a dead one
rules ../packs/life
seed 1
size 128 64
terrain water_level 0 rock_on_soil 0 rock_on_water 0
start alive 3 / 8 in (0, 0) to (63, 63)
start dead  5 / 8 in (0, 0) to (63, 63)
start dead  1 / 1 in (64, 0) to (127, 63)
```

Where regions overlap, an error names the line and a cell where the shares pass 1. A share
is exact to one part in 2^24 (the smallest is 1 / 16777216), so
halves, quarters and eighths add up exactly (`start alive 3 / 8` and `start dead 5 / 8` fill
every cell), while `1 / 3` and `2 / 3` leave about one cell in 16.7 million empty. An explicit start takes its cell, whatever the
shares would have put there. A kind starts in its layer: a cell holds one explicit start a layer (a gull over a crab),
and the shares one actor a cell, in its kind's layer, on walkable cells. The shares with `on`
are a second draw: where a share started someone standing, the cell draws again, and those
shares cut that range, in their order, adding up to at most 1 among themselves; a draw that
lands on a share whose `on` kind is not the one standing there starts nobody. So `start tree 1 /
3` and `start monkey 1 / 4 on tree` start a monkey in about a quarter of the trees. Everyone
starts newborn, needs full, but for what `with` sets.

**A drawn map.** A scenario can draw its ground instead of rolling it. The map's size is the
scenario's `size` unless you give a larger one (leave `size` out to use the map's; a smaller
one is an error), shares still apply to its walkable cells, and every kind character becomes
a `start K at (x, y)`. This is the fox and the
cornered hen of `scenarios/tests/fox_pen.scenario`, with the pen drawn:

```scenario
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

```scenario
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
| `expect count K OP N` | actors of K alive now, a group's every head (§12, Groups); `count K in STATE` only those in that state (`count villager in WORK`) |
| `expect rows K OP N` | actors of K alive now, a group as one; `in STATE` as for `count` |
| `expect born\|became\|eaten\|died K OP N` | the life counters so far: born of a spawn, became K, eaten, died. `eaten K by K2` counts only the deaths a bite of K2's family caused, `died K of NEED` only those where that vital need ran out |
| `expect thinks\|traps K OP N` | the thinks run so far, and those that trapped (out of fuel, a second action, a fault; `expect traps K == 0` guards against a rules bug), as `wmc run` prints them |
| `expect min\|max\|sum\|avg NAME of K OP V` | a need (as it stands now, decayed since the last think) or memory (an array's element: `max route[0] of K`) over every actor of K, or with `in STATE` those in that state (no actor: the check fails); `avg` is their mean, compared exactly (`avg wealth of villager >= 40`) |
| `expect min\|max\|sum\|avg FIELD OP V` | a field over every cell of the world: `always sum water == 3276800` holds what a conserved field moves to its total |
| `expect at (X, Y) K` | someone there, in any layer, is a K; `nobody` for a cell with nobody in any layer; the cell must be inside the world |
| `expect checksum HEX`, `expect state HEX` | the world, with and without the rules hash |

`OP` is one of `== != < <= > >=`, and `V` a whole number or a time. A kind means its
family, as in the rules; `only K` means K alone (`expect count only chick == 1`). A state is
looked up by name in each kind of the family; one without it counts none.

**Expectations over time.** The lines above check one moment. A goal is usually about a
stretch of the run: the hens never die out, foxes eat so many a day. Four words look at a
window of it instead:

| line | holds when |
|---|---|
| `expect always COND [from T] [to T]` | COND held at every check in the window. COND is any line above but `checksum` and `state` |
| `expect never COND ...` | it held at no check |
| `expect eventually COND ...` | it held at one check at least |
| `expect mean METRIC OP V ...` | the average, over the checks, of `count K`, `min\|max\|sum\|avg NAME of K` or of a field, each with its `in STATE` (a check with none alive is left out); `V` may be a decimal (`2.5`) |
| `expect mean COUNTER K per day OP V ...` | how much a counter (`born`, `became`, `eaten`, `died`, `thinks`, `traps`, with its `by` or `of`) grew per day across the window: a rate |

The window runs `from T` (else the world's start) `to T` (else the line itself), both times
since the world was made, and it must lie within the `run` lines before its line. Its checks
fall at its start, its end and every whole minute between (every 15 ticks), so a condition
that lasts less than a minute can slip between two; a rate reads its counter at the two
ends. The report says what it saw: `true at all 121 checks; it was 2`, `false at 3h 15min
(180); from 150 to 260`, `first true at 1min (0)`, `mean 18.4 over 721 checks (3 more with none alive)`, `24 per
day: 2 in 2h`.

**Several seeds.** One seed can be lucky. `seeds N` runs the test on seeds `seed`,
`seed + 1`, ... (`N` of them, at most 1000), and `pass M` makes it pass when every
expectation held on at least `M` of them (all, when not given); `wmc scenario --seeds N
--pass M` overrides both (a `pass` above the seeds it runs on is an error, so `pass 7`
without `seeds` needs `--seeds`). The report gives a line per seed (its verdict, what failed on it,
its checksum), then how many seeds each expectation held on: `7/8`.

```scenario
# watched_pen.scenario
seed 3
seeds 4
pass 3
outside rock
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
  F fox with (food = 2h)    # starving: it hunts at once
}

run 2h
expect always count fox == 1                        # the fox lives
expect eventually count chicken == 0                # and eats the hen
expect never died chicken of water > 0              # she was not thirsty
expect mean eaten chicken by fox per day >= 12      # one in two hours
expect mean count chicken < 0.1                     # she lived a few minutes of 120
```

A scenario names kinds, so it has to fit the rules: a start naming a kind the rules don't
define, or a trait, refuses the new world (a save's are skipped, §15), and so does one on
water or rock, two on one cell, or a `with` its kind lacks or past a need's range. The error gives the start's line (a drawn
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
trait, file sub, const, tag and field name is global across all of them: a kind in one pack
can extend a kind or trait of another, call its subs and use its constants, and a name
declared twice, in any two files, is an error naming both. A sub declared inside a kind or
trait is that item's own: two items may each have a `wander`, but it can't share a name with
a file sub, a kind or a trait. A pack given twice loads once. With several packs an error
names a file `pack/file.rules`, with more parent directories where two packs share a name.

```sh
wmc play saves/zoo --rules rules --rules mods/wolves     # the built-in kinds, then a mod's
WMC_RULES=rules:mods/wolves wmc run saves/zoo 1000       # the same, for any command
wmc lint rules mods/wolves                               # check them together
```

A scenario can name its world's packs, relative to the scenario file, so it runs like any
other with nothing but `--scenario`:

```scenario
rules ../packs/life                 # in scenarios/life.scenario
```

A save remembers its packs, by absolute path. So a new world runs `--rules`, else
`WMC_RULES`, else its scenario's `rules`, else the built-in kinds; a saved one runs
`--rules`, else `WMC_RULES`, else the packs it was saved with. If one of those is gone it
says so and uses the built-in rules, and the next save (`p` or quitting `wmc play`) records
the built-in rules in place of the missing packs: from then on the save opens with them,
even once the packs are back (pass `--rules` to return to them). `wmc lint --scenario <file>`, with no packs (no
`--rules`, positional pack or `WMC_RULES`), lints the ones the scenario names, the same order
as for a new world.

**Arenas and players.** In a game where people write creatures and meet in one world, the
world's packs are the **arena**, and each person's is a **player** pack, given as
`NAME=PATH` (`--rules red=packs/red`) or by a scenario's `player NAME PATH` line. A player's
top-level names (kinds, traits, subs, consts, fields) are its own: inside its files it
writes `ant`, and the world knows it as `red.ant`, so two players may both have an `ant` and
neither can name the other's. A player uses the arena's fields, traits, subs and constants,
within their write policy (`conserved`, `fixed`), and tags stay global, so colonies can find
each other (`nearest insect`). It declares kinds, traits, subs, consts and `private` fields
only: no ground, feature, layer or shared field, so its code can't change the world's
physics or make a resource. It can't take a name the arena has, or a word the language
reads (`food`, `health`, `glyph`, ...). A `private` field is a field only the player's code
can name, with a law if it likes: a trail the other colony can't smell.

```text
# packs/colony/red/red.rules
field trail max 255 private { every 16 { trail = trail - (trail + 31) / 32 } }
kind ant extends forager {                  # the arena's trait: its needs, its tag
  when crumbs >= 6 => { add trail 60  move toward at(nest_x, nest_y) }
  when best trail on free within 6 as t => move toward t
  ...
}

# scenarios/tests/colony.scenario
rules ../../packs/colony
player red ../../packs/colony/red
player blue ../../packs/colony/blue
start red.ant 1 / 200 in (0, 0) to (63, 127)
start blue.ant 1 / 200 in (64, 0) to (127, 127)
```

**A budget.** An arena may set what each player gets, so one player's code can't slow the
world or crowd it for the others: one `budget` block, beside its fields, each part a
constant expression and each optional.

```rules
budget { fuel 256  rows 60  private 1 }
```

| part | meaning |
|---|---|
| `fuel N` | a player's kinds think in at most N ops (1 to 32768): a kind's fuel, its own or the default 512, is cut to N. The arena's own kinds keep theirs |
| `rows N` | no player has more than N rows alive. Each tick the room a player has left is shared among its spawns, chunk by chunk in coordinate order, then in key order; the rest are `REFUSED`. A death makes room again. A scenario's `start` lines aren't held to it. It counts the rows of the loaded world: scenarios, tests and `lab` load all of it, so there it is exact; in an endless world played with streaming, rows in chunks off screen are frozen (§11) and uncounted until they load again, which can bring a player over until deaths make room |
| `private N` | a player declares at most N `private` fields |

Only the arena declares a budget; a player's pack that does is refused. `packs/colony` keeps
each colony to 60 ants.

**Namespaces.** Packs share one set of names, so two arena packs by different authors that
both declare a `path` don't load together. A file that starts `namespace NAME` puts its
top-level names (kinds, traits, subs, constants, fields, materials, layers) in NAME: inside
the namespace's files they stay plain, and everywhere else, other packs and scenarios, they
are `NAME.ITEM`. Tags stay global (they are how packs find each other's actors). A namespace's
item can't take a word the language reads (`limit`, `steps`, `size`, `group`, `food`, ...),
as a player's can't, except a `ground` or `feature` that redeclares the engine's `soil`,
`water` or `rock`: that one stays global, and a namespace that redeclares `ground water`
changes the world's water. A file has one `namespace` line. A namespace can't be a player's
name or any pack's item, and a player's pack declares none (its names are its own already).

```text
# packs/weather/weather.rules
namespace weather
layer sky { flies }
field path max 255 { every 1 { if is(here, cloud) { path = min(path + 8, 255) } else { path = path - (path + 31) / 32 } } }
kind cloud { glyph "@"  layer sky  when chance(70) => move east  when true => move random free }

# scenarios/tests/weather.scenario: beside packs/gather, which has a `path` of its own
rules ../../packs/gather ../../packs/weather
start weather.cloud 1 / 600
expect eventually max weather.path >= 100 to 2h
```

Scenarios, `expect` lines and the tools name a player's kinds as `red.ant`. A save keeps
each pack's player (`red=/abs/path`). A world without players is as it always was.

The main menu (`wmc` alone) makes a new pack from a template: Packs, then `+ New pack` and a
name, writes `packs/<name>/<name>.rules` (two small kinds, every line explained, their names
carrying the pack's) and `scenarios/<name>.scenario` to try it in, and opens the rules in your
editor; Scenarios, then `+ New scenario`, does the same for a scenario. `+ Copy of a pack`
copies a pack's rules files to `packs/<name>/` with their names unchanged (so a world runs the
copy or the original, not both), and a scenario to try it: the one that plays the original, its
`rules` line the copy's, if there is one; `+ Copy of a scenario` copies a scenario as it is.

A pack need not build on the built-in kinds. `packs/life` is Conway's Game of Life in two
kinds, `dead` and `alive`, and nothing else. `scenarios/life.scenario` plays it on an
endless board of random soup; `scenarios/tests/life_soup.scenario` checks a drawn soup's
live count against a plain Life's up to generation 100, and `life_patterns.scenario` tests
a blinker, a block and a glider. `packs/village` is a small social world, one kind in two
states: villagers work for wealth (a need with `decay 0`), rest when tired, and `give` to a
neighbour whose `look` says it is poorer; `scenarios/tests/village.scenario` holds its
goals (how many at work, the mean and the lowest wealth). `packs/turing` is a Turing machine
of 2 symbols and up to 5 states, the 5-state busy beaver by default, drawn as its history:
a row of the world each sweep of the head. The head is a standing actor that keeps its state
in memory; the tape is ground cover that turns into what the head standing on it wrote
(`look_of(here)`), and a turn of the head opens the row below, which a wave copies one cell a
tick from the row above, so that no rule reads farther than a neighbour. A finished cell
turns into a still twin with a very long `cadence`, which costs almost nothing.
`scenarios/turing.scenario` draws its first 10 000 steps; `scenarios/tests/turing.scenario`
checks a whole row against a plain simulation of the machine. On a board with walls it starts
over: a head whose next step is into rock leaves, a wipe spreads from its cell over the whole
picture, and an `origin` on the cell above where the head began starts a new one
(`scenarios/tests/turing_restart.scenario`).

Four more packs draw pictures, each with a scenario in `scenarios/` and a test in
`scenarios/tests/` checked against a plain simulation. `packs/elementary` is any of
Wolfram's 256 one-dimensional rules (`const RULE`), a row a generation: a cell waits until
the three above it are decided, so rule 90 from one cell draws the Sierpinski triangle; with
a top row of `spring`s, which pick again every so often, the board is erased and redrawn in a
wave, and never stands still.
`packs/cyclic` is a cyclic automaton of 16 colours, where a cell takes the next colour round
the ring once a neighbour has it; from soup, spirals. `packs/turmite` is Langton's ant and
any turmite of up to 8 colours; each ant's rule is its memory, so a scenario starts ants
with different rules (`start ant at (48, 48) with (turns = 12, colours = 4)`).
`packs/lsystem` grows trees as the turtle reading of the L-system `X -> F[+X][-X]`, with
dice in every segment and branch; an old tree withers from its stump (each cell when the one it
grew from has) and grows again. A planted bud `with (trunk = N)` grows a tree of that trunk,
its branches in proportion. `scenarios/glider_gun.scenario` is Gosper's glider gun in
`packs/life` with an eater that swallows its gliders, and
`scenarios/fox_pen_showcase.scenario` a fox and a flock in a small walled pen. Every name is
global, so packs meant to share a world keep their consts and traits apart
(`TAPE_STILL`, `ROW_CADENCE`); all of these compile together with the built-in kinds.
`scenarios/gallery.scenario` runs them all on one board, each world in a rock frame and its
soups in their own regions (§14), and checks that each runs as it does alone.

A save also opens under packs that number things differently, which is what adding a pack
does. Kinds, needs, memory, states and fields are matched **by name**, so every actor
keeps its kind, cell, needs (clamped to a lowered max) and memory. The first time the world
writes to the save, the whole directory moves over to the new numbering, and from then on it
belongs to the new set of packs. Rules that lack one of the save's kinds are refused with the
list, and so is a kind that moved to another layer (standing, ground cover, a declared one by name). A saved start
naming a kind the rules lack (its rows went in a reload, below) is skipped, said so on
stderr, and kept in the save: its share keeps its slice of the draw, so the other kinds
keep their cells, and the kind starts again in new ground when it is back.

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

**Fields.** `trail`, laid by laden bees on their way home, strongest near the flowers, and fading by a 32nd every 16 ticks: its law.

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
  - `signal_of` when no rule sets
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
- **`wmc why [-v] <dir> <x> <y> [time [w h seed]]`** steps `time` (ticks, or `90min`, `2h`,
  `4d`), waits for the actor at (x, y) to think (up to its cadence; past 65536 ticks, about
  3 days, it says when that think comes and how many ticks to give it instead), and prints
  that think. The header shows a group's heads and a big actor's size. It shows the
  actor's needs and memory, and every rule it checked: `FIRED`, `no` (condition false),
  `TRAPPED` (the think trapped in its condition) or blank (not reached); under a `no` whose
  condition has several terms, the one that was false (`false: food < 20h`). Then the decision
  with its effects, what it wrote, the fuel spent, and after the real step where it went and
  its result. `-v` adds every op, up to the one that trapped.
- **Click an actor in `wmc play`** to open the inspector, a see-through panel over the
  right of the map, in a smaller font than the map. At the top: kind, uid and parents,
  age, state, look, signal and last result. Then **next think**: the rules that fire,
  copied out (a rule that only writes memory falls through, so there can be several),
  and the decision, its writes and its cost. Then needs (with a bar), memory, and
  **rules**: every rule of its reflexes and current state, in the order the think tries
  them. Rule text is coloured as an editor colours code: keywords, actions, senses and
  targets, numbers, kind names, the kind's own needs and memory, sub calls. A line too
  long for the panel ends in `...`; click a rule to open it onto as many rows as it
  needs, and again to close it. The line under the pointer is lit. It is `wmc why`
  without the ops, refreshed every frame, and changes nothing. The mouse wheel or the
  trackpad scrolls the panel; the last row says where you are. Clicking the same map
  cell again shows the ground cover under the actor. `f` follows the actor (the camera
  glides after it; moving the camera yourself lets go), and `Esc` closes the panel.
  When the actor dies, the panel keeps its last state, faded.
- **`wmc rates [pack...] [--kind K]`** turns each kind's numbers into day units: thinks a
  day, how long each need lasts, what eating one gives; per rule, its dice as odds a think
  and times a day (if reached and its other gates hold; `gates` counts them), the age it
  waits for, and what it does (`goes on` when it only writes memory). A `die` past an age
  reads as days left on average. It says what a rule does when reached: `wmc observe`
  measures how often it is.
- **`wmc lab <file> --set NAME=a,b,c`** runs the scenario test in `<file>` (a goal: its
  `expect` lines, §14) once for every combination of the `--set` values (the first name
  varies slowest), on each of its seeds, many runs at once: a world each, on one thread,
  `--jobs N` at a time (one per core). Per variant it says whether it passed (`pass M` of
  `seeds N` held every expectation), and per expectation on how many seeds it held and the
  spread of its number across them: the lowest count an `always >=` saw, a mean, a rate
  (`31.2 .. 44, median 38`; the median of an even count is the mean of the middle two).
  Runs go every variant's first seed, then every variant's second, and so on. A run stops
  at its first sure failure (an `always` false, a `never` true), and its later lines read
  `not reached` unless already decided (an `eventually` already true, a window closed); and once more of a variant's seeds failed than `pass` allows, its later
  seeds do not run (`2 not run, it could no longer pass`): a hopeless variant costs little
  (a fox goal's lab of 3 variants x 8 seeds: 176 s down to 45 s). `--full` runs every seed
  of every variant to its end. `--seeds` and `--pass` override the scenario's, `--csv
  <file>` writes every verdict as a row. The report is the same whatever `--jobs` says. An
  optimized build (`cargo build --profile fast`, then `target/fast/wmc`) runs it about
  twice as fast as a dev build.
  ```sh
  wmc lab goals/fox_pressure.scenario --set HUNT=6,8,10 --set FULL=16h,20h --seeds 8 --pass 7
  ```
- **`wmc observe <time> [w h seed] --scenario <file>`** makes the scenario's world (on no
  disk; its `run` and `expect` lines are not run), steps it for that time (ticks, or
  `90min`, `2h`, `4d`) and reports how each kind lives and dies: how many are alive over
  time and the lowest count; births and deaths per day, deaths by cause (`killed by fox`,
  `no water`, `own die`) and per head (deaths a day over the mean alive), and per kind and
  cause the same per head in about 8 windows over the run (a rate that only grows is a
  stock filling up); who kills whom
  (`kills a day`: per killer kind, all and per head, and of which kinds); and per kind, its
  typical needs, how many are in each state, what its thinks decide, the rules that fire
  most (by `file:line`, `#2` for a second rule on one line), and how its actions turn out (`BLOCKED 14%`). Decisions and rules
  come from dry runs of up to `--sample N` (100) thinks per kind at each sample, as the
  inspector runs them: nothing in the world changes. `--gates` adds, per kind, each rule
  those thinks reached: how many reached it and fired it, and per term of its condition's
  top-level `and` how many of the thinks that got that far it stopped, so the gate that
  binds is the one that stops most; each with where one such actor stood (`e.g. (77, 103)
  after 97200 ticks`: `wmc why <dir> 77 103 97200 --scenario <file>` follows it). `--every
  <time>` sets how often it samples (an hour), `--kind K` details only some kinds, and
  `--csv <file>` writes every sample as `tick,day,hour,kind,metric,value` rows, for a closer
  look. The report is the same on any thread count.
  ```sh
  wmc observe 4d 128 128 7 --kind fox --kind chicken --gates --csv fox.csv
  ```
- **`TRAPS b3`** on the play status bar means three bee thinks ran out of fuel or faulted.
  `wmc why` on one of them shows where.
- **`r` in `wmc play`** reloads the rules. A compile error shows on the status bar and
  changes nothing. Actors keep kind, needs, memory, state and fields by name. Rows of a kind
  you removed are dropped: the one way to take a kind out of a save. Saved chunks are
  rewritten to match. Its starts stay in the save and place nobody (`starts skipped` on the
  status bar) until the kind is back (§15).

## 18. Limits

| | |
|---|---|
| needs, mem slots per kind | 4, 32 |
| heads in a group | 65535 |
| a big actor's size | 16 x 16 cells |
| tags per rule set | 64, less one for each layer up to the last that rests on a kind |
| fields per rule set | 64 |
| grounds, features per rule set | 16, 15 (the engine's three among them) |
| declared layers per rule set | 6 (beside standing and cover) |
| players per rule set | 254 |
| fields one think adds to | 4 (a fifth traps) |
| steps in one law | 128 (one per number, name, operator and aggregate, about) |
| numbers in a search's state | 8 |
| states a search expands | 4096 (`limit`; 256 by default) |
| states a search holds | 32768 |
| states per kind | 64 |
| sight | 64 |
| fuel per think | 32768 |
| sub call depth | 8 |
| value stack per think | 64 (shared by a rule and the subs it calls; a rule or sub that needs more by itself is an error: nest less deeply or split with `let`) |
| parameters and locals in one rule or sub | 16 slots (a target takes 2, a `for each` 5, a `choose` 1 per arm plus 1, a `repeat` 1, and `toward`, `away` and `random free` 2 while evaluated) |
| nesting in one rule or sub | 128 levels: each nested statement, parenthesis, call argument, operand, `-`, `not`, `toward` and `away` is one, and so is each operator in a chain (`(m) > 0` is three: the parenthesis, `m` and `>`) |
| compiled size of one rule, one state's rules, one sub | 32767 ops (jumps are 16 bits; split what is longer) |
| distinct constants outside -32768..32767 per rule set | 65536 |
| kinds, subs per rule set | 65534, 65536 (each kind's member subs count once per kind that has them) |
| rules files per rule set | 65536 |
| a chain of `extends` | 128 kinds and traits below the one that extends them |
| chunks a scenario's `size` generates at creation | 4096 (4096 x 4096 cells) |
| seeds a scenario test runs on | 1000 |
| shares with a region in a scenario | 256 |
| cells each way from (0, 0) | 1000000000 (x and y run from -1000000000 to 999999999; beyond, rock and nobody, §14) |

Reserved words can't name a need, mem, local, kind, sub or constant. They are every keyword
in this document, the built-in functions among them (`min`, `max`, `abs`, `sign`, `clamp`,
`rand`, `chance`, `flow`, `dist`, `free`, `is`, ...), plus the sense names (`x`, `y`, `age`,
`light`, `hour`, `day`, `kind`, `look`, `signal`, `state`, `hurt`, `hurt_dir`, `result`,
`taken`, `trapped`, `heads`), and `place` (where kinds start is a scenario's `start` line now, §14).
`wmc lint` says so when you hit one. `water`,
`soil`, `rock`, `bare` and `food` are not reserved: a kind may have `need water`. But
`water`, `soil`, `rock` and `bare` always mean the predicate where one is read, so they can't
name a kind or a tag.
