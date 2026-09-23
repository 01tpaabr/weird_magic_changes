# Performance baselines

Update when the hot path changes. Always record: machine, thread count, `n`, profile.
Run: `make bench` (criterion, `target/criterion/report/index.html`) and
`cargo run --release -p app --bin wmc -- show 4096 4096 7`.

| date | commit | machine | bench | n | result | notes |
|------|--------|---------|-------|---|--------|-------|
| 2026-09-23 | initial | Apple Silicon 8c | `wmc 1000000 600` release | 1e6 | 88.7 µs/step (53 ms / 600 steps), 8 threads | skeleton saxpy step, memory-bound |
| 2026-09-23 | stage | Apple Silicon 8c | `wmc 4096 4096 7` release, generate only | 16.8e6 cells | 60.7 ms (≈276 M cells/s), 8 threads | value-noise ground + hashed rocks, row-band chunks; 2048² = 31.8 ms incl. pool startup |
| 2026-09-23 | chunked | Apple Silicon 8c | `wmc show 4096 4096 7` release, generate only | 4096 chunks = 16.8e6 cells | 77 ms (≈218 M cells/s), 8 threads | first version copied chunks vec→slab: 102 ms; now generated in place via `cells_mut_at`. Remaining gap to the flat layout is the zero-fill of fresh slab slots (~100 MB sequential) |
| 2026-09-23 | chunked | Apple Silicon 8c | criterion `generate_many/32x32 chunks` | 4.2e6 cells | 18.2 ms (≈231 M cells/s) | pure generation, no slab |
| 2026-09-23 | chunked | Apple Silicon 8c | criterion `stage_checksum/32x32 chunks` | 4.2e6 cells | 5.2 ms | fnv1a per chunk in parallel, fold in coord order |
| 2026-09-23 | chunked | Apple Silicon 8c | criterion `ensure_loaded/sweep 1 chunk/frame, radius 2` | 5 chunks gen + 5 unload per frame | 224 µs/frame | the per-frame streaming cost while panning; budget at 60 fps is 16 ms |
