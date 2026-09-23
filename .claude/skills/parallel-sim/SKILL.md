---
name: parallel-sim
description: Design patterns for massively parallel, deterministic simulation systems in Rust (rayon) with Zig kernels — phases, chunking, double buffering, deterministic reductions, spatial partitioning, avoiding false sharing, job graphs, and how to test/profile them. Invoke before designing or rewriting any system, tick loop, or scheduler.
---

# parallel-sim

How to structure sim work so it scales across cores *and* stays deterministic.
Code snippets: `references/patterns.md`. Coding-level rules: `/zig-rust-dev`.

## The mental model

A tick is a DAG of **phases**. Inside a phase, all work is embarrassingly parallel over
**chunks** and touches no shared mutable state. Between phases there is a barrier. That's
it. If a design needs anything else (locks mid-phase, "just one atomic", a work queue
with mutation) go back and split the phase.

```
tick:
  phase A: par over chunks of entities   -> writes only its own chunk of `next`
  barrier
  phase B: par over chunks of cells      -> reads `next`, writes cell buffers
  barrier
  phase C: sequential merge / swap buffers / emit events (cheap, O(chunks))
```

## Rules

1. **Read-only shared, write-only owned.** Every phase declares what it reads (shared,
   immutable during the phase) and what it writes (partitioned so each chunk owns a
   disjoint slice). Rust's borrow checker enforces this if you pass `&[T]` and
   `par_chunks_mut`; if you find yourself reaching for `unsafe` or `UnsafeCell` to share a
   `&mut`, the partition is wrong.
2. **Double-buffer cross-entity effects.** Anything that reads other entities' state
   reads `prev`, writes `next`. Swap at the barrier. Costs memory, buys freedom from
   ordering bugs entirely.
3. **Per-thread accumulate, deterministic merge.** Effects that many entities apply to
   one target (damage, forces, counts): each chunk writes to its own scratch buffer or
   event list; a sequential phase merges them **in chunk order**. Never atomics-add floats.
   Integer atomics are OK only for totals where order is irrelevant.
4. **Chunks are fixed and index-based, not thread-based.** Chunk `i` covers entities
   `[i*CHUNK, (i+1)*CHUNK)`. Results depend on chunk boundaries, never on which thread ran
   them or how many threads exist. This is what makes 1-thread == 64-thread bit-identical.
5. **Seeds are derived, never shared.** `rng_for(seed, tick, chunk_or_entity_id)` via a
   hash (e.g. splitmix64) -> `Xoshiro256PlusPlus::seed_from_u64`. Global RNG is banned.
6. **No allocation inside a tick.** Scratch buffers live on the `World`/`Scratch` struct
   and are `clear()`ed, not dropped. Event lists are `Vec<Vec<Event>>` indexed by chunk.
7. **Spatial queries use a rebuilt-per-tick grid** (uniform grid / sort-by-cell), not a
   mutable tree. Build = counting sort (parallelizable, deterministic), query = read-only.
8. **Chunk size is measured.** Start at 1–4k elements (fits L1/L2 with a few arrays),
   tune with `make bench`. Too small: rayon overhead. Too big: idle cores at the tail.
9. **False sharing:** never let two chunks write adjacent bytes. Chunk-aligned writes
   into distinct slices are safe; per-thread counters need `#[repr(align(64))]` padding.
10. **Zig kernels take a chunk.** Rust slices the chunk, Zig vectorizes inside it. Do not
    thread inside Zig.

## Choosing the tool

| Need                                    | Use                                              |
|-----------------------------------------|--------------------------------------------------|
| same op over N independent elements     | `rayon` `par_chunks_mut` (+ Zig kernel inside)   |
| two/three independent heavy tasks       | `rayon::join` / `rayon::scope`                    |
| pipeline across frames (sim -> render)  | `crossbeam::channel` bounded, or triple buffer    |
| long-lived background worker            | `std::thread::scope` + channel                    |
| per-frame counters/stats                | per-chunk `u64` then sum; `AtomicU64` if lazy     |
| shared config read by all               | `&Config` (immutable) or `arc_swap`               |
| a lock                                  | reconsider; if truly needed, `parking_lot`, at frame boundary only |

## Testing parallel code

- Every system: `run_with_threads(1) == run_with_threads(N)` on checksums after K ticks
  (`rayon::ThreadPoolBuilder::new().num_threads(n).build().unwrap().install(..)`).
- Replay test: record inputs per tick, replay, expect identical checksum.
- Property test (`proptest`) for kernels: random lengths including 0, 1, and non-multiples
  of the SIMD width.
- Run tests under `cargo test --release` occasionally; some races only show up fast.
- If a determinism test flakes, it's a real bug. Don't retry it; bisect the phase.

## Profiling

- `make bench` (criterion) for micro; `docs/PERF.md` for baselines.
- Whole-app: `samply record target/release/wmc 1000000 600` (install: `cargo install
  samply`) or Instruments on macOS. Look for: time in rayon scheduling (chunks too small),
  one hot chunk (imbalance), memory-bound loops (need SoA / narrower types), `memcpy`
  (accidental clones).
- Count, don't guess: entities/sec/core is the number to report. Note thread count and
  `n` next to every number.
