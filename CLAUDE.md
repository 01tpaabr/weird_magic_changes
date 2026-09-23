# weird_magic_changes — agent instructions

Massively parallel game/simulation. Rust orchestrates (world state, threading, systems,
app); Zig provides tight SIMD compute kernels behind a C ABI. Design details are TBD and
will be added to `docs/`; the rules below apply regardless of the design.

## Priorities, in order

When two of these conflict, the higher one wins.

1. **Determinism.** Same seed + same inputs => bit-identical state, on 1 thread or 64.
   Fixed reduction order, seeded RNG per entity/chunk (never a global RNG), no wall-clock
   in the sim, no iteration over HashMap. Every system gets a test that runs it with
   `num_threads(1)` and `num_threads(N)` and compares checksums. If it isn't
   deterministic, it isn't done.
2. **Data layout before algorithms.** Flat `Vec<T>` structure-of-arrays, indices not
   pointers, entities as `u32` ids, no `Box<dyn Trait>` in hot data, no per-entity
   allocation inside a tick. Parallelism is a property of the layout; you cannot bolt it
   on later.
3. **Parallelism by structure, not by locks.** A tick is a sequence of *phases*; inside a
   phase, work is split into independent chunks (`par_chunks_mut`) with no shared mutable
   state. Cross-entity effects go through double-buffering (read `prev`, write `next`)
   or per-thread accumulation + deterministic merge. `Mutex`/`RwLock` inside a phase is a
   design bug, not a fix. Atomics only for counters and only when order doesn't matter.
4. **Measure, then optimize.** No perf change without a before/after criterion number in
   the PR/commit message. `make bench`. Keep `docs/PERF.md` current. Don't guess about
   SIMD, cache, or scheduling; profile (`samply`, Instruments) and look.
5. **Iteration speed over polish.** It's a game: broken is cheap, slow feedback is not.
   Small commits, `make check` green, delete code rather than abstract it. Three similar
   lines beat one generic helper. No traits until there are two real implementations.
6. **Safety lives at the boundary.** `unsafe` and `extern "C"` exist only in
   `crates/zig-kernels`. Everything above sees safe Rust. Zig kernels are pure functions
   over caller-owned buffers: no allocation, no globals, no threads, no libc.

## Layout

```
crates/app          binary `wmc` (entry point, later: window/render/input)
crates/sim-core     world state + systems + tick (Rust, rayon)
crates/zig-kernels  the ONLY unsafe crate; build.rs runs `zig build`, safe wrappers
zig/                Zig package -> libwmc_kernels.a; src/root.zig = exported C ABI
docs/               ARCHITECTURE.md (decisions), PERF.md (baselines)
.claude/skills/     zig-rust-dev (auto-loaded each session), parallel-sim (on demand)
```

## Commands

```
make            build (dev: opt-level 1, Zig ReleaseSafe -> bounds checks on)
make run ARGS="1000000 600"
make check      fmt + clippy -D warnings + zig fmt        (pre-commit runs this)
make test       zig build test + cargo test
make ci         check + test  == "done"
make bench      criterion, results in target/criterion
make release    LTO + ReleaseFast
make fmt
```

`cargo build --profile fast` = release speed with dev build times, for perf iteration.

## Workflow rules

- Before saying a task is done: `make ci` passes. Paste failures verbatim if not.
- New Rust dependency: add to `[workspace.dependencies]` in the root `Cargo.toml` first,
  then `foo.workspace = true` in the crate. Prefer the pre-approved list there.
- New Zig kernel: file under `zig/src/kernels/`, `export fn wmc_*` in `zig/src/root.zig`,
  matching `extern "C"` + safe wrapper + test in `crates/zig-kernels/src/lib.rs`, bump
  `abi_version` on both sides if a signature changed. Same commit.
- Unsure about a Zig 0.16 std API? Don't guess from memory (training data is mostly
  0.11–0.14, which differs). Grep the real stdlib: `$(zig env | grep std_dir)`; on this
  machine `/opt/homebrew/Cellar/zig/0.16.0_1/lib/zig/std`. Or `zig init` in scratch.
- Rust edition is 2024: `unsafe extern "C"` blocks, `unsafe` ops inside `unsafe fn` must
  be wrapped, `gen` is reserved.
- Git is local-only for now. Commit on `main` in small steps; no remote, no PRs yet.
- Keep this file short. Design rationale goes in `docs/ARCHITECTURE.md`; deep how-to goes
  in the skills.

## Skills

- `/zig-rust-dev` — Rust+Zig coding practices, FFI rules, Zig 0.16 cheatsheet. Injected
  automatically at session start via hook; invoke again after context compaction.
- `/parallel-sim` — patterns for parallel systems: phases, chunking, double buffers,
  deterministic reductions, false sharing, job graphs, how to test and profile them.
