# Performance baselines

Update when the hot path changes. Always record: machine, thread count, `n`, profile.
Run: `make bench` (criterion, `target/criterion/report/index.html`) and
`cargo run --release -p app --bin wmc -- show 4096 4096 7`. Thread count: `WMC_THREADS=n`.
Render benches (`crates/app/benches/render.rs`) are sized like a 2560x1440 window at a
16 px cell: 160x90 cells. Rows before 2026-09-23 "bevy" are from the rayon + Zig build and stay as the "before" of
the migration; the CPU blit rows were dropped with the blit (the GPU draws the tiles now).

| date | commit | machine | bench | n | result | notes |
|------|--------|---------|-------|---|--------|-------|
| 2026-09-23 | initial | Apple Silicon 8c | `wmc 1000000 600` release | 1e6 | 88.7 µs/step (53 ms / 600 steps), 8 threads | skeleton saxpy step, memory-bound |
| 2026-09-23 | stage | Apple Silicon 8c | `wmc 4096 4096 7` release, generate only | 16.8e6 cells | 60.7 ms (≈276 M cells/s), 8 threads | value-noise ground + hashed rocks, row-band chunks; 2048² = 31.8 ms incl. pool startup |
| 2026-09-23 | chunked | Apple Silicon 8c | `wmc show 4096 4096 7` release, generate only | 4096 chunks = 16.8e6 cells | 77 ms (≈218 M cells/s), 8 threads | first version copied chunks vec→slab: 102 ms; now generated in place via `cells_mut_at`. Remaining gap to the flat layout is the zero-fill of fresh slab slots (~100 MB sequential) |
| 2026-09-23 | chunked | Apple Silicon 8c | criterion `generate_many/32x32 chunks` | 4.2e6 cells | 18.2 ms (≈231 M cells/s) | pure generation, no slab |
| 2026-09-23 | chunked | Apple Silicon 8c | criterion `stage_checksum/32x32 chunks` | 4.2e6 cells | 5.2 ms | fnv1a per chunk in parallel, fold in coord order |
| 2026-09-23 | chunked | Apple Silicon 8c | criterion `ensure_loaded/sweep 1 chunk/frame, radius 2` | 5 chunks gen + 5 unload per frame | 224 µs/frame | the per-frame streaming cost while panning; budget at 60 fps is 16 ms |
| 2026-09-23 | window | Apple Silicon 8c | criterion `render_cells/160x90` | 14.4e3 cells | 50.6 µs (1 thr), 45.7 µs (8 thr) | phase 1, Stage -> CellFrame. Too small to gain from rows in parallel (rayon overhead ≈ work); keep row-parallel for 4K/zoomed-out views, revisit if it ever shows in a profile |
| 2026-09-23 | time | Apple Silicon 8c | criterion `render_cells/160x90` | 14.4e3 cells | 55.4 µs (1 thr), 49.1 µs (8 thr) | was 48.3 / 46.6 µs: +14% / +5% for scaling both colours of every cell by daylight (6 mul + 6 div-by-const per cell). Chosen over a pre-scaled palette table because 7 µs of a 16 ms frame is not worth a second palette type; revisit if phase 1 ever appears in a profile |
| 2026-09-23 | time | Apple Silicon 8c | `wmc run <dir> 1000 1024 1024 7` release | 256 chunks | 0 ns/tick, checksum `a06a0a89729ce5a9` on 1 and 8 threads | `World::step` has no systems yet, so the loop folds to `tick += n`; this row exists so the first system has a "before". Same checksum with `RAYON_NUM_THREADS=1` (now `WMC_THREADS=1`) was the determinism gate |
| 2026-09-23 | bevy | Apple Silicon 8c | criterion `generate_many/32x32 chunks` | 4.2e6 cells | 19.8 ms (212 M cells/s), 8 threads | rayon baseline 18.2 ms. First Bevy version collected one `Vec` per task and flattened them: 23.9 ms; `par_zip_mut` writes each chunk in place through disjoint slices: 19.8 ms (4x4 chunks: 354 µs, better than before). 1 vs 4 chunks per task is noise. **Before the `multi_threaded` feature was enabled on `bevy_tasks` for sim-core this was 94.8 ms: the pool ran serially. `sim-core` alone must ask for the feature** |
| 2026-09-23 | bevy | Apple Silicon 8c | criterion `stage_checksum/32x32 chunks` | 4.2e6 cells | 5.8 ms (721 M cells/s), 8 threads | was 5.2 ms: `par_map` in batches of 16 chunk entities, `Query::get` per chunk, fold in coord order |
| 2026-09-23 | bevy | Apple Silicon 8c | criterion `ensure_loaded/sweep 1 chunk/frame, radius 2` | 5 chunks gen + 5 unload per frame | 225 µs/frame | was 224 µs on rayon (250 µs with the copying `generate_many`): entity spawn/despawn costs the same as slab slots |
| 2026-09-23 | bevy | Apple Silicon 8c | criterion `step/16x16 chunks` | 256 chunks, no systems | 9.0 µs/tick | the empty `SimTick` schedule on the multi-threaded executor: ~9 µs fixed cost per tick (was `tick += 1`, 0 ns). Irrelevant at 8 TPS; caps `max` speed near 100k ticks/s. The number every new system moves |
| 2026-09-23 | bevy | Apple Silicon 8c | criterion `render_cells/160x90/8` | 14.4e3 cells | 31.2 µs, 8 threads | was 49.1 µs on rayon rows: 8-row bands on the task pool, closure chunk lookup |
| 2026-09-23 | bevy | Apple Silicon 8c | criterion `grid_upload/160x90` | 14.4e3 cells | 457 µs, 1 thread | phase 2 now: `CellFrame` -> two `TilemapChunkTileData` (two `Color::tint` sRGB->linear `powf` per cell). Replaces the 0.47 ms Zig blit of 3.7 Mpx; the GPU draws the pixels. Bevy then repacks 14.4k `TileData` per layer per frame. Candidate for a tint lookup table if it ever shows in a profile |
