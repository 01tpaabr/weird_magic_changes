---
name: bevy-dev
description: Bevy 0.19 practices for this massively parallel, deterministic sim. Loaded automatically at session start. Re-invoke after context compaction or before writing any system, schedule, plugin, render or task-pool code. Verified API cheatsheets live in references/.
---

# bevy-dev

Bevy is the engine: its ECS holds the world, its schedule is the tick, its task pool is
the parallelism, its renderer draws the grid. Read `CLAUDE.md` priorities first; this
skill is the how. Exact signatures (verified against the 0.19.1 sources, not memory):
`references/ecs-tasks-app.md` (ECS, schedules, tasks, App, messages, migration renames)
and `references/render-window-input.md` (TilemapChunk, images, camera, window, input,
text). For parallel *design* patterns invoke `/parallel-sim`.

**Your training data is mostly Bevy 0.14–0.16. 0.17–0.19 renamed a lot** (`Event`s split
into `Message`s, `Trigger`→`On`, `ExecutorKind` removed, `Resource: Component`, camera
types moved to `bevy::camera`, `Anchor` is a component, `WindowResolution::new(u32,u32)`,
`Assets::get_mut` returns `AssetMut`). When unsure, grep the checked-out source:
`~/.cargo/registry/src/*/bevy_ecs-0.19.1/src` (etc.), or read the references. Never
write a Bevy call from memory if you can check it in ten seconds.

## Where things live

| `sim-core` (bevy_ecs + bevy_tasks only)              | `app` (the `bevy` umbrella)                          |
|------------------------------------------------------|------------------------------------------------------|
| `stage`: chunk entities (`ChunkCoord`, `ChunkCells`, `ChunkMeta`), `Stage` directory resource, `StageCells` read param, insert/remove/checksum | `play`: `PlayPlugin`, `run()`, resources (`Layout`, `Zoom`, `Frames`, `Grids`), the `Update` chain |
| `sim`: `SimConfig`, `Tick`, `SimTick` schedule + `Phase` sets, `install/create/open/save/ensure_loaded/step/checksum` | `render::cells` phase 1 (chunks -> `CellFrame`), `render::grid` phase 2 (`CellFrame` -> tile data), `render::atlas` tileset, `render::palette` |
| `par`: `init_task_pool` (honours `WMC_THREADS`), `par_map`, `par_for` | `camera` (`ViewCamera`), `clock` (`SimClock`): wall-clock lives here only |
| `store`, `rng`, `time`: unchanged plain Rust          | `main.rs`: `show`/`run` are headless (bare `World`), `play` is the `App` |

No `App`, `Time`, asset or window type may appear in `sim-core`. No `unsafe` anywhere.

## The model

- **A chunk is an entity** with `ChunkCoord` (immutable), `ChunkCells` (flat SoA arrays, 24 KiB)
  and `ChunkMeta`. Bevy stores each component type in a dense table column, so
  `Query<&mut ChunkCells>` iterates a `Vec<ChunkCells>`: the slab, for free.
- **`Stage` is the directory**, not the data: `HashMap<ChunkCoord, Entity>` for lookup plus
  `active: Vec<(ChunkCoord, Entity)>` sorted by coordinate. `active()` is the **only**
  iteration order allowed to affect results. Entity ids and table order depend on load
  history: never sort by `Entity`, never rely on query iteration order for anything observable.
- **A tick is `world.run_schedule(SimTick)`.** `Phase::Simulate` then `Phase::Advance`,
  chained. The schedule is built with `ambiguity_detection: LogLevel::Error`: two systems
  with overlapping access and no explicit order refuse to build (tested in `sim.rs`). Order
  them or split the data. The multi-threaded executor runs disjoint systems concurrently;
  with every conflict ordered, its output equals the single-threaded one by construction.
- **Speed is not the engine's.** `SimClock` (app) decides how many `sim::step`s a frame
  runs (budget, drop debt, max speed, step-and-pause). Bevy's `FixedUpdate`/`Time<Fixed>`
  is *not* used for the sim: it cannot drop debt or cap a frame. `Time` is read in `app`
  for the camera glide and the clock, nowhere else.

## Writing a sim system

```rust
// sim-core/src/systems/moss.rs
pub const STREAM_MOSS: u64 = 0x0003;          // new rng stream, never reuse a number

fn grow_moss(tick: Res<Tick>, cfg: Res<SimConfig>,
             mut chunks: Query<(&ChunkCoord, &mut ChunkCells, &mut ChunkMeta)>) {
    chunks.par_iter_mut().for_each(|(coord, mut cells, mut meta)| {
        if !cadence::due(tick.0, 4, *coord) { return; }      // every 4 ticks, staggered by coord
        let mut rng = rng_for(cfg.seed, tick.0, coord_id(*coord));
        for i in 0..CHUNK_CELLS { /* read+write this chunk only */ }
        meta.dirty = true;
    });
}
// in sim::install: schedule.add_systems(grow_moss.in_set(Phase::Simulate));
```

Rules that keep it deterministic and fast:

1. **`par_iter_mut` writes only the entity it is handed.** Reading *other* chunks from inside
   the closure needs a second, read-only query over a *different* component (double buffer:
   `Query<&CellsPrev>` + `Query<&mut ChunkCells>`), or the neighbour data copied into a halo
   in an earlier phase. Two queries on the same component, one mutable, will not compile.
2. **Nothing accumulates across chunks in the parallel closure.** Per-chunk output goes in
   the chunk (a component field or a `Vec` per chunk); a sequential system in the next
   phase merges by walking `stage.active()`. `bevy::utils::Parallel<T>` drains in thread
   order: acceptable only if you `sort_unstable_by_key` on a stable key before merging.
3. **RNG is `rng_for(seed, tick, id)` / `hash_cell`.** `id` is a hash of the chunk coord or
   the actor id, never an `Entity`, never a slot.
4. **Batching:** `par_iter_mut()` batches by entity count / thread count; results never
   depend on the batch size because of rule 1. Tune `.batching_strategy(BatchingStrategy::fixed(n))`
   only with a `make bench` number.
5. **No `Commands` spawn/despawn in a phase.** Chunk entities come and go only in
   `ensure_loaded` (exclusive, between ticks). Actors are `u32` ids inside chunk data.
6. **Every system gets tests**: a unit test of the rule on one `ChunkCells`, and the
   `wmc run` checksum (the integration test in `crates/app/tests/determinism.rs` runs the
   binary with `WMC_THREADS=1,3,8`). Bevy's compute pool is one per process, so one-thread
   vs many-thread comparisons are cross-process by design. Update the `step` bench baseline
   in `docs/PERF.md`.

Read-only convenience inside systems: `StageCells` (`SystemParam`: `Stage` + `Query<&ChunkCells>`)
gives `chunk(coord)`, `get(pos)`, `walkable`, `free`. Hot loops iterate `Query<&ChunkCells>`.
From `&mut World` (streaming, saves, tests): `stage::chunk`, `stage::chunk_mut` (marks dirty),
`stage::insert`, `stage::remove`, `stage::checksum`.

## Parallel work outside queries

`ComputeTaskPool::get().scope(|s| { for x in items { s.spawn(async move { f(x) }) } })`
returns `Vec<T>` **in spawn order** (verified: FIFO queue) when spawned from the scope
closure. `par::par_map(items, batch, f)` and `par::par_for(n, batch, f)` wrap this; disjoint
`&mut` slices (frame rows, buffer bands) are split *before* the scope and moved into tasks
(`render::cells`). `par_iter` and `scope` panic if the pool was never initialised: call
`sim_core::par::init_task_pool()` (tests, headless) or let `TaskPoolPlugin` do it (App).
`WMC_THREADS=n` sizes both paths.

## Adding a per-cell layer

Field in `ChunkCells` -> fold into `ChunkCells::hash` -> encode/decode in `store` and bump
`FORMAT_VERSION` -> `worldgen` fills it -> `render::palette::style` shows it. Same commit.

## The frame (app)

`Update` runs one chain: `handle_input -> advance_camera -> layout -> stream_and_tick
(exclusive) -> render_frame -> upload_tiles`, ordered `.before(update_tilemap_chunk_indices)`
so Bevy repacks our tiles in the same frame. Patterns used, copy them:

- **Exclusive system for anything that needs `&mut World`** (streaming, `run_schedule`,
  save): `fn f(world: &mut World)`. Pull other resources with `world.resource_scope(|world,
  store: Mut<Store>| ..)`. Everything else is an ordinary system with `Res`/`ResMut`/`Query`.
- **One-shot requests** from input are a `Pending { save, step, quit }` resource, consumed
  with `std::mem::take` in the exclusive system. Quitting writes `AppExit::Success` through
  `world.write_message`; the window's close button is ours (`close_when_requested: false`,
  read `MessageReader<WindowCloseRequested>`) so the world is saved first.
- **Rendering is `TilemapChunk`** (`bevy::sprite_render`), two per layer: bg = the solid
  tileset box tinted per cell, fg = the glyph box tinted, `AlphaMode2d::Blend`, fg at
  `z + 0.5`. Facts that bite: the component is **immutable** (re-insert to resize, together
  with a `TilemapChunkTileData` of the right length or the insert hook bails);
  tile row 0 is the **bottom** row; the shader multiplies the texel by the tint's raw 8-bit
  channels **without** sRGB decoding, so `palette::Color::tint` pre-linearises; a tile of
  `None` is discarded (use it for blank glyphs). Mutating `TilemapChunkTileData` re-uploads.
- **Coordinates:** camera at the origin, `Projection::Orthographic.scale = window.scale_factor()`
  so one world unit is one *physical* pixel; `Layer::place` converts a window-pixel rect
  (y down) to the chunk centre (y up). The tileset is rasterised at the physical cell size
  and displayed at that size, so glyphs are 1:1 on Retina.
- **Assets from bytes:** `Image::new(Extent3d { depth_or_array_layers: N }, D2, bytes,
  Rgba8UnormSrgb, RenderAssetUsages::RENDER_WORLD)` then `images.add(img)`. No `assets/` dir.

## Testing with Bevy

- Unit tests of systems: `sim::new_world(&cfg)` (pool + install + create), then
  `world.run_system_once(|s: StageCells| ..)` (`use bevy_ecs::system::RunSystemOnce`)
  or `world.query::<..>().iter(&world)`. A schedule is exercised with `sim::step(&mut world)`.
- Ambiguity: a test builds a schedule with two conflicting unordered systems and expects
  the panic; keep it when you touch `install`.
- Nothing in `sim-core` tests needs an `App`; nothing in `app` tests opens a window.

## Build & run

- `make` / `make run` / `make test` pass `--features app/dev` = `bevy/dynamic_linking`. The
  binary then only runs through `cargo run` (or `make run`): `target/debug/wmc` alone dies
  with `Library not loaded: @rpath/libstd`. That is expected; `make release` links statically.
- First build ~5 min; an app-crate change relinks in seconds. `[profile.dev.package."*"]
  opt-level = 3` is what makes debug builds usable.
- Features are trimmed (`Cargo.toml`): 2D sprite rendering, winit, log, assets; no audio,
  UI, 3D, gltf, picking, scenes. Add a feature in one place when a plugin is missing.
- Clippy: `needless_pass_by_value`, `too_many_arguments`, `type_complexity` are allowed
  workspace-wide because Bevy systems trip them by design. Everything else is `-D warnings`.
- Logs: `LogPlugin { filter: "warn,app=info" }`; use `info!/warn!/error!` from the prelude.

## 0.19 gotchas, short list

`Res<Time>` is virtual time (pausable); `Query::single()` returns `Result`; `AppExit` is a
`Message`; `Resource: Component` so `Query<Entity>` also sees resource entities (filter
`Without<IsResource>` if it matters); `Schedule::set_executor(SingleThreadedExecutor::new())`
for a serial run; `HashMap` is `bevy::platform::collections::HashMap` (std's is fine for
lookup-only maps); `Parallel<T>` is in `bevy::utils`; `Entity::PLACEHOLDER` exists,
`Entity::from_raw` does not; `despawn()` is recursive. Full list: `references/ecs-tasks-app.md` §9.

## Definition of done

`make ci` green (fmt, clippy -D warnings, all tests including the cross-process
determinism gate). For anything touching a hot path: a criterion before/after number in
the commit message and `docs/PERF.md` updated. For any new system: its own unit test and an
unchanged-or-explained `wmc run` checksum story.
