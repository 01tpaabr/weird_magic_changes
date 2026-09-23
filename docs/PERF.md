# Performance baselines

Update when the hot path changes. Always record: machine, thread count, `n`, profile.
Run: `make bench` (criterion, `target/criterion/report/index.html`) and
`cargo run --release -p app --bin wmc -- 1000000 600`.

| date | commit | machine | bench | n | result | notes |
|------|--------|---------|-------|---|--------|-------|
| 2026-09-23 | initial | Apple Silicon 8c | `wmc 1000000 600` release | 1e6 | 88.7 µs/step (53 ms / 600 steps), 8 threads | skeleton saxpy step, memory-bound |
