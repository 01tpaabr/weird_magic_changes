# Performance baselines

Update when the hot path changes. Always record: machine, thread count, `n`, profile.
Run: `make bench` (criterion, `target/criterion/report/index.html`) and
`cargo run --release -p app --bin wmc -- show 4096 4096 7`. Thread count: `WMC_THREADS=n`.
Render benches (`crates/app/benches/render.rs`) are sized like a 2560x1440 window at a
16 px cell: 160x90 cells. Rows before 2026-09-23 "bevy" are from the rayon + Zig build;
the `blit/*` rows are obsolete (the GPU draws the tiles now) and kept for the record.

| date | commit | machine | bench | n | result | notes |
|------|--------|---------|-------|---|--------|-------|
| 2026-09-23 | initial | Apple Silicon 8c | `wmc 1000000 600` release | 1e6 | 88.7 µs/step (53 ms / 600 steps), 8 threads | skeleton saxpy step, memory-bound |
| 2026-09-23 | stage | Apple Silicon 8c | `wmc 4096 4096 7` release, generate only | 16.8e6 cells | 60.7 ms (≈276 M cells/s), 8 threads | value-noise ground + hashed rocks, row-band chunks; 2048² = 31.8 ms incl. pool startup |
| 2026-09-23 | chunked | Apple Silicon 8c | `wmc show 4096 4096 7` release, generate only | 4096 chunks = 16.8e6 cells | 77 ms (≈218 M cells/s), 8 threads | first version copied chunks vec→slab: 102 ms; now generated in place via `cells_mut_at`. Remaining gap to the flat layout is the zero-fill of fresh slab slots (~100 MB sequential) |
| 2026-09-23 | chunked | Apple Silicon 8c | criterion `generate_many/32x32 chunks` | 4.2e6 cells | 18.2 ms (≈231 M cells/s) | pure generation, no slab |
| 2026-09-23 | chunked | Apple Silicon 8c | criterion `stage_checksum/32x32 chunks` | 4.2e6 cells | 5.2 ms | fnv1a per chunk in parallel, fold in coord order |
| 2026-09-23 | chunked | Apple Silicon 8c | criterion `ensure_loaded/sweep 1 chunk/frame, radius 2` | 5 chunks gen + 5 unload per frame | 224 µs/frame | the per-frame streaming cost while panning; budget at 60 fps is 16 ms |
| 2026-09-23 | window | Apple Silicon 8c | criterion `render_cells/160x90` | 14.4e3 cells | 50.6 µs (1 thr), 45.7 µs (8 thr) | phase 1, Stage -> CellFrame. Too small to gain from rows in parallel (rayon overhead ≈ work); keep row-parallel for 4K/zoomed-out views, revisit if it ever shows in a profile |
| 2026-09-23 | window | Apple Silicon 8c | criterion `blit/reference/1` | 3.7e6 px | 8.59 ms (429 Mpx/s) | scalar Rust oracle, per-channel loop, single thread |
| 2026-09-23 | window | Apple Silicon 8c | criterion `blit/zig/1` | 3.7e6 px | 1.85 ms (1.99 Gpx/s) | Zig `wmc_blit_cells`, pixel-row-major, cov 0/255 fast paths, single thread: 4.6x the reference. ~8 GB/s of writes |
| 2026-09-23 | window | Apple Silicon 8c | criterion `blit/zig/8` | 3.7e6 px | 0.47 ms (7.8 Gpx/s) | + band-parallel driver, `BAND_ROWS = 4`, 8 threads: 3.9x over 1 thread, ~31 GB/s writes, i.e. memory-bound. A 4K frame costs <0.5 ms of a 16 ms budget; no SIMD work warranted yet |
| 2026-09-23 | smooth camera | Apple Silicon 8c | criterion `blit/zig/1` and `blit/zig/8` | 3.7e6 px | 1.96 ms (1 thr), 0.52 ms (8 thr) | kernel gained a signed pixel origin + per-cell-row clipping for sub-cell scrolling: +6% / +14% vs the unclipped kernel. Chosen over a scratch buffer + copy, which would add a full 15 MB write per frame. `blit/reference/1` 11.1 ms (now per-window-pixel with division, slower oracle, irrelevant) |
| 2026-09-23 | time | Apple Silicon 8c | criterion `render_cells/160x90` | 14.4e3 cells | 55.4 µs (1 thr), 49.1 µs (8 thr) | was 48.3 / 46.6 µs: +14% / +5% for scaling both colours of every cell by daylight (6 mul + 6 div-by-const per cell). Chosen over a pre-scaled palette table because 7 µs of a 16 ms frame is not worth a second palette type; revisit if phase 1 ever appears in a profile |
| 2026-09-23 | time | Apple Silicon 8c | `wmc run <dir> 1000 1024 1024 7` release | 256 chunks | 0 ns/tick, checksum `a06a0a89729ce5a9` on 1 and 8 threads | `World::step` has no systems yet, so the loop folds to `tick += n`; this row exists so the first system has a "before". Same checksum with `RAYON_NUM_THREADS=1` is the determinism gate |
