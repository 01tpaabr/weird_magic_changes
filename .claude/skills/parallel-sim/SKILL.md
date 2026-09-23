---
name: parallel-sim
description: Design patterns for massively parallel, deterministic simulation systems in Rust on Bevy (schedules, par_iter, task pool) — phases, chunking, double buffering, deterministic reductions, spatial partitioning, avoiding false sharing, job graphs, and how to test/profile them. Invoke before designing or rewriting any system, tick loop, or scheduler.
---

# parallel-sim

How to structure sim work so it scales across cores *and* stays deterministic.
Code snippets: `references/patterns.md`. Bevy-level how-to (schedules, `par_iter_mut`,
task pool, rendering): `/bevy-dev`.

## The mental model

A tick is a DAG of **phases**. Inside a phase, all work is embarrassingly parallel over
**chunks** and touches no shared mutable state. Between phases there is a barrier. That's
it. If a design needs anything else (locks mid-phase, "just one atomic", a work queue
with mutation) go back and split the phase.

```
tick (= the SimTick schedule):
  Phase A: system(s) with Query::par_iter_mut over chunk entities -> each writes its own chunk's `Next`
  barrier (the next Phase set)
  Phase B: par over chunks -> reads `Prev` of any chunk (read-only query), writes own cell buffers
  barrier
  Phase C: sequential merge in `stage.active()` order / swap buffers / emit messages (cheap, O(chunks))
```

In Bevy terms: a phase is a `SystemSet`, the sets are `.chain()`ed, and the schedule is
built with `ambiguity_detection = Error` so two systems in one phase that touch the same
data refuse to build until you order them or split the data.

## Rules

1. **Read-only shared, write-only owned.** Every phase declares what it reads (shared,
   immutable during the phase) and what it writes (partitioned so each chunk owns a
   disjoint slice). Bevy enforces this: a system's queries declare access, `par_iter_mut`
   hands each task whole entities, and two systems that conflict must be ordered. If you
   find yourself reaching for `unsafe`, `UnsafeCell` or a `Mutex` to share a `&mut`, the
   partition is wrong.
2. **Double-buffer cross-entity effects.** Anything that reads other chunks' state reads
   the `Prev` component (a second read-only query), writes `Next`. Swap at the barrier
   (a sequential system, or `std::mem::swap` per chunk in parallel). Costs memory, buys
   freedom from ordering bugs entirely.
3. **Per-chunk accumulate, deterministic merge.** Effects that many entities apply to
   one target (damage, forces, counts): each chunk writes to its own scratch component or
   list; a sequential system in the next phase merges them **in `stage.active()` order**.
   Never atomics-add floats. Integer atomics are OK only for totals where order is
   irrelevant. `bevy::utils::Parallel<T>` drains in thread order: sort by a stable key first.
4. **Chunks are fixed and coordinate-based, not thread-based.** A chunk is a `ChunkCoord`;
   results depend on chunk boundaries, never on which thread ran them, how `par_iter_mut`
   batched them, or the entity id. This is what makes 1-thread == 64-thread bit-identical.
5. **Seeds are derived, never shared.** `rng_for(seed, tick, chunk_or_actor_id)` via a
   hash (splitmix64) -> `Xoshiro256PlusPlus::seed_from_u64`. Global RNG is banned; so is
   seeding from an `Entity`.
6. **No allocation inside a tick.** Scratch buffers live in components/resources and are
   `clear()`ed, not dropped. Per-chunk event lists are a `Vec` component on the chunk. No
   `Commands` spawn/despawn in a phase: chunk entities change only in streaming.
7. **Spatial queries use a rebuilt-per-tick grid** (uniform grid / sort-by-cell), not a
   mutable tree. Build = counting sort (parallelizable, deterministic), query = read-only.
8. **Chunk size is measured.** 64x64 cells today; tune with `make bench`. Too small: task
   overhead. Too big: idle cores at the tail. `BatchingStrategy::fixed(n)` only with a number.
9. **False sharing:** never let two tasks write adjacent bytes. Whole-component writes per
   entity are safe; per-thread counters need `#[repr(align(64))]` padding.
10. **Bevy's executor is the outer scheduler.** Systems in one phase with disjoint access
    run concurrently; inside a system, `par_iter_mut` is the inner loop. Never spawn your
    own threads, never a second pool.

## Choosing the tool

| Need                                    | Use                                              |
|-----------------------------------------|--------------------------------------------------|
| same op over every chunk                | one system, `Query<..>::par_iter_mut().for_each` |
| same op over an arbitrary slice         | `par::par_map` / `par::par_for` (task-pool scope, results in input order) |
| two/three independent heavy systems     | put them in the same phase with disjoint access; Bevy runs them concurrently |
| pipeline across frames (sim -> render)  | Bevy's pipelined rendering already does it; sim state is read in `Update` |
| long-lived background work (IO, saves)  | `IoTaskPool::get().spawn(..)` + a `Task<T>` polled in a system |
| per-frame counters/stats                | per-chunk `u64` then sum in `active()` order; `AtomicU64` if lazy |
| shared config read by all               | `Res<Config>` (immutable during the phase)        |
| a lock                                  | reconsider; if truly needed, `parking_lot`, at frame boundary only |

## Testing parallel code

- Every system: same checksum after K ticks with `WMC_THREADS=1` and `=8`. Bevy's compute
  pool is one per process, so this is cross-process: `crates/app/tests/determinism.rs`
  runs the `wmc run` binary; in-process unit tests compare two fresh worlds.
- Replay test: record inputs per tick, replay, expect identical checksum.
- Property test (`proptest`) for per-chunk rules: random cell contents, lengths 0/1/odd.
- Run tests under `cargo test --release` occasionally; some races only show up fast.
- If a determinism test flakes, it's a real bug. Don't retry it; bisect the phase.
- A schedule that fails to build with "ambiguity" is the executor telling you two systems
  share data without an order. Order them; do not lower the log level.

## Profiling

- `make bench` (criterion) for micro; `docs/PERF.md` for baselines.
- Whole-app: `samply record target/release/wmc run saves/dev 10000` (install: `cargo install
  samply`) or Instruments on macOS; Bevy's `trace_tracy` feature + Tracy shows every system
  and span per frame. Look for: time in task scheduling (batches too small), one hot chunk
  (imbalance), memory-bound loops (need SoA / narrower types), `memcpy` (accidental clones,
  archetype moves from inserting/removing components on chunk entities).
- Count, don't guess: entities/sec/core is the number to report. Note thread count and
  `n` next to every number.
