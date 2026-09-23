# Architecture

Status: Stage (terrain grid) built; actors and systems next. This file records decisions that
are already made and the shape the design must fit into.

## Decisions

| # | Decision | Why | Revisit when |
|---|----------|-----|--------------|
| 1 | Rust owns the process; Zig is a static lib behind a C ABI | Rust's borrow checker enforces the "no shared mutation inside a phase" rule for free; Zig gives explicit SIMD and simple codegen for kernels | Never, unless Zig grows a safe threading story we want |
| 2 | Cargo is the single build driver; `build.rs` shells out to `zig build` | One command (`make`), one cache (`target/`), Zig opt mode follows cargo profile | If Zig side grows its own executables/tools |
| 3 | No threads in Zig; rayon in Rust | One scheduler, one mental model, deterministic chunking in one place | If a kernel needs intra-chunk parallelism (unlikely) |
| 4 | SoA world state in flat `Vec`s, `u32` entity ids | Parallelism and SIMD follow layout | N/A |
| 5 | Determinism across thread counts is a hard requirement | Reproducible bugs, replays, lockstep multiplayer option, testability | N/A |
| 6 | dev profile = opt-level 1 + Zig ReleaseSafe | opt-level 0 sim is unusable; ReleaseSafe keeps Zig bounds checks | If debug builds get too slow: `--profile fast` |
| 7 | `target-cpu=native` / `-Dcpu=native` | Sim runs where it's built, for now | When shipping binaries: switch to baseline + runtime dispatch |
| 8 | Git local only, commits on `main` | Solo, early | When a remote exists |
| 9 | Stage is a fixed `width x height` grid, row-major, one flat `Vec` per layer (`ground`, `feature`, `occupant`) | SoA: a system touches only the layers it needs; any layer is a byte slice for Zig | If the world must be unbounded/streamed: chunk-major tiles behind the same API |
| 10 | Parallel unit on the stage = a band of `CHUNK_ROWS` rows (`Stage::chunk_len()` cells) | Contiguous in memory so `par_chunks_mut` works directly; boundaries on row edges so 2D neighbour systems need a 1-row halo | If bench shows square tiles win on cache for neighbour-heavy systems |
| 11 | Two terrain layers: `Ground` (what a cell is: Soil/Water) and `Feature` (what rests on it: None/Rock) | Rock on soil and rock on water are the same rock; keeps enum products from exploding | If features need per-cell state beyond a tag: add a parallel `Vec` for that state |
| 12 | At most one actor per cell (`occupant: Vec<ActorId>`, `ActorId::NONE` = empty) | Movement/collision become a per-cell ownership question with no spatial index | If stacking is a game requirement: occupant becomes a head index into a per-actor linked list |
| 13 | Worldgen is a pure function of `(seed, x, y)` via `hash_cell` + value noise; no sequential state | Bit-identical for any thread count *and* any chunk size; the test recomputes every cell serially | If gen needs global passes (rivers, erosion): those become phases with their own determinism tests |
| 14 | Rendering lives in `app` (`render/ascii.rs`); `sim-core` has no glyphs | Layering; ASCII is a stand-in for a real backend | When a windowed renderer lands |

## Layers

```
app         window / input / render / config / logging   (later)
sim-core    World, phases, systems, scheduler, events
zig-kernels safe wrappers        <- only unsafe crate
zig/        pure compute kernels
```

Dependencies point downward only. `sim-core` never knows about rendering.

## Stage layout

```
Stage { width, height,
        ground:   Vec<Ground>   u8  per cell   Soil | Water
        feature:  Vec<Feature>  u8  per cell   None | Rock
        occupant: Vec<ActorId>  u32 per cell   NONE | dense actor id }
index(x, y) = y * width + x        chunk i = rows [i*CHUNK_ROWS, (i+1)*CHUNK_ROWS)
```

Walkability: `Ground::walkable && !Feature::blocks` (soil yes, water no, rock blocks). One place
to change. Per-cell scalars (moisture, heat, mana...) are added as further `Vec<T>` layers.

## Open questions (fill in as the design lands)

- Actor model: SoA arrays keyed by `ActorId` with a free list; what components?
- Tick model: fixed timestep? sub-stepping? interpolation for render?
- Movement/conflict resolution when two actors want one cell (per-chunk intents + in-order merge?)
- Rendering backend / windowing crate? (ASCII for now)
- Save/replay format?
