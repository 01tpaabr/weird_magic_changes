# Architecture

Status: Stage (chunked, unbounded terrain grid), streaming, persistence and a windowed
ASCII renderer with a WASD camera are built; actors and systems next. This file records decisions that
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
| 9 | Stage is an **unbounded** grid stored as 64x64 **chunks**; each chunk holds one contiguous array per layer (`ChunkCells`); cell coords are `i32` | The initial map has a known size but the world must grow in any direction and stream; a chunk is at once the parallel unit, the streaming unit and the save unit. Replaced the flat row-major `Vec` + row-band design of the first Stage commit | If a system needs finer parallel granularity than a chunk: split inside the chunk by rows, never re-layout |
| 10 | Loaded chunks live in a slab (`Vec<ChunkCells>` + `Vec<ChunkMeta>`) with a free list; `HashMap` for lookup only; `active` = slots sorted by coord is the **only** iteration order allowed to affect results | Slot numbers depend on load history; sorting by coordinate makes every merge and the checksum independent of it. Meta is a separate slab so a double-buffered phase can read `cells` while writing `cells_next` | If the hash lookup shows up in profiles: swap for a 2-level array keyed by chunk coord |
| 11 | Two terrain layers: `Ground` (what a cell is: Soil/Water) and `Feature` (what rests on it: None/Rock) | Rock on soil and rock on water are the same rock; keeps enum products from exploding | If features need per-cell state beyond a tag: add a parallel `Vec` for that state |
| 12 | At most one actor per cell (`occupant: Vec<ActorId>`, `ActorId::NONE` = empty) | Movement/collision become a per-cell ownership question with no spatial index | If stacking is a game requirement: occupant becomes a head index into a per-actor linked list |
| 13 | Worldgen is a pure function of `(seed, x, y)` via `hash_cell` + value noise; no sequential state | Bit-identical for any thread count *and* any chunk size; the test recomputes every cell serially | If gen needs global passes (rivers, erosion): those become phases with their own determinism tests |
| 14 | Rendering lives in `app` (`render/`); `sim-core` has no glyphs or colours. `palette.rs` is the one place a tile becomes (glyph, fg, bg): the ground picks the cell background, the thing on it picks glyph + foreground | Layering; a system never needs to know how it looks | If tiles get per-cell scalar visuals (moisture tint): palette takes the scalar layers as input |
| 15 | Streaming: `World::ensure_loaded(focus, LoadPolicy)` loads chunks within `load` chunks of the focus and unloads beyond `unload` (`unload > load` = hysteresis). Only loaded chunks simulate | Loaded set is a pure function of inputs, so replays stay bit-identical; the camera is just one source of focus | When actors wander off-screen: add per-actor focus points (or a "simulation bubble") to the same call |
| 16 | Persistence = a directory: `world.wmc` meta + `chunks/<x>_<y>.wmcc`, raw little-endian layer dumps, atomic rename on write. **Only dirty chunks are written**; clean ones are regenerated from the seed | Zero-copy format (memcpy of `ChunkCells`), saves of an unexplored world are bytes not megabytes. `FORMAT_VERSION` + `CHUNK_BITS` fingerprint refuse mismatched files | When a save has thousands of chunk files: region files (32x32 chunks per file with an offset table) |
| 17 | Camera is app state, saved in `camera.txt` beside the world, not inside the sim | The sim must not know where a player is looking; several viewers must be possible | Never |
| 18 | A frame is two pure phases: `cells::render_cells` (Stage viewport -> `CellFrame`, three SoA `Vec`s glyph/fg/bg, **row-parallel**, one chunk lookup per row-chunk span) then `blit::blit` (`CellFrame` + glyph atlas -> RGBA8, **band-parallel** over `BAND_ROWS` cell rows, Zig kernel `wmc_blit_cells` per band). No reduction anywhere, so 1 thread == N threads bit-for-bit (tested) | Same shape as a sim tick (read shared, write owned slices); the Zig kernel is a pure function over caller-owned buffers, the Rust `blit_reference` is its oracle and bench baseline | If the blit is measured hot at 4K: move the blend to a wgpu shader with the same `CellFrame` as input; the cell phase stays |
| 19 | Window = `winit` + `softbuffer` (presents a CPU `u32` framebuffer, `0x00RRGGBB`) + `fontdue`. The TUI and `crossterm` are gone; `wmc show` prints text through the same cell phase. Pixel colours are packed for softbuffer in `palette::Color`; the Zig blit treats the four bytes as opaque channels | A CPU framebuffer keeps the whole picture a flat buffer we own and can hand to Zig/rayon, with no GPU stack in the build. `pixels` 0.17 (wgpu 29) was tried first and presents nothing on macOS (blank window, `render()` returns Ok; reproduced with a 40-line program), so it was dropped | When phase 2 moves to a shader (see 18): that is a wgpu surface replacing the softbuffer one, `CellFrame` unchanged |
| 20 | Glyph atlas: bundled JetBrains Mono NL (OFL, `crates/app/assets/`) rasterized once per **physical** cell size into square `cell x cell` coverage boxes for printable ASCII; glyph centred in the square. Cell = logical 16 px x window scale factor; `+`/`-` zoom rebuilds the atlas. Event-driven loop (`ControlFlow::Wait`): redraw on input/resize, and while the camera moves each frame requests the next (AppKit paces it to the display) | Square cells for a top-down grid; per-physical-pixel atlas means crisp text on Retina; bundling makes the picture identical on every machine | When the sim runs continuously: fixed-timestep loop with the same frame pipeline; when Unicode glyphs are wanted: atlas becomes a map from char to box |
| 21 | Camera is a fractional cell position with velocity (`camera.rs`): held keys set a direction, diagonals are normalised, speed eases in/out with a 70 ms time constant, wall-clock `dt` clamped to 100 ms. The blit takes a signed pixel origin and clips (`Target`), so the map grid is drawn one cell larger than the window and shifted by the sub-cell remainder; status rows are a second, unshifted blit at the bottom | Smooth scrolling without touching the cell phase or the sim: the camera is app state and its clock never reaches `World`. Clipping in the kernel beats a scratch buffer + copy (would double the memory traffic of the hottest loop) | When actors are followed by the camera: the target becomes an actor position, same easing |

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
world cell (x, y): i32     chunk = (x >> 6, y >> 6)      local = (y & 63) * 64 + (x & 63)

Stage { cells:  Vec<ChunkCells>   slot -> { ground: [Ground; 4096]   u8   Soil | Water
                                            feature: [Feature; 4096] u8   None | Rock
                                            occupant: [ActorId; 4096] u32  NONE | actor }
        meta:   Vec<ChunkMeta>    slot -> { coord, loaded, dirty }
        index:  HashMap<ChunkCoord, slot>   lookup only
        active: Vec<slot>                   sorted by (y, x): THE iteration order }
```

A phase is `stage.cells.par_iter_mut()` (or `cells_mut_at(slots)` for a subset). Cross-chunk
reads come later via a second slab (`cells_next`) so all of `cells` is readable during a write.
Walkability: `Ground::walkable && !Feature::blocks` (soil yes, water no, rock blocks). One place
to change. Per-cell scalars (moisture, heat, mana...) are further arrays in `ChunkCells`.

Streaming per frame (`app/window.rs`): load radius = chunks needed to cover half the view + 1,
unload radius = load + 2. Unload of a dirty chunk writes it; without a store, dirty chunks stay.

## Open questions (fill in as the design lands)

- Actor model: SoA arrays keyed by `ActorId` with a free list; what components?
- Tick model: fixed timestep? sub-stepping? interpolation for render?
- Movement/conflict resolution when two actors want one cell (per-chunk intents + in-order merge?)
- Actors crossing chunk borders / standing in a chunk that gets unloaded (freeze with the chunk?)
- Cross-chunk neighbour reads for cell systems: `cells_next` slab + halo, or stitched 66x66 scratch?
- Save/replay format?
