# weird_magic_changes

Massively parallel game / simulation. Rust for orchestration, Zig for SIMD kernels.

```
make setup     # installs zig (brew), rust components, git hooks, runs make ci
make run ARGS="show 80 24 42"        # print a map once
make run ARGS="play saves/dev"       # interactive: WASD pan, space tick, p save, q quit
make ci        # fmt + clippy + zig fmt + zig tests + cargo tests
```

Read `CLAUDE.md` for the rules and layout, `docs/ARCHITECTURE.md` for decisions,
`docs/PERF.md` for baselines. Toolchain: Rust stable (edition 2024), Zig 0.16.
