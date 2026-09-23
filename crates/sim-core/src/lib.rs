//! Simulation core.
//!
//! This is a placeholder skeleton that proves the pipeline: SoA world state,
//! a parallel tick over chunks with rayon, hot inner loop delegated to a Zig
//! SIMD kernel. Replace the contents once the actual sim design lands, but keep
//! the shape: **data in flat arrays, systems as pure functions over slices,
//! parallelism at the chunk level, determinism by construction.**

use rayon::prelude::*;

/// Structure-of-arrays world state. Each field is one contiguous buffer so
/// systems can iterate a single attribute cache-friendly and chunk it for rayon.
#[derive(Debug, Clone, Default)]
pub struct World {
    pub pos: Vec<f32>,
    pub vel: Vec<f32>,
    pub tick: u64,
}

/// Work unit size for rayon. Tune with benches; too small = scheduling overhead,
/// too large = poor load balance. 4096 f32 = 16 KiB, comfortably in L1.
pub const CHUNK: usize = 4096;

impl World {
    /// Deterministic world with `n` entities. Same `seed` => same world, always.
    pub fn new(n: usize, seed: u64) -> Self {
        let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = move || {
            // xorshift64*: tiny, fast, deterministic; fine for init, use rand_xoshiro for gameplay.
            s ^= s >> 12;
            s ^= s << 25;
            s ^= s >> 27;
            (s.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32
        };
        let pos = (0..n).map(|_| next() * 100.0).collect();
        let vel = (0..n).map(|_| next() - 0.5).collect();
        Self { pos, vel, tick: 0 }
    }

    /// Advance one step: `pos += vel * dt` over all entities in parallel.
    pub fn step(&mut self, dt: f32) {
        self.pos
            .par_chunks_mut(CHUNK)
            .zip(self.vel.par_chunks(CHUNK))
            .for_each(|(p, v)| zig_kernels::saxpy(dt, v, p));
        self.tick += 1;
    }

    /// A cheap checksum for determinism tests and bug reports.
    pub fn checksum(&self) -> f32 {
        zig_kernels::sum(&self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_is_deterministic_across_thread_counts() {
        let run = |threads: usize| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            pool.install(|| {
                let mut w = World::new(100_000, 42);
                for _ in 0..10 {
                    w.step(1.0 / 60.0);
                }
                w.checksum().to_bits()
            })
        };
        assert_eq!(run(1), run(8));
    }

    #[test]
    fn step_moves_entities() {
        let mut w = World::new(10, 1);
        let before = w.pos.clone();
        w.step(1.0);
        for ((a, b), v) in before.iter().zip(&w.pos).zip(&w.vel) {
            assert!((b - (a + v)).abs() < 1e-5);
        }
        assert_eq!(w.tick, 1);
    }
}
