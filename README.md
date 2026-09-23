# weird_magic_changes

Massively parallel game / simulation in Rust on Bevy 0.19.

```
make setup     # rust components, git hooks, runs make ci (first Bevy build takes minutes)
make run ARGS="show 80 24 42"        # print a map once
make run ARGS="play saves/dev"       # window: WASD glide, space pause, . step, [ ] speed, p save, q quit
make run ARGS="run saves/dev 1000"   # headless: N ticks, µs/tick, checksum (WMC_THREADS=1 must match)
make ci        # fmt + clippy + tests (incl. the cross-thread-count determinism gate)
```

Read `CLAUDE.md` for the rules and layout, `docs/ARCHITECTURE.md` for decisions,
`docs/PERF.md` for baselines. Toolchain: Rust stable >= 1.95 (edition 2024).
