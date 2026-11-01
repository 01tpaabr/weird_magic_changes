# The world

Status: **design, nothing built** (owner, 2026-11-01; ARCHITECTURE decisions 49-53). The cost
measurement in §8 is done; the build order is §10. Syntax below is a proposal: it lands in
`GRAMMAR.md` and `RULES.md` with each step, and may change there.

## 1. The principle

Today the engine owns the world's laws: soil and water are Rust enums, water is not walkable,
rock blocks, scent fades by 1/32 every 16 ticks, and nothing a rule does changes the terrain.
The world should follow the same rule as the actors:

> The engine is the substrate: a grid of cells, ticks, chunks, determinism. Every layer of
> the world, and every law that changes it, is declared by a pack and acts **locally**: a
> cell, its neighbours, the agents on it. Nothing global but the clock.

What a world does over time then comes from local interactions (cells with cells, agents with
cells, agents with agents), never from a fixed rule of the engine or a top-down process.

Three tiers of thing, chosen by how dense they are:

| tier | lives in | costs | for |
|---|---|---|---|
| **field** | a number in every cell | an array per chunk; a law runs over whole arrays | sugar, heat, moisture, height, water depth, pheromone, owner |
| **slot** | at most one per cell per layer | an id per cell, sparse rows | grass, an egg lying there, a bird in the air, a root below |
| **actor** | sparse rows | a VM think | foxes, villagers, soldiers |

A field with a law is a cell that thinks, in the cheapest form there is. Conway's Life as a
field law costs about a fortieth of Life as an actor per cell (§8).

## 2. Fields

```text
field sugar max 4 {                  # 0..=4 in every cell: a byte (a max over 255: 16 bits)
  init sugar_cap                     # starts at another field's value (or a number, or noise)
  every 1h { sugar = min(sugar + 1, sugar_cap) }      # its law (§3)
}
field sugar_cap max 4 { init noise scale 30 }         # no law: set at creation, changed by agents
field fox_scent max 255 { every 16 { fox_scent -= (fox_scent + 31) / 32 } }   # today's scent
```

- **Storage.** One array per declared field in each chunk (`ChunkCells`), allocated when the
  chunk is made, never inside a tick. Folded into the chunk hash, saved by name (`store`,
  `FORMAT_VERSION` bumped), matched by name on reload like needs. Only fields some pack
  declares exist; a world without fields stores none.
- **Values** are integers `0..=max`; laws and actors compute in `i32` and a store clamps.
- **Names** are global, like every name in a rule set. A field may share its name with a need:
  `sugar(t)` is the field at a target, bare `sugar` the actor's own need (Sugarscape's agents
  carry the sugar the cells grow).
- **Initial values** are a pure function of the seed and the cell (a number, another field,
  noise), because chunks are made lazily as the camera reaches them.
- **Drawn** by a declared colour ramp (`color "#e0c040"`: intensity by value), and `relief` for
  a height field: hillshade from the neighbours.
- **Scent becomes fields.** `mark` and `scent(CH)` are re-expressed as a field the built-in
  pack declares, with the decay law above; the 4-channel limit and the fixed fade go away.

**Reads by actors** (inside `sight`, as every search): `sugar(t)`; predicates on a field in
searches, `count (sugar >= 3) within 4`, `nearest (sugar >= 3) within 6 as s`; and the best
cell, `best sugar within 6 as s` (the largest value, ties broken by the same random ring start
`nearest` uses).

## 3. Laws

A law is a block on a field, run in every cell of a chunk at the field's cadence (`every N`,
a power of two of ticks, staggered by the chunk coordinate like scent today). It is written in
the rules language but compiled to a **column program**: each op runs over a whole chunk's
array, one dispatch per op per chunk instead of per cell (§8: 4 to 34 times cheaper than a
per-cell interpreter, as fast as native code once the law reads neighbours). So a law may use only what
runs over arrays:

- the cell's own fields (any field, not only its own), constants, `hour`, `day`, `light`;
- neighbour aggregates within 1: `sum`, `avg`, `min`, `max` of a field, `count (pred)`;
- arithmetic, comparisons, `min`, `max`, `clamp`, `abs`, `and`/`or`/`not` on conditions;
- `if`/`else` (compiled to a select: both arms computed, a mask picks);
- `rand(n)` and `chance(p)` per cell, counter-based like actors' dice
  (`hash(seed, stream, tick, x, y)`).

No actions, searches beyond 1, loops, or reads of actors: what needs more is an actor, and the
compiler says so. A law writes only its own cell; neighbours are read as they stood before the
law ran (**compute** every due chunk into a scratch, reading its own and its neighbours'
arrays; then **commit**), so the order of chunks never matters. A chunk where the field is all
0 and the law leaves 0 at 0 is skipped (scent's check today).

```text
field heat max 255 { every 16 { heat = avg heat within 1 } }                 # diffusion
field fire max 9 {
  every 4 {
    if fire > 0 { fire = fire - 1 }
    else if fuel > 0 and count (fire > 5) within 1 > 0 and chance(30) { fire = 9 }
  }
}
```

## 4. Writes from actors

Two kinds, because they are two things:

| | `add FIELD t n` (an effect) | `harvest FIELD t n` / `drop FIELD t n` (actions) |
|---|---|---|
| means | emit or erase: pheromone, footsteps, heat, a claim | move units between the cell and the actor's need of that name |
| conserved | no | yes: what the cell loses, the actor gains |
| target | `here` or adjacent | `here` or adjacent |
| settled | every delta to a cell summed in `i64`, clamped once: no order | key order when contested, like `take` and `give` |
| like today | `mark` (combines with the action, doesn't end the think) | `take`, `give` (one action a think; `BLOCKED` if nothing moved) |

**A write policy per field.** By default a field takes both. `conserved` allows only `harvest`
and `drop` (and its own law), so no rule can make or destroy what the field holds: ore, sugar,
money. `fixed` allows no writes from actors at all, only its law and its `init`: a height that
only `dig` and `build` change is not `fixed`, a daylight or a climate is. The compiler refuses
an `add` to a `conserved` field and any write to a `fixed` one, in any pack:

```text
field ore max 255 conserved { init noise scale 40 }   # mined, carried, dropped; never made
field fox_scent max 255 { every 16 { fox_scent -= (fox_scent + 31) / 32 } }   # add freely
field climate max 100 fixed { init noise scale 400 }
```

A simulation wants this as much as a game: it turns "nothing is created" from a convention the
author keeps into a property the compiler checks and `sim::invariants` tests.

The tick: actors think -> bites -> transfers (`take`, `give`, `harvest`, `drop`, key order) ->
deltas -> laws due this tick (compute, commit). Each a phase, chained. Cross-chunk writes use
the paths bites and transfers use across a chunk border today.

## 5. Terrain, declared and changeable

`Ground` and `Feature` stop being Rust enums. A pack declares materials; a cell keeps two slots
as today, a ground (under foot) and a feature (on it), each a byte index into the declared
materials:

```text
ground soil  { walkable  color "#5a4632" }
ground water { drink  color "#2b5d8a" }              # not walkable; `drink` takes from it
ground mud   { walkable  cost 2 }                    # a step takes two thinks
feature rock { blocks  opaque }
feature wall { blocks  opaque }
```

The built-in pack declares soil, water and rock with today's properties, so every world keeps
its behaviour and its saves. Actors change terrain with `dig t` (clears a feature, or lowers a
field named by the kind's `digs` line) and `build MATERIAL at t` (sets one), at a cost the
pack's rules pay from a need. Walls, dams, trenches, roads and nests come from agents, not
from the map. A scenario's `terrain` knobs become noise thresholds onto declared materials.

## 6. Height and what it does

Height is a field (`field height max 4000 { init noise scale 60 relief }`): mountains,
valleys and cliffs are shapes in it, `dig`/`build` move earth, and materials can follow from
it at creation (rock on steep slopes, snow above a line). A heightmap (2.5D) is where this
stops: true 3D multiplies every cell's cost; tunnels and bridges are layers (§7).

What height does is the pack's, through general senses and actions:

| | what | used for |
|---|---|---|
| `height(t)`, any field at a target | §2 | `best height within 8`: archers take the hill |
| `max FIELD along t`, `sum FIELD along t` | an aggregate over the cells on the line from here to t (integer line drawing, at most `sight` cells) | smoke or forest density in the way, a wall between |
| `sees(t) over FIELD [eye N]` | line of sight over a heightfield: no cell on the line rises above the straight line from the eye to t | hills hide armies, ridges are held |
| `reach N` on a kind | `hit` and `eat` on a target up to N cells away; the pack checks `sees(t)` and rolls its own dice; settled with bites, key order | archers, artillery, spells |
| `climb N on FIELD` on a kind | `move` refuses a step up more than N | cliffs, walls, passes |

Slower steps uphill, better hits downhill (`height(here) - height(t)` in the dice) and cover
behind a ridge stay in the pack's rules. **Water from height** is the next law past §3's
form: a conserved flow (each pair of neighbours exchanges `(level - level') / k`, computed the
same on both sides, so nothing is made or lost) needs a pairwise aggregate over the
neighbours, `flow`. Then lakes fill hollows and rivers run downhill, and water is where water
went, not a fixed ground type.

## 7. Layers

"Standing + cover" becomes declared layers; each kind says which it lives in, each layer what
it blocks. One `u32` array per declared layer per chunk:

```text
layer ground   # grass, moss: under whoever stands there (today's cover)
layer item     # what lies there: an egg, dropped food, a tool; `pick`, `put`
layer standing # today's occupants: one a cell, blocks
layer air      # bees and birds fly over the standing
layer below    # roots, burrows
```

## 8. Cost (measured)

`crates/sim-core/benches/field_law.rs`: three laws over 100 chunks (409 600 cells), one
thread, Apple Silicon 8c, load 3.3. `native` is a hand-written loop; `per cell` a checked
stack interpreter run once per cell, the way the VM runs a think; `per chunk` the same
bytecode with each op run over a chunk's array. All three produce the same cells (asserted).

| law | native | per cell | per chunk |
|---|---|---|---|
| regrowth `min(s + 1, cap)`, 6 ops | 14.8 µs | 4.18 ms | 124 µs |
| diffusion `avg h within 1`, 4 ops | 755 µs | 2.99 ms | 822 µs |
| Life (B3/S23), 10 ops | 1.32 ms | 6.55 ms | 1.23 ms |

Life today, one actor per cell (`packs/life`, `wmc run` 50 ticks of 640x640, `--profile
fast`): **53.7 ms/tick** on 1 thread, 15.1 ms on 8. So:

- per cell costs 4-280x native: dispatch per op per cell dominates small programs;
- per chunk is native speed once a law reads neighbours (the neighbour op is native code),
  and 8x native on pure arithmetic (each op writes a 16 KB column; fusing ops can close it);
- a Life law per chunk is ~44x cheaper than Life as actors.

Hence laws compile to column programs (§3). The neighbour sum in the bench clamps every read;
an interior fast path or separable sums will speed native and per chunk alike.

## 9. Showcases that test it

- **Sugarscape** (steps 1-2): a sugar field with regrowth to a per-cell cap, agents that
  `harvest`, metabolise, die and inherit; a skewed wealth distribution must emerge.
- **Fire in a forest** (step 2): fuel and fire fields, trees as actors that die where
  `fire(here) > 0`.
- **A battle** (step 4): teams by `look`, morale a need, orders relayed by `signal` from the
  nearest soldier who holds one (no global commander), archers on the high ground with
  `sees`, infantry with `climb 1`. A goal like "the side holding the hill wins 70% of seeds
  with 20% fewer troops" is a `wmc lab` goal.
- **Life as a field law**: the same board, ~44x faster, and the bench row of step 2.

## 10. Build order

Each step is general (any pack can use it), lands with its tests, fuzzers and docs (CLAUDE.md
workflow rules: `GRAMMAR.md` + `gen_rules.rs` Lively mode, store round trip + `store/fuzz.rs`,
`sim::invariants`, a scenario test, a doc example), and `make fuzz` before it closes.

1. **Fields**: declaration, storage, hash, store, ramp rendering; reads and field predicates in
   searches; `add`, `harvest`, `drop`; the write policy (`conserved`, `fixed`); scent
   re-expressed as fields. Invariant: a `conserved` field's total plus its needs' total
   changes only by its law.
2. **Laws**: the column compiler and interpreter, cadence, compute/commit phases, the neighbour
   halo, the edge rule (§11), the quiet-chunk skip. Life as a field law, with a PERF row.
3. **Declared terrain**: materials, `dig`, `build`, the built-in pack declaring today's three.
4. **Height**: `along`, `sees`, `reach`, `climb`, hillshade; then `flow` for water.
5. **Layers.**
6. **Arenas and players** (§12), with namespaces, when the game work begins; private fields
   last.

## 11. Unloaded chunks

Unloaded chunks **freeze** (decision 29), fields included: a frozen chunk is a paused piece of
world, so sugar and the agents that eat it pause together, and nothing catches up on load.
Scenarios, `lab`, `observe` and tests run the whole `size` with nothing streaming, so they
never depend on a camera; only play in an endless world does, as it already does for actors.
At the edge of what is loaded, a law reads an unloaded neighbour as **its own cell's value**
(a mirror: diffusion neither leaks nor gains there), or as the field's `outside V` if it
declares one. A scenario line to keep a region simulated in play waits for a world that needs
it.

## 12. Arenas and players

A programming game (players write creatures in the language, they meet in one world) needs to
know who may shape the world. A world's packs get a role:

- **The arena pack** (the game's designer) declares the fields, laws, materials, layers and
  the scenario: the shared world and its physics.
- **Player packs** declare kinds (and their subs, traits, consts) only. They read and write
  the arena's fields within its write policy, but can't declare a field, a law, a material or
  a layer, and can't redefine an arena name. The compiler refuses it, naming the line.

So a player's code can neither change the rules of the world nor create a resource. This
lands with **namespaces per pack** (§13): a player's names are its own (`red.ant`,
`blue.ant`), the arena's are shared, and two players may both have an `ant`. A scenario names
the arena and the players (`arena packs/ants`, `player red packs/red`); a plain world keeps
today's behaviour, every pack equal.

**Private fields.** A player pack may declare one kind of field after all: a `private` field,
an array **per player**, readable and writable only by that player's kinds. Ants that lay a
trail the other colony can't smell, a team's map of what it has explored. It costs a field's
memory per player who declares one, and a law on it is the arena's laws' form (§3), run per
player's array; an arena may cap how many a player declares (`private fields 2`).

```text
# packs/red/red.rules, a player pack in an arena
field trail max 255 private { every 16 { trail -= (trail + 31) / 32 } }
kind ant {
  ...
  when carrying > 0 => add trail here 40  move toward nest
  when best trail within 4 as t => move toward t
}
```

## 13. Open

- Negative field values (a sea floor below 0), or an offset in the pack?
- Laws reading actors (`count chicken within 1`: grass trampled where hens walk): the occupant
  array is a column too; add when a world needs it.
- Does `light` become a field (shade under trees), with the clock's daylight its default?
- A scenario's `terrain` and drawn `map` syntax for declared materials and fields.
- Namespaces per pack, before two packs of different authors share a world: needed by §12,
  and by any two packs that should not collide on a name.
- A player's budget in an arena (ops a think, rows, private fields), so one player's code
  can't slow the world for the others.
