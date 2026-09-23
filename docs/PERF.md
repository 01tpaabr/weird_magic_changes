# Performance baselines

Update when the hot path changes. Always record: machine, thread count, `n`, profile.
Run: `make bench` (criterion, `target/criterion/report/index.html`) and
`cargo run --release -p app --bin wmc -- 2048 2048 7`.

| date | commit | machine | bench | n | result | notes |
|------|--------|---------|-------|---|--------|-------|
| 2026-09-23 | initial | Apple Silicon 8c | `wmc 1000000 600` release | 1e6 | 88.7 µs/step (53 ms / 600 steps), 8 threads | skeleton saxpy step, memory-bound |
| 2026-09-23 | stage | Apple Silicon 8c | `wmc 4096 4096 7` release, generate only | 16.8e6 cells | 60.7 ms (≈276 M cells/s), 8 threads | value-noise ground + hashed rocks, row-band chunks; 2048² = 31.8 ms incl. pool startup |
