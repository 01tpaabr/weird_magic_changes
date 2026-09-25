# weird_magic_changes — agent instructions

Massively parallel game/simulation in Rust on **Bevy** (0.19). Bevy's ECS is the world
state, its schedule is the tick, its task pool is the parallelism. Design details are TBD
and will be added to `docs/`; the rules below apply regardless of the design.

## Priorities, in order

When two of these conflict, the higher one wins.

1. **Determinism.** Same seed + same inputs => bit-identical state, on 1 thread or 64.
   Fixed reduction order, seeded RNG per entity/chunk (never a global RNG), no wall-clock
   in the sim, no iteration over `HashMap`, no reliance on entity spawn order or `Entity`
   ids. Time is integer ticks (`sim_core::time`), never floats; a system's *cadence*
   (every 2^k ticks, staggered by a hash of a stable id: the chunk coord for cell
   systems, the actor `uid` for actors) is separate from the tick and from real-time
   speed (`app::clock`). The sim schedule (`SimTick`) is built with ambiguity
   detection set to **error**: two systems that touch the same data must be ordered
   explicitly. Every system gets a determinism test (`WMC_THREADS=1` vs default
   checksums, see `make test`). If it isn't deterministic, it isn't done.
2. **Data layout before algorithms.** Chunks are entities; a chunk's cells are one
   component holding flat arrays (SoA). Actors are dense `u32` ids inside chunk data,
   not one entity per actor. No `Box<dyn Trait>` in hot data, no per-entity allocation
   inside a tick, no `Commands` spawn/despawn inside a hot phase.
3. **Parallelism by structure, not by locks.** A tick is a sequence of *phases*
   (`SystemSet`s, chained); inside a phase, systems have disjoint data access (Bevy
   checks) and iterate chunks with `Query::par_iter_mut`. Cross-chunk effects go through
   double-buffering (read `Prev`, write `Next` component) or per-chunk accumulation +
   merge in coordinate order. `Mutex`/`RwLock`/`Arc<Mutex>` inside a phase is a design bug.
   `bevy::tasks::Parallel` (per-thread buckets) only if you sort the drained result by a
   stable key.
4. **Measure, then optimize.** No perf change without a before/after criterion number in
   the commit message. `make bench`. Keep `docs/PERF.md` current. Profile (`samply`,
   Instruments, Bevy's `trace` feature + Tracy) and look; don't guess.
5. **Iteration speed over polish.** It's a game: broken is cheap, slow feedback is not.
   Small commits, `make check` green, delete code rather than abstract it. Three similar
   lines beat one generic helper. No traits until there are two real implementations.
   Dev builds link Bevy dynamically (`--features app/dev`, done by `make`).
6. **The sim never sees the engine's clock or the renderer.** `sim-core` depends on
   `bevy_ecs` + `bevy_tasks` only: no `Time`, no assets, no window. Rendering, input,
   camera and the real-time driver live in `app`. No `unsafe` anywhere.

## Layout

```
crates/app          binary `wmc` (show/play/run): Bevy App, plugins, camera, clock, renderer
crates/sim-core     bevy_ecs world: Stage (chunk entities, 64x64 cells), actors (rows per chunk + phases), rules (VM, compiler), worldgen, store, rng, time, SimTick
rules/              *.rules files: the kinds (all built into the binary; WMC_RULES=<dir> swaps them)
docs/               ARCHITECTURE.md (decisions), ACTORS.md (actor + rules design), PERF.md (baselines)
.claude/skills/     bevy-dev (auto-loaded each session), parallel-sim (on demand)
```

## Commands

```
make            build (dev: opt-level 1, deps opt-level 3, Bevy dynamic_linking)
make run ARGS="show 80 24 42"      # or ARGS="play saves/dev [w h seed]" (WASD, space=pause, .=step, [ ]=speed, p=save, r=reload rules, q=quit)
make run ARGS="run saves/dev 1000" # headless: step N ticks, print µs/tick + checksum (WMC_THREADS=1 must match)
make run ARGS="lint rules/"        # compile a rules dir/file, print the kind table; WMC_RULES=<dir> makes show/play/run use it
make run ARGS="why saves/dev 77 103 3000"  # explain the next think of the actor at (x, y) after N ticks (-v: every op)
make check      fmt + clippy -D warnings        (pre-commit runs this)
make test       cargo test (unit + the determinism integration test)
make ci         check + test  == "done"
make bench      criterion, results in target/criterion
make release    LTO, static Bevy
make fmt
```

`cargo build --profile fast` = release speed with dev build times, for perf iteration.
`WMC_THREADS=n` sizes Bevy's task pools (`run`, `show`, `play`, and the tests).

## Workflow rules

- Before saying a task is done: `make ci` passes. Paste failures verbatim if not.
- New Rust dependency: add to `[workspace.dependencies]` in the root `Cargo.toml` first,
  then `foo.workspace = true` in the crate. Prefer the pre-approved list there. Prefer
  what Bevy already ships (`bevy::math` = glam, `bevy::platform::collections`, tasks).
- Unsure about a Bevy 0.19 API? Don't guess from memory (training data is mostly
  0.14–0.16, and 0.17–0.19 renamed a lot). Read the checked-out source:
  `~/.cargo/registry/src/*/bevy_ecs-0.19.*/src`, or `cargo doc -p bevy --no-deps --open`.
  The skill's `references/` hold verified cheatsheets.
- New sim system: a plain fn in `sim-core`, added to `SimTick` inside a `Phase` set, with a
  checksum test. New actor kind: a `.rules` file (`docs/ACTORS.md` §5), `wmc lint` it. New
  sense or action: an opcode in `rules/vm.rs` + a keyword in `rules/compile.rs` + a test. New per-cell layer: a field in `ChunkCells`, folded into `hash`, encoded
  in `store` (bump `FORMAT_VERSION`), rendered in `app/render/palette.rs`. Same commit.
- Rust edition is 2024 (`gen` is reserved, `unsafe` ops inside `unsafe fn` must be wrapped).
- Remote: `origin` = github.com/01tpaabr/weird_magic_changes. Commit on `main` in small steps
  and push; no PRs yet.
- Keep this file short. Design rationale goes in `docs/ARCHITECTURE.md`; deep how-to goes
  in the skills.

## Skills

- `/bevy-dev` — Bevy 0.19 patterns for this project: ECS layout, schedules, task pools,
  deterministic parallel iteration, rendering the grid, testing. Injected automatically at
  session start via hook; invoke again after context compaction.
- `/parallel-sim` — patterns for parallel systems: phases, chunking, double buffers,
  deterministic reductions, false sharing, job graphs, how to test and profile them.
