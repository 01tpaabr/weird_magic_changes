# Review of 2026-11-03: decisions for the owner

Twelve reviewers went over the language and the engine after the resting layers, groups
and big actors (decisions 71-73): parser, codegen, VM, the tick, big actors, groups and
layers, fields and laws, saves and streaming, scenarios, the CLI, determinism, and an
author writing hostile packs from the docs alone. Thread-count determinism held
everywhere (every scenario test and stress worlds up to 210 000 actors, at 1, 3, 8 and
64 threads). The clear bugs were fixed (see the commits after 11f4289). What follows
changes the language, the saves or the CLI, so it is yours. Each has a recommendation.

## Language

**D1. Conserved fields leak through needs of the same name.** A `conserved` field is
promised to only move (WORLD.md §4; a player "can't make a resource", §15). But
`harvest`/`drop` move it to and from the need of the same name, and that need is an
ordinary need: a newborn starts with it full, a rule can assign it (`ore = 9`), it
decays, and it vanishes with a death. Any pack can mint or destroy the resource,
including a player's (confirmed: `sum ore` 0 -> 8100 in 100 ticks). `packs/gather` and
`packs/colony` dodge it by emptying the need on the first think.
*Recommended:* a need named like a conserved field is the field carried: it starts at 0,
doesn't decay, and only `harvest`, `drop`, `take`/`give` and decreases write it (an
assignment that raises it is a compile error); on death what it holds is dropped on the
cell. Then `expect always sum F == V` holds in every pack.

**D2. Nothing can bite an actor in a declared layer.** `eat`/`hit` bite who stands at
the target (and, from a declared layer, the one standing under the biter), while every
sense finds actors in any layer. A hawk that finds a monkey in the canopy and eats it
bites the tree under it (confirmed: trees died, monkeys fell). The lint even says
"`hawk` eats `monkey`".
*Recommended:* a bite reaches the target cell in the biter's own layer first, then the
standing one (`eat t` = the first with `health` from the biter's layer down). Needs a
note in RULES §8 either way.

**D3. A big actor senses and reaches from its top-left cell.** `nearest`, `reach`,
`within r` all measure from the anchor, so a 4 x 4 dragon with `reach 2` never bites a
herd touching its right edge. Documented now; is it what you want?
*Recommended:* keep it (cheap, predictable) and document it, which is done. The
alternative, distances from the nearest cell of the footprint, costs every search of a
big kind.

**D4. Searches run out of fuel long before their documented limits.** A state costs
about 19 fuel at the simplest, so at the most fuel (32768) about 1700 states expand,
not the documented 4096; a trapped search leaves memory half-written.
*Recommended:* charge a search's states from a budget of their own (`limit` states,
each one op), or document "at most about fuel / 20 states". Also make a `limit` outside
0..=4096 a compile error when it is a constant (now silently clamped).

**D5. `MAX_LAYERS` went from 8 to 6.** Done, as the fix of a crash: with declared layers
a chunk can hold (2 + layers) x 4096 rows, which overflowed the row checks (a dev panic,
an unreadable chunk in release), and past 32768 rows a row id collides with the big
actors' part marker. 6 keeps every row id below it. If you want 8 back, parts need
another encoding (a bit of the kind instead, which costs a mask in every scan).

**D6. A need or mem named like a constant.** It now is a compile error (like a trait
parameter that hides one), because `rates` and `lab` read the constant while the VM
reads the mem. Say if you'd rather have a warning.

**D7. The rules stack is shared across calls.** Each rule and sub is checked against
the 64-value stack alone; together, deep expressions around calls can trap at run time
(documented in §18). A static check of call-site depth plus the callee's peak would
catch it at compile time. Worth it?

**D8. A resting layer's tag bit is fixed by its place.** The support bit of a layer that
rests on a kind is `63 - (place - 2)`, so a pack whose only resting layer is its sixth gives
up six tags, not one (the compiler now reserves every bit up to the last resting layer).
*Recommended:* number support bits by rank among the resting layers, so each costs one tag;
a small VM change, no save change.

## Engine

**E1. Big actors move one at a time.** Every big move or birth is settled in Migrate on
one thread, even inside a chunk: a world of 12 800 actors of 2 x 2 runs at 28.6 ms a
tick on 8 threads, against 4.7 ms for 16 000 one-cell actors.
*Recommended:* settle a big move whose old and new footprints stay in one chunk in
Apply, in parallel, and send only the ones crossing a border to Migrate; add a bench
with big actors.

**E2. A crash after streaming can duplicate actors.** Unloading a chunk writes its
file, stamped now, but not the world file; a crash before the next save reopens the
world at the old tick with some chunk files newer than their neighbours, so an actor
that crossed into a written chunk exists twice (same uid). A clean quit saves and is
fine.
*Recommended:* stage unloaded chunks in a generation directory that `save` commits
(write world file, then swap), or write the world file with every unload.

**E3. Under a pending remap one bad far chunk file makes the world unsaveable.** Opening
a save with other rules migrates every chunk file at the first save, and one corrupt
file refuses all saves.
*Recommended:* leave files listed as unreadable out of the migration.

**E4. `kind:look` predicates never match a big actor's part cells** (only its anchor),
while kind and tag predicates match every cell. Documented; making the per-cell test
follow a part to its anchor costs every scan.

**E5. Several actors on one cell of one layer.** Still one a layer by design (decision
71); a group stands for many. No change proposed.

## CLI and scenarios

**C1. `wmc lab` exits 0 when no variant passes.** As a CI gate it should exit 1.
*Recommended:* exit 1 when nothing passes.

**C2. `wmc run DIR N` never writes the save**, and a second run on the same directory
quietly builds a new world from the default scenario. *Recommended:* `run` saves at the
end (and `--no-save` for a dry run), so `run` twice equals `run` once with twice the
ticks.

**C3. A cadence has no upper limit.** `cadence 1073741824` compiles; `why` now gives up
after a game day of waiting. *Recommended:* cap cadence at 2^20 ticks (about 30 days)
and make it a compile error past that.

**C4. Field metrics in scenarios count cells outside `size`.** `expect sum water` folds
every loaded chunk, so a 100 x 70 world counts 128 x 128 cells. *Recommended:* clip to
the scenario's size, which is what "every cell of the world" reads as.

**C5. `expect never max X of K > 0` passes when none of K is alive** (a `max` of nothing
is no number, and `never` counts it as false). A test that kills its subject passes by
accident. *Recommended:* an `always`/`never` over a metric with no number fails with
"none alive", unless written `... or none`.
