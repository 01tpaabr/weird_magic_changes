# Parallel sim patterns (Rust + rayon), copy-paste ready

## Phase over chunks with a Zig kernel inside
```rust
pub const CHUNK: usize = 4096;
pos.par_chunks_mut(CHUNK)
   .zip(vel.par_chunks(CHUNK))
   .for_each(|(p, v)| zig_kernels::saxpy(dt, v, p));
```

## Double buffer
```rust
pub struct Buf<T> { pub prev: Vec<T>, pub next: Vec<T> }
impl<T> Buf<T> { pub fn swap(&mut self) { std::mem::swap(&mut self.prev, &mut self.next); } }
// phase: reads prev (shared &), writes next (partitioned &mut)
let prev = &state.prev;
state.next.par_chunks_mut(CHUNK).enumerate().for_each(|(ci, out)| {
    let base = ci * CHUNK;
    for (i, o) in out.iter_mut().enumerate() { *o = rule(prev, base + i); }
});
state.swap();
```

## Per-chunk accumulate, deterministic merge
```rust
// scratch.per_chunk: Vec<Vec<Effect>>, one per chunk, pre-allocated and cleared each tick
scratch.per_chunk.par_iter_mut().enumerate().for_each(|(ci, out)| {
    out.clear();
    for e in chunk_entities(ci) { if let Some(fx) = compute(e) { out.push(fx); } }
});
for list in &scratch.per_chunk {          // sequential, chunk order => deterministic
    for fx in list { apply(&mut world, fx); }
}
```

## Deterministic float reduction
```rust
let partial: Vec<f32> = data.par_chunks(CHUNK).map(zig_kernels::sum).collect(); // chunk order preserved
let total: f32 = partial.iter().sum();   // sequential, fixed order
```

## Derived RNG (no shared state)
```rust
use rand::SeedableRng;
use rand_xoshiro::Xoshiro256PlusPlus;
fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}
pub fn rng_for(seed: u64, tick: u64, id: u32) -> Xoshiro256PlusPlus {
    Xoshiro256PlusPlus::seed_from_u64(splitmix64(seed ^ splitmix64(tick) ^ (id as u64) << 32))
}
```

## Uniform grid rebuilt per tick (counting sort; parallel-friendly, deterministic)
```rust
// 1. cell id per entity (par, pure)            cell[i] = hash_cell(pos[i])
// 2. histogram per chunk, then prefix sum over (chunk, cell) in fixed order -> offsets
// 3. scatter entity ids into `sorted` using per-chunk offsets (each chunk writes disjoint ranges)
// 4. queries: for each neighbor cell, iterate sorted[start[c]..start[c+1]] — read only
```

## Padded per-thread counters (avoid false sharing)
```rust
#[repr(align(64))] #[derive(Default)] struct Padded(std::sync::atomic::AtomicU64);
```

## Thread-count determinism test
```rust
fn run(threads: usize) -> u32 {
    rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap().install(|| {
        let mut w = World::new(100_000, 42);
        for _ in 0..10 { w.step(1.0 / 60.0); }
        w.checksum().to_bits()
    })
}
#[test] fn deterministic_across_threads() { assert_eq!(run(1), run(8)); }
```

## Sim/render handoff (triple buffer, no lock on the hot path)
Sim writes into `bufs[write]`, publishes index via `AtomicUsize` (Release); render loads
(Acquire) and reads. Or `crossbeam::channel::bounded(1)` with `try_send` and drop-if-full.
