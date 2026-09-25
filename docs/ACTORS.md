# Actors

Status: designed 2026-09-24 (a five-proposal design panel, adversarially judged, then
synthesised and critiqued; the raw material is outside the repo). Being built in the order
of §11. This file is the design; `ARCHITECTURE.md` holds the numbered decisions it adds.

An **actor** is anything that stands on a cell and is governed by rules: a seed, a tree, a
chicken, a fox, a bee. An actor has **senses** (what it can ask about the world), **needs**
(counters that make it act and kill it at zero) and **directives** (a rules file per kind,
compiled once, run by every individual of that kind against its own state).

## 1. The shape

An actor is a **row in the chunk it stands on**: a 12-byte public record (`ActorPub`) that
any chunk may read while thinking, an 88-byte private record (`ActorMind`) that only its
own chunk touches, and the `occupant` entry of its cell, which packs `(kind, slot)`. A
**cover** kind (grass) sits in the cell's `cover` entry instead: walkable ground cover that
lies under whoever stands on the cell and never blocks a move. There is no actor entity. A **kind** is a text file compiled at world open into bytecode plus a
property table (`Res<Programs>`), shared read-only by every thread. Persistent per-actor
state is exactly needs + memory + a state byte: no saved program counter, so a think is a
pure function of (own row, tick-start world, tick, seed).

```
SimTick  (Phase sets chained; one system per set; ambiguity_detection = Error)
 Simulate  par   scent_decay (cadence 16)     W own ChunkCells.scent
 Think     par   actors::think                R Tick SimConfig Programs Stage, ANY ChunkCells + ChunkActors
                                              W own ChunkMinds Intents Outbox ChunkMeta.dirty
 Resolve   par   actors::resolve              sort own intents by key; WAKE consumed; in-chunk bites recorded on the victim chunk
 Exchange  seq   actors::exchange             cross-chunk bites recorded; per victim: bites in key order, hurt, WAKE, death; food by share
 Apply     par   actors::apply                claim winners move/spawn; become; die; signal/look/mark; result
 Migrate   seq   actors::migrate              stage.active() order: cross-chunk move/spawn
 Compact   par   actors::compact              swap-remove DEAD rows, repair occupant, reset claims
 Advance         advance_tick                 W Tick
```

Thinking is separate from doing: a think emits one **intent**; the resolve phases settle
conflicts with keys derived from state (never from threads, slots or entity ids); only
work that touches two chunks at once runs sequentially, in coordinate order.

## 2. Storage

```rust
#[repr(C)] pub struct ActorPub {   // 12 B, Pod. Read by any chunk in Think; written by the owner outside Think.
    cell: u16,      // local cell; invariant: occupant[cell] == ActorId::pack(kind, slot)
    kind: u16,      // index into Programs.kinds (0xFFFF reserved by ActorId::NONE)
    stagger: u16,   // uid low bits: cadence phase
    signal: i16,    // rule-written, readable by others via signal_of(t)
    look: u8,       // rule-written appearance: palette variant + `kind:look` predicate
    flags: u8,      // DEAD | WAKE
    _pad: u16,
}
#[repr(C)] pub struct ActorMind {  // 88 B, Pod. Only the owning chunk touches it.
    uid: u64,                 // identity: hash_cell(seed, STREAM_UID, x, y) [^ splitmix64(tick) when spawned at run time]
    born: u32, last_think: u32,   // wrapping ticks
    needs: [i32; 4],          // ticks-until-empty, or points when decay 0; named per kind
    mem: [i32; 12],           // the program's whole persistent memory, named per kind
    state: u8, events: u8, hurt: u8, hurt_dir: u8,
    _pad: u32,
}
#[derive(Component)] pub struct ChunkActors { rows: Vec<ActorPub> }   // reserved capacity per chunk
#[derive(Component)] pub struct ChunkMinds  { rows: Vec<ActorMind> }  // same length, same order
```

- **Two components, not one**, so Think can read every chunk's `ChunkActors` while writing
  its own `ChunkMinds` (Bevy refuses the alternative). The every-tick scans (cadence
  due-check, dead-check) stream only the 12-byte rows.
- **`ActorId` keeps its `u32`**; a live value is `kind << 16 | slot`. A vision scan learns
  what stands on a cell from the occupant array alone. Slots are chunk-local and valid only
  within a tick; identity across ticks is `uid` or a position (unique: one standing and one cover actor
  per cell).
- **Worldgen rows** get a tick-free `uid`, so regenerating a chunk equals reloading it.
  Run-time spawns fold in the tick.
- **Migration**: a move into another chunk goes to the source `Outbox`; `Migrate` walks
  `stage.active()`, copies the *current* row into the target (damage taken this tick travels
  with it), writes the target's occupant, flags the source DEAD. An unloaded target is a
  wall, so actors never leave the loaded set.
- **Death and spawn without `Commands`**: death = `DEAD` flag + occupant cleared, by
  whichever phase kills. Spawn = a cell claim, then a push in the owner's `Apply` (or in
  `Migrate` for a foreign cell). Rows are only appended during a tick and only removed in
  `Compact`, so a slot in this tick's intents can never mean a reused row. A chunk's row
  `Vec` may grow past its reserve (chunk-level, amortised, counted in the bench); a
  per-actor allocation never happens.
- **Cover**: a kind declared `cover` lives in `ChunkCells.cover` and carries `flags::COVER`
  in its row; at most one per cell, so a chunk holds up to 2 x 4096 rows. It can be eaten
  (`graze`), spawned onto a walkable cell without cover and killed, but never moves and
  never `become`s a standing kind. Searches see both layers: a kind or tag pred matches the
  occupant or the cover, `free` asks only about the occupant, `bare` means walkable with no
  cover. The renderer tints a covered cell toward the cover's colour and draws the occupant,
  else the cover's glyph.
- **Save**: chunk file v7 = cell layers (`occupant`, then `cover`), `n`, `ActorPub[n]`,
  `ActorMind[n]` as raw LE bytes; every row validated on load (`kind` in range, `cell` in
  range, the row's layer agrees). A chunk
  holding any row is **dirty** once actors think (undirtied rows would vanish on unload).
  `world.wmc` carries the kind name table; rows are remapped by name on load.
- **Cadence**: `cadence 2^k` per kind; an actor is due when `(tick + stagger) & (2^k - 1) ==
  0` or `WAKE` is set. Stagger is per actor (decision 28). Only `hurt` and being taken from
  set `WAKE`; the result of an action is read at the next scheduled think.
- **RNG**: counter-based, no stream state: draw `n` for `uid` at `tick` is
  `splitmix64(splitmix64(seed ^ STREAM_THINK ^ splitmix64(tick) ^ uid) + n)`. Claim key
  `splitmix64(uid ^ splitmix64(tick))`, compared as a full `u64`.
- **Frozen chunks**: on load, `last_think` and `born` shift forward by the frozen interval
  (`now - last_ticked`), so nothing decays or ages off screen and a reopen at the save tick is
  bit-identical to never stopping (decision 29: freeze, not catch-up).
- **Needs on `become`**: consumable needs (ticks-until-empty) carry over by name, clamped
  to the new max; point needs (`decay 0`, e.g. health) reset to max. Needs the new kind
  adds start at max. Memory carries by name, the rest is zeroed; `state` resets.

Per actor: 104 B persistent (12 + 88 + 4 occupant) + 32 B intent scratch.

## 3. Senses

Pulled, not pushed: a sense is a bytecode op evaluated when a rule asks, against tick-start
state (nothing writes `ChunkCells`/`ChunkActors` during Think, so the live state *is* the
snapshot; no `Prev` copy). Every Think task resolves its 3x3 chunk halo once; `sight <= 16`.

| group | senses | source |
|---|---|---|
| self | each need and mem by name, `age`, `x`, `y`, `kind`, `look`, `signal`, `state`, `light`, `hour`, `day` | own rows, `Tick`, `time::daylight`, `Clock::at` |
| events | `hurt`, `hurt_dir`, `result` (OK / BLOCKED / MISSED / REFUSED / NONE; `blocked`, `missed`, `refused` are shorthands for `result == ...`), `event(TAKEN)`, `event(FUEL)` | latched bytes written by the resolve phases, cleared after the think that read them |
| here / at | `ground`, `feature`, `scent(ch)`; `ground_at(t)`, `feature_at(t)`, `free(t)`, `is(t, pred)`, `look_of(t)`, `signal_of(t)` | cells and public rows in the halo; unloaded = rock, no actor |
| search | `nearest pred within r as v`, `count pred within r`, `for each pred within r as v`, `sniff ch within r as v` | Chebyshev rings 1..=r, row-major in a ring, ring start rotated by one RNG draw |
| geometry | `dist(t)`, `t.dx`, `t.dy`, `toward t`, `away t`, `at(x, y)` | arithmetic |

A *pred* is one integer at run time: a kind, `kind:look`, a tag, a ground, a feature,
`free` or `bare`; kinds and tags share one namespace so `sub forage(what, r)` works for every
herbivore, and a kind or tag matches the cell's occupant or its cover.
**The message system is the latched byte set**: bounded, Pod, delivered to exactly the next
think. Hits in one tick sum into `hurt` (saturating); `hurt_dir` is the lowest-key attacker.
No per-actor queues. Broadcast goes through `signal`, `look` and per-cell scent layers.

## 4. Needs

`need water max 2h vital`: an `i32` in **ticks-until-empty**, decremented lazily by
`tick - last_think` when a think starts. `decay 0` needs are in points (health). A vital
need at 0 makes the think emit `die` before any rule runs. Rules read needs by name and may
write them (`water += 6h`, clamped to max); world-validated refills come from resolved
actions (`eat`, `drink`, `take`/`give`). Dynamic objectives are a `state` plus targets in
`mem`; first-class `goal` syntax waits for two kinds that need it.

## 5. Directives: the language

Three nested formalisms: a **finite-state machine** per kind (`state` blocks, `next`), a
**priority-ordered list of productions** per state (`when cond => body`, first match), and
a **small imperative body** (assignments, if/while/for-each, sub calls, weighted random,
exactly one action). Compiled at load to bytecode for a fuel-bounded integer stack VM.

```ebnf
file     := item*
item     := "include" STRING | "const" NAME "=" expr | sub | kind
kind     := "kind" NAME [ "extends" NAME ] "{" decl* rule* state* "}"
decl     := "glyph" STRING | "color" STRING | "cover" | "tags" NAME+
          | "cadence" INT | "sight" INT | "fuel" INT | "bite" INT
          | "food" TIME | "place" INT "/" INT                 # worldgen share of walkable cells
          | "need" NAME "max" (INT | TIME) [ "decay" INT ] [ "vital" ]
          | "mem" NAME ("," NAME)*
state    := "state" NAME "{" rule* "}"
rule     := "when" cond "=>" body
sub      := "sub" NAME "(" [ param ("," param)* ] ")" block
param    := NAME [ ":" ("target" | "pred") ]                 # default int
body     := stmt | block
block    := "{" stmt* "}"
stmt     := action | effect
          | lvalue ("=" | "+=" | "-=") expr | "let" NAME "=" expr
          | NAME "(" [ expr ("," expr)* ] ")"
          | "if" cond block [ "else" (block | "if" ...) ]
          | "while" cond block | "repeat" expr block
          | "for" "each" pred "within" expr "as" NAME block
          | "choose" "{" (expr ":" body)+ "}"
          | "next" NAME | "return" [ expr ]
action   := "idle" | "die" | "become" NAME
          | "move" target | "eat" target | "hit" target | "graze" target | "drink" target
          | "take" target NAME expr | "give" target NAME expr
          | "spawn" NAME "at" target [ "with" "(" expr "," expr ")" ]
effect   := "signal" "=" expr | "look" "=" expr | "mark" NAME expr
cond     := expr | "nearest" pred "within" expr "as" NAME | "sniff" NAME "within" expr "as" NAME
          | cond "and" cond | cond "or" cond | "not" cond | "(" cond ")"
target   := NAME | "here" | "attacker" | "toward" target | "away" target | "at" "(" expr "," expr ")"
          | "north" | "east" | "south" | "west" | "dir" "(" expr ")" | "random" "free"
pred     := NAME [ ":" INT ] | "water" | "soil" | "rock" | "free" | "bare"
expr     := INT | TIME | NAME | sense | "blocked" | "missed" | "refused" | "(" expr ")"
          | expr ("+"|"-"|"*"|"/"|"%"|"<"|"<="|"=="|"!="|">="|">"|"and"|"or") expr | "not" expr
          | "rand" "(" expr ")" | "chance" "(" expr ")" | "count" pred "within" expr | "dist" "(" target ")"
          | ("min"|"max"|"abs"|"sign"|"clamp"|"pack"|"hi"|"lo") "(" expr ("," expr)* ")"
          | NAME "(" [ expr ("," expr)* ] ")"
TIME     := INT ("min" | "h" | "d")
```

**Semantics.**
- One value type, `i32`, wrapping; `x / 0 == 0`; conditions are nonzero; `TIME` is ticks.
- A think: decay; `die` if a vital need is 0; scan the rules outside any state (reflexes),
  then the current state's rules, top to bottom; the first rule whose `cond` holds runs its
  body **to completion**. A body that emitted an action or a `next` ends the think. A body
  with neither falls through and scanning continues. Falling off the end is `idle`.
- **One action per think.** A second action is a compile error where visible, a trap at
  run time. Effects (`signal`, `look`, `mark`) combine with the action. An action always
  emits; a `move` with no free cell is simply `BLOCKED` in `result`.
- `as v` bindings are visible in the body only when the search is a top-level conjunct.
- `choose` evaluates all weights (clamped >= 0), draws once, runs that arm.
- `state` blocks follow the reflex rules; an actor starts in the first one, `next NAME`
  switches for the following think and ends this one like an action, `become` resets to
  the first. `next` inside a sub is a compile error (states belong to a kind).
- `for each pred within r as v { }` visits the matching cells of rings `1..=r` in a fixed
  order (each ring clockwise from its top-left corner, no rotation: it visits them all),
  pays the search's fuel once, and binds `v` per cell. There is no `break`: an action in
  the body that runs twice traps; collect into locals and act after the loop.
- `const NAME = expr` is folded at compile time (numbers, earlier constants, arithmetic,
  comparisons, `min`/`max`/`abs`/`sign`/`clamp`/`pack`/`hi`/`lo`); a need, mem or local of
  the same name shadows it. `kind:look` (`flower:1`) matches that kind showing that `look`
  byte; `look_of(t)` and `signal_of(t)` read the public bytes of whoever stands at `t`
  (else its cover; 0 for nobody). `pack(a, b)` is `a * 256 + (b & 255)`, `hi`/`lo` take it
  apart as signed bytes, so a direction fits one `signal`.
- Subs are file-scope, shared by every kind; may act (the think still ends when the rule
  body that called them finishes); recursion depth 8. Locals never persist.
- **Fuel** is charged per bytecode op plus `(2r+1)^2 / 8` per search; default 512, kind
  override up to 4096. Fuel out, depth > 8 or a trap ends the think with `idle`, sets
  `event(FUEL)` and bumps a per-chunk counter. The sim never panics on a rules file.
- Compiled at world open (`rules/compile.rs`: lexer, recursive-descent parser, codegen
  through `rules/asm.rs`): `Vec<Op>` with a constant pool (immediates are 16-bit; `3d` =
  64 800 goes to the pool), kind ids in **sorted file name then declaration order**, the
  rules hash recorded in `world.wmc` and folded into the checksum. `water`, `soil`, `rock`,
  `free` and `bare` are contextual words: predicates after `count`/`nearest`/`is`/`random`, plain
  names elsewhere, so `need water` and `water < 40min` read as intended; `food` likewise is a
  declaration only where a declaration starts (`need food`, `food < 20h` work). `x` and `y`
  are senses, so they cannot name a parameter or local. A pred name is a sub's `pred`
  parameter, else a kind, else a **tag**: tags are global names numbered in first-appearance
  order (64 at most, never a kind's name), a kind's tags a bitset the VM checks against the
  occupant (`nearest meat within 8`). `place N / D` gives a kind a share of walkable cells at
  worldgen: one placement hash per cell, the shares cut `0..2^24` into intervals in kind
  order, so the terrain parameters in the save header are terrain only (decision 31).
  `color "#rrggbb"` is the glyph's colour (default a pale yellow), `cover` makes the kind
  ground cover (§2), `dir(h)` is the step for heading `h` (1..8 clockwise from north, 0 =
  none). The files in `rules/` (animals, grass, plants) are built into the binary; `WMC_RULES=<dir>` swaps in a directory; `wmc lint` compiles and
  prints the kind table. A radius after `within` is an additive expression, never a
  comparison (`count water within 2 > 0` counts within 2). `wmc why <x> <y>` re-runs one
  actor's think with per-op logging (step 7).

**Example** (abridged; `rules/animals.rules` has the full one).

```
sub flee(t: target) { if free(away t) { move away t } else { move random free } }
sub forage(what: pred, r) {                # graze cover `what` underfoot, else walk onto the nearest
  if is(here, what) { graze here }
  else if nearest what within r as s { move toward s }
}
sub turn(h) {                              # mostly straight on
  if h == 0 { return rand(8) + 1 }
  choose { 80: return h   8: return h % 8 + 1   8: return (h + 6) % 8 + 1   4: return rand(8) + 1 }
}

kind chicken {
  glyph "C"   color "#f2ead8"
  tags animal meat
  cadence 4   sight 6   food 1d   place 1 / 400
  need food   max 1d vital
  need water  max 4h vital
  need health max 20 decay 0 vital
  mem knows_water, water_x, water_y, heading, detour, last_egg
  when hurt > 0                    => { detour = 0  flee(attacker) }
  when nearest fox within 5 as f   => flee(f)
  when nearest water within 6 as w => { knows_water = 1  water_x = x + w.dx  water_y = y + w.dy }
  when blocked                     => { heading = rand(8) + 1  detour = 3 }   # walk round it
  when detour > 0                  => { detour -= 1  move dir(heading) }
  when water < 90min and nearest water within 1 as w => drink w
  when water < 90min and knows_water == 1 => move toward at(water_x, water_y)
  when food < 20h                  => forage(grass, 6)
  when hour >= 6 and hour < 18 and food > 20h and day + 1 > last_egg and rand(1000) < 2
       and count meat within 6 < 3 and nearest free within 1 as c
       => { last_egg = day + 3  spawn egg at c }       # dice before the count: it short-circuits
  when hour >= 20 or hour < 5      => { look = 1  idle }
  when true                        => { look = 0  heading = turn(heading)  move dir(heading) }
}
```

## 6. Actions and conflict resolution

Movement and adjacency are 8-neighbour (matching Chebyshev vision); `move toward t` steps
`(sign dx, sign dy)` and slides around a blocked cell via the two 45-degree neighbours.

1. **Resolve** (parallel, own chunk): sort intents by `key`; clear `WAKE` of every actor
   that thought; an `eat`/`hit` must be adjacent (else REFUSED) and find a standing actor
   with a `health` need (empty cell: MISSED; no health: REFUSED); a `graze` bites the cell's
   cover instead and may target its own cell (`graze here`); an in-chunk bite is recorded as
   a `Hit` on the chunk's scratch, a cross-chunk one goes to the `Outbox`.
2. **Exchange** (sequential): cross-chunk bites recorded on their victims against tick-start
   occupancy; then chunk by chunk in `stage.active()` order, bites grouped per victim (and
   layer): in key order each takes up to its `bite` from the health left, `hurt` grows
   (saturating), `hurt_dir` points at the lowest-key biter (the `attacker` target reads it),
   `WAKE` set. An `eat` or `graze` that took `t` points gains `food * t / max_health` of the
   victim kind's `food` into its own `food` need, so a kill feeds every biter by its share
   and a grazed tuft feeds without dying. A `hit` never feeds. At `health <= 0` the row is
   DEAD; a standing victim's cell is cleared and touched, a cover victim's is not. No move
   has been applied yet, so damage is symmetric across borders. (Damage is
   summed on one thread here rather than per chunk in Resolve: bites are rare next to
   thinks, and the sequential sum needs no cross-chunk credit pass; the per-chunk split is
   the hatch if Exchange ever shows in a profile.)
3. **Apply** (parallel): intents of DEAD actors dropped; claims skip touched cells; `key ==
   claim[target]` wins the cell; losers get `BLOCKED`; a cover spawn claims nothing and takes
   its cell in key order if it is walkable and has no cover yet; a cover row's `move` and a
   `become` across layers are REFUSED; `become`, self-`die` (a standing actor touches its
   cell), `drink`, `look`, `result` written. Births, `become`s, bites that fed and deaths are
   counted per kind (`Tally`: not saved, not hashed; the status rows of `wmc play` and the
   table after `wmc run` print it).
4. **Migrate** (sequential): cross-chunk `move`/`spawn` into cells free now and not touched
   this tick; contenders settled by key. An in-chunk winner beats a cross-chunk one (**home
   advantage**, deterministic, documented; decision 30).
5. **Compact** (parallel): swap-remove DEAD rows top-down, repair the moved row's occupant,
   reset claims.

## 7. Why it is deterministic

Thread count, batching and entity ids can reach a result only through a shared mutable
write, an iteration order or a random source. Every parallel phase writes only the chunk it
was handed; per-chunk order is sorted-key order; claims use `min` and damage uses `+`, both
commutative; every sequential phase walks `stage.active()`; every random draw is
`f(seed, tick, uid, n)`. Not promised: independence from chunk borders (`CHUNK_BITS` joins
the world's identity) and from the rules text (its hash is part of the checksum).

Tests per step: two-fresh-worlds checksum per new system; `crates/app/tests/determinism.rs`
at 1/3/8 threads; save at T, reopen, step to T+N equals the continuous run; VM unit tests
(fuel, `/0`, wrapping, `choose`, traps); a schedule-build test that every actor system has
its own `Phase` set.

## 8. Cost hypotheses (unmeasured; the first PERF rows replace them)

~5 ns per op, 3-5 ns per occupant word scanned: a chicken think (one to three searches at
r = 6) ≈ 1.5-2.5 µs, a tree ≈ 0.4 µs. 100k actors in 256 chunks (75k plants at cadence 512,
20k chickens at 8, 5k foxes at 4) ≈ 3.9k thinks/tick ≈ 10-12 ms serial, 2-3 ms on 8 threads,
against a 125 ms tick at 8 TPS. Breaks at everything-cadence-1-with-sight-8 (≈ 25 TPS
ceiling; answer: cadence 2-4 + cached targets), one chunk of 1024 cadence-1 actors (split by
slot range inside the chunk), or a border stampede (bucket outboxes by target chunk).

## 9. Decisions taken (owner, 2026-09-24)

| choice | taken | why |
|---|---|---|
| syntax | productions with braces | reads as a priority list; the bytecode is front-end agnostic |
| plans | reactive re-evaluation + `state` blocks | no saved pc: reflexes interrupt for free, `wmc why` is a re-run |
| control flow | Turing complete with fuel | loops cost nothing unused; bounded per think |
| stagger | per actor `uid` | organic motion, no phase jump on migration |
| social bytes | `signal`, `look` in the row from commit one | avoids a format bump for the first social creature |
| public surface | kind, look, signal, position | an actor controls what it broadcasts; needs stay private |
| off-screen | freeze (clocks shift by the frozen interval) | decision 25 as it stands; reload at the save tick equals the continuous run |
| borders | home advantage | one asymmetric case; the symmetric protocol is the hatch |
| rows | `Vec` + compaction, not fixed slabs or a free list | dense scans, no hidden capacity, no mid-tick slot reuse |

## 10. Rejected

Embedded scripting (Lua, Rhai, Rune, wasm: per-thread instances, allocation, f64); one entity
per actor (`Commands` in hot phases, ids in results); a global actor slab (locks or a serial
phase per spawn); per-actor message queues (allocation + delivery-order contract); an
`ActorsPrev` double buffer (the public/private split gives the snapshot for free); a
precomputed sense record per think (pays for interests never read); a persisted program
counter (suspension mid-loop loses locals); a tree-walking AST (`Box` in hot data);
condition/action tables (not Turing complete); permille needs with per-think decay (rounds
to zero); statement-counted fuel (off by 15-60x); kind ids from directory order; `u32` claim
keys (two winners); waking on every action result (collapses cadence to 1).

## 11. Build order

Each commit ends with `make ci` green, a determinism checksum, and a `docs/PERF.md` row
where it touches the tick.

1. **Rows and store.** `actors` module: `ActorPub`, `ActorMind`, `ChunkActors`,
   `ChunkMinds`, `ActorId::pack/unpack`, push/kill/compact, store v3 with the kind table,
   load validation, checksum over rows. Worldgen places `seed` rows by `hash_cell`; the
   palette draws a kind's glyph. Proves: rows freeze, save, reload and hash bit-identically.
2. **VM with hand-assembled programs.** `rules::vm`, `Op`, `KindDef`; seed and tree as
   `Op` literals; Think/Apply/Compact only (`idle`, `become`, `spawn`, decay, cadence,
   wake). Proves: the ISA, fuel, decay, `become`, spawn claims, the first `wmc run`
   checksum at 1/3/8 threads with a spreading forest.
3. **Compiler for the plant subset**, `wmc lint`, the rules hash in `world.wmc`. Replace
   the literals; the compiled plants must reproduce the hand-assembled bytecode (they do,
   after the assembler was aligned to the compiler's immediate-vs-pool choice, which moved
   the rules hash once; populations and the `step` bench are unchanged).
4. **Chicken.** `move` (unit steps that slide around a blocked cell), `drink`, in-chunk
   claims, `Outbox` + the sequential Migrate phase (cross-chunk moves and spawns, contenders
   by key, home advantage), `result`, subs with int/target/pred parameters and `return`,
   targets (`toward`, `away`, `at`, `random free`, directions, `.dx`/`.dy`, `dist`, `free`,
   `is`), `let`, `while`, `repeat`; chickens placed by worldgen (`animal_density`). Done:
   `rules/animals.rules`; a game day of wandering with invariants checked; Migrate tested on
   its own; 1/8-thread checksums equal with 21k chickens on 4096 chunks. `eat`/`graze` wait
   for step 5 (they need damage resolution).
5. **Fox and eggs.** Done: `eat`/`hit`/`bite`, the Resolve and Exchange phases (damage,
   `hurt`/`hurt_dir`/`WAKE`, death, kill credit to the lowest-key eater, since replaced
   by food by share in 5b), the `attacker`
   target, the `look =` effect, tags as predicates, `place N / D` (placement moved from
   `GenParams` into the rules, store v6). Content: chickens graze `feed`, remember water,
   flee foxes and lay eggs; eggs hatch; foxes drink, sleep by day, hunt `meat` and breed;
   seeds are food and trees seed faster. Tests: damage resolution on hand-made intents
   (in-chunk + cross-border eaters, credit by key, hit, miss, refuse), a fox eating penned
   chickens on both sides of a border with the real rules, grazing and hatching, two days of
   the built-in world; the cross-process gate now runs 36 game hours of it.
5b. **Behaviour pass: walkable grass.** Done: the `cover` layer (store v7), `graze`, `bare`,
   `color`, `dir(h)`, `blocked`/`missed`/`refused`, bites in key order with food by share,
   the per-kind `Tally`. Content: `rules/grass.rules` (roots reach water 8 cells away,
   spreads onto bare ground, grows back when left alone); chickens keep a heading and walk
   round what blocks them, lay by day from day 0 (at most one egg in 3 days, where there is
   feed and few others), eggs hatch into chicks that grow into hens; foxes hunt `meat` within
   8 and follow the last place they saw it, hold a territory and have a litter only where
   prey is plentiful; old age is a daily chance past an age, so cohorts do not die together.
   Tests: grazing onto walkable grass, bites by share, the hatch-to-chick path.
6. **Social primitives.** `signal`, `take`/`give`, `mark`/scent layers (store v8), `state`
   blocks, `for each`, `sniff`; the bee is the acceptance test. Done so far: `state`/`next`,
   `const`, `for each`, `signal =`, `look_of`/`signal_of`, `kind:look`, `pack`/`hi`/`lo`
   (new opcodes appended, so the programs of the existing kinds and their hash are unchanged).
7. **Tooling.** Hot reload, `wmc why`, fuel/trap counters in the status line, `docs/RULES.md`.
