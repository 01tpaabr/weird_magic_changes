# Parallel sim patterns (Rust + Bevy 0.19), copy-paste ready

Exact API signatures: `.claude/skills/bevy-dev/references/ecs-tasks-app.md`.

## Phase over chunks: one system, `par_iter_mut`

```rust
fn grow(tick: Res<Tick>, cfg: Res<SimConfig>,
        mut chunks: Query<(&ChunkCoord, &mut ChunkCells, &mut ChunkMeta)>) {
    chunks.par_iter_mut().for_each(|(coord, mut cells, mut meta)| {
        let mut rng = rng_for(cfg.seed, tick.0, coord_id(*coord));   // derived seed, never shared
        for i in 0..CHUNK_CELLS { /* this chunk only */ }
        meta.dirty = true;
    });
}
// install: schedule.add_systems(grow.in_set(Phase::Simulate));
```
Each task gets whole entities; writes never overlap; batch size is irrelevant to the result.

## Double buffer across chunks (neighbour reads)

```rust
#[derive(Component)] struct CellsPrev(ChunkCells);          // snapshot of last tick

fn snapshot(mut q: Query<(&ChunkCells, &mut CellsPrev)>) {  // Phase::Snapshot
    q.par_iter_mut().for_each(|(cur, mut prev)| prev.0.clone_from(cur));
}
fn diffuse(stage: Res<Stage>, prev: Query<&CellsPrev>,       // read ANY chunk's prev
           mut cur: Query<(&ChunkCoord, &mut ChunkCells)>) {  // write OWN chunk's cur
    cur.par_iter_mut().for_each(|(c, mut cells)| {
        let north = stage.entity(ChunkCoord::new(c.x, c.y - 1)).and_then(|e| prev.get(e).ok());
        /* read prev of self + neighbours, write cells */
    });
}
// configure_sets((Phase::Snapshot, Phase::Simulate).chain())
```
Two queries on different components: Bevy accepts it; one mutable + one read-only on the
same component would not compile, which is the point.

## Per-chunk accumulate, merge in coordinate order

```rust
#[derive(Component, Default)] struct Outbox(Vec<Effect>);   // on the chunk entity

fn emit(mut q: Query<(&ChunkCells, &mut Outbox)>) {           // parallel: write own outbox
    q.par_iter_mut().for_each(|(cells, mut out)| { out.0.clear(); /* push effects */ });
}
fn apply(stage: Res<Stage>, boxes: Query<&Outbox>, mut cells: Query<&mut ChunkCells>) {
    for &(_, e) in stage.active() {                            // sequential, fixed order
        for fx in &boxes.get(e).unwrap().0 { /* apply to cells.get_mut(target) */ }
    }
}
```
Never `f32` atomics. `bevy::utils::Parallel<Vec<T>>` works too, but `drain()` is in thread
order: `sort_unstable_by_key` on a stable key before applying.

## Arbitrary parallel map with ordered results (task pool scope)

```rust
let hashes: Vec<u64> = sim_core::par::par_map(stage.active(), 16, |&(coord, e)| {
    splitmix64(cells.get(e).unwrap().hash() ^ coord_bits(coord))
});                                                            // input order, any thread count
let total = hashes.iter().fold(SEED, |a, &h| splitmix64(a ^ h));
```
Underneath: `ComputeTaskPool::get().scope(|s| for batch in .. { s.spawn(async move {..}) })`
returns `Vec<T>` in spawn order. Disjoint `&mut` slices are split *before* the scope and
moved into the tasks (see `app/src/render/cells.rs`).

## Spatial bucketing (counting sort, deterministic)

```rust
// 1. count per cell (parallel per chunk into the chunk's own counts), 2. exclusive prefix
// sum sequentially in active() order, 3. scatter (parallel per chunk, each writes its own
// range). Query = read-only slice of `sorted[start[c]..start[c+1]]`.
```

## Deterministic RNG per work unit

```rust
let mut rng = rng_for(cfg.seed, tick.0, coord_id(coord));   // Xoshiro256++ from splitmix64
let v = hash_cell(cfg.seed, STREAM_ROCK, x, y);             // or a pure hash per cell
```
Never an `Entity`, never a slot, never a global RNG.

## Determinism test

```rust
// In-process: two fresh worlds agree.
let a = { let mut w = sim::new_world(&cfg); for _ in 0..200 { sim::step(&mut w); } sim::checksum(&mut w) };
let b = { let mut w = sim::new_world(&cfg); for _ in 0..200 { sim::step(&mut w); } sim::checksum(&mut w) };
assert_eq!(a, b);
// Across thread counts: crates/app/tests/determinism.rs runs the binary with
// WMC_THREADS=1, 3, 8 and compares the `checksum:` lines (the pool is per process).
```

## Ambiguity as a build error

```rust
schedule.set_build_settings(ScheduleBuildSettings {
    ambiguity_detection: LogLevel::Error, ..Default::default() });
```
Two systems in a phase that both write `ChunkCells` with no `.before/.after/.chain()` make
`run_schedule` panic on first run. That is the executor refusing to pick an order for you.

## Anti-patterns (rejected in review)

- `Arc<Mutex<..>>`, `RwLock`, `RefCell` inside a phase.
- `Commands` spawn/despawn inside a phase; chunk entities change only in `ensure_loaded`.
- Iterating a query and letting its order matter (sort via `stage.active()` instead).
- `par_iter().for_each` that pushes to a shared `Vec` or adds to a shared float.
- Seeding from `Entity`, `Instant`, thread id, or a global RNG.
- Reading `Time` or any wall clock inside `sim-core`.
