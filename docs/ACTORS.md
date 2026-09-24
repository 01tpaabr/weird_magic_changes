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
own chunk touches, and the `occupant` entry of its cell, which packs `(kind, slot)`. There
is no actor entity. A **kind** is a text file compiled at world open into bytecode plus a
property table (`Res<Programs>`), shared read-only by every thread. Persistent per-actor
state is exactly needs + memory + a state byte: no saved program counter, so a think is a
pure function of (own row, tick-start world, tick, seed).

```
SimTick  (Phase sets chained; one system per set; ambiguity_detection = Error)
 Simulate  par   scent_decay (cadence 16)     W own ChunkCells.scent
 Think     par   actors::think                R Tick SimConfig Programs Stage, ANY ChunkCells + ChunkActors
                                              W own ChunkMinds Intents Outbox ChunkMeta.dirty
 Resolve   par   actors::resolve              sort own intents by key; in-chunk damage (sums); cell claims (min key)
 Exchange  seq   actors::exchange             stage.active() order: cross-chunk damage, take/give, finalize deaths
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
  within a tick; identity across ticks is `uid` or a position (unique: one actor per cell).
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
- **Save**: chunk file v3 = cell layers, `n`, `ActorPub[n]`, `ActorMind[n]` as raw LE bytes;
  every row validated on load (`kind` in range, `cell` in range, occupant agrees). A chunk
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
| events | `hurt`, `hurt_dir`, `result` (OK / BLOCKED / MISSED / REFUSED / NONE), `event(TAKEN)`, `event(FUEL)` | latched bytes written by the resolve phases, cleared after the think that read them |
| here / at | `ground`, `feature`, `scent(ch)`; `ground_at(t)`, `feature_at(t)`, `free(t)`, `is(t, pred)`, `look_of(t)`, `signal_of(t)` | cells and public rows in the halo; unloaded = rock, no actor |
| search | `nearest pred within r as v`, `count pred within r`, `for each pred within r as v`, `sniff ch within r as v` | Chebyshev rings 1..=r, row-major in a ring, ring start rotated by one RNG draw |
| geometry | `dist(t)`, `t.dx`, `t.dy`, `toward t`, `away t`, `at(x, y)` | arithmetic |

A *pred* is one integer at run time: a kind, `kind:look`, a tag, a ground, a feature or
`free`; kinds and tags share one namespace so `sub graze(what, r)` works for every herbivore.
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
decl     := "glyph" STRING | "tags" NAME+ | "cadence" INT | "sight" INT | "fuel" INT | "bite" INT
          | "food" TIME
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
          | "move" target | "eat" target | "hit" target | "drink" target
          | "take" target NAME expr | "give" target NAME expr
          | "spawn" NAME "at" target [ "with" "(" expr "," expr ")" ]
effect   := "signal" "=" expr | "look" "=" expr | "mark" NAME expr
cond     := expr | "nearest" pred "within" expr "as" NAME | "sniff" NAME "within" expr "as" NAME
          | cond "and" cond | cond "or" cond | "not" cond | "(" cond ")"
target   := NAME | "here" | "toward" target | "away" target | "at" "(" expr "," expr ")"
          | "north" | "east" | "south" | "west" | "random" "free"
pred     := NAME [ ":" INT ] | "water" | "soil" | "rock" | "free"
expr     := INT | TIME | NAME | sense | "(" expr ")"
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
- Subs are file-scope, shared by every kind; may act (the think still ends when the rule
  body that called them finishes); recursion depth 8. Locals never persist.
- **Fuel** is charged per bytecode op plus `(2r+1)^2 / 8` per search; default 512, kind
  override up to 4096. Fuel out, depth > 8 or a trap ends the think with `idle`, sets
  `event(FUEL)` and bumps a per-chunk counter. The sim never panics on a rules file.
- Compiled at world open and on hot reload: lex, parse, resolve, `Vec<Op>` with a constant
  pool (immediates are 16-bit; `3d` = 64 800 needs the pool), kind ids in **sorted file
  name then declaration order**, bytecode hash saved in `world.wmc` and folded into the
  checksum. `wmc why <x> <y>` re-runs one actor's think with per-op logging.

**Example.**

```
sub graze(what, r) {                       # shared by every herbivore
  if nearest what within r as s {
    if dist(s) == 1 { eat s } else { move toward s }
  }
}
sub wander() { choose { 3: move random free   2: idle } }

kind chicken {
  glyph "c"
  tags animal meat
  cadence 8   sight 6   food 4h
  need food   max 3h vital
  need water  max 2h vital
  need health max 20 decay 0 vital
  mem last_egg
  when hurt                       => flee(hurt_dir)
  when nearest fox within 5 as f  => flee(f)
  when water < 40min              => drink_from(6)
  when food < 2h                  => graze(feed, 6)
  when age > 1d and food > 2h and day > last_egg and chance(2)
       and nearest free within 1 as c => { last_egg = day; spawn egg at c }
  when hour >= 20 or hour < 5     => { look = 1; idle }
  when true                       => wander()
}
```

## 6. Actions and conflict resolution

Movement and adjacency are 8-neighbour (matching Chebyshev vision); `move toward t` steps
`(sign dx, sign dy)` and slides around a blocked cell via the two 45-degree neighbours.

1. **Resolve** (parallel, own chunk): sort intents by `key`; in-chunk `eat`/`hit` subtract
   `bite` from the victim's health and post `hurt` (sums commute); `move`/`spawn` into a
   cell free at tick start writes `claim[cell] = min(claim[cell], key)`. Cross-chunk work
   and every `take`/`give` go to the `Outbox`.
2. **Exchange** (sequential): cross-chunk damage; transfers, takers in key order; then
   deaths are finalized: `health <= 0` gets DEAD, occupant cleared, lowest-key hitter
   credited the victim kind's `food`. No move has been applied yet, so damage is symmetric
   across borders.
3. **Apply** (parallel): intents of DEAD actors dropped; `key == claim[target]` wins the
   cell; losers get `BLOCKED`; `become`, self-`die`, effects, `result` written.
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
3. **Compiler for the plant subset**, `wmc lint`, `Programs.hash` in `world.wmc`. Replace
   the literals; the step-2 checksum must not change. PERF row: `step/16x16` with 10k plants.
4. **Chicken.** `move`, claims, `Outbox`, Migrate, `result`, `sight`, subs, `graze`, `flee`.
   Determinism test with a pen straddling a chunk border.
5. **Fox and eggs.** `eat`/`hit`/`bite`, Resolve/Exchange damage, death finalization, kill
   credit, `hurt`, wake-on-event, `look`.
6. **Social primitives.** `signal`, `take`/`give`, `mark`/scent layers (store v4), `state`
   blocks, `for each`, `sniff`; the bee is the acceptance test.
7. **Tooling.** Hot reload, `wmc why`, fuel/trap counters in the status line, `docs/RULES.md`.
