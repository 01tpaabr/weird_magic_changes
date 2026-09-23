# Performance baselines

Update when the hot path changes. Always record: machine, thread count, `n`, profile.
Run: `make bench` (criterion, `target/criterion/report/index.html`) and
`cargo run --release -p app --bin wmc -- show 4096 4096 7`.
Render benches (`crates/app/benches/render.rs`) are sized like a 2560x1440 window at a
16 px cell: 160x90 cells, 3.7 Mpx.

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
