# Architecture

Status: skeleton. Game/sim design not yet specified. This file records decisions that are
already made and the shape the design must fit into.

## Decisions

| # | Decision | Why | Revisit when |
|---|----------|-----|--------------|
| 1 | Rust owns the process; Zig is a static lib behind a C ABI | Rust's borrow checker enforces the "no shared mutation inside a phase" rule for free; Zig gives explicit SIMD and simple codegen for kernels | Never, unless Zig grows a safe threading story we want |
| 2 | Cargo is the single build driver; `build.rs` shells out to `zig build` | One command (`make`), one cache (`target/`), Zig opt mode follows cargo profile | If Zig side grows its own executables/tools |
| 3 | No threads in Zig; rayon in Rust | One scheduler, one mental model, deterministic chunking in one place | If a kernel needs intra-chunk parallelism (unlikely) |
| 4 | SoA world state in flat `Vec`s, `u32` entity ids | Parallelism and SIMD follow layout | N/A |
| 5 | Determinism across thread counts is a hard requirement | Reproducible bugs, replays, lockstep multiplayer option, testability | N/A |
| 6 | dev profile = opt-level 1 + Zig ReleaseSafe | opt-level 0 sim is unusable; ReleaseSafe keeps Zig bounds checks | If debug builds get too slow: `--profile fast` |
| 7 | `target-cpu=native` / `-Dcpu=native` | Sim runs where it's built, for now | When shipping binaries: switch to baseline + runtime dispatch |
| 8 | Git local only, commits on `main` | Solo, early | When a remote exists |

## Layers

```
app         window / input / render / config / logging   (later)
sim-core    World, phases, systems, scheduler, events
zig-kernels safe wrappers        <- only unsafe crate
zig/        pure compute kernels
```

Dependencies point downward only. `sim-core` never knows about rendering.

## Open questions (fill in as the design lands)

- Entity model: fixed arrays vs archetypes vs sparse sets?
- Tick model: fixed timestep? sub-stepping? interpolation for render?
- Spatial structure: uniform grid vs hierarchical?
- Rendering backend / windowing crate?
- Save/replay format?
