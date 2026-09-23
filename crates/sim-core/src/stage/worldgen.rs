//! Deterministic stage generation.
//!
//! Every cell is a **pure function of `(seed, x, y)`**: ground comes from
//! thresholded value noise, rocks from a per-cell hash. There is no sequential
//! state, so the result is identical for any thread count *and* any chunk
//! size. The parallel split is by row band purely for throughput.

use rayon::prelude::*;

use super::{Feature, Ground, Stage};
use crate::rng::{hash_cell, unit_f32};

/// Hash streams used by generation. Never reuse a value elsewhere.
pub const STREAM_GROUND: u64 = 0x0001;
pub const STREAM_ROCK: u64 = 0x0002;

/// Knobs. Defaults give a soil map with a few lakes and scattered rocks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GenParams {
    /// Noise feature size in cells. Larger = bigger, smoother lakes.
    pub water_scale: f32,
    /// Fraction of cells that end up water, roughly (noise is in `[0, 1]`).
    pub water_level: f32,
    /// Probability that a soil cell holds a rock.
    pub rock_on_soil: f32,
    /// Probability that a water cell holds a rock.
    pub rock_on_water: f32,
}

impl Default for GenParams {
    fn default() -> Self {
        Self {
            water_scale: 12.0,
            water_level: 0.30,
            rock_on_soil: 0.04,
            rock_on_water: 0.01,
        }
    }
}

/// Generate a `width x height` stage from `seed`.
pub fn generate(width: u32, height: u32, seed: u64, params: &GenParams) -> Stage {
    let mut stage = Stage::new(width, height);
    let chunk = stage.chunk_len();
    let Stage {
        ground, feature, ..
    } = &mut stage;

    ground
        .par_chunks_mut(chunk)
        .zip(feature.par_chunks_mut(chunk))
        .enumerate()
        .for_each(|(ci, (g, f))| {
            let first_row = u32::try_from(ci).expect("chunk index fits u32") * super::CHUNK_ROWS;
            for (row, (g, f)) in g
                .chunks_mut(width as usize)
                .zip(f.chunks_mut(width as usize))
                .enumerate()
            {
                let y = first_row + u32::try_from(row).expect("row fits u32");
                for (x, (g, f)) in g.iter_mut().zip(f.iter_mut()).enumerate() {
                    let x = u32::try_from(x).expect("column fits u32");
                    (*g, *f) = gen_cell(seed, params, x, y);
                }
            }
        });
    stage
}

/// The whole rule set for one cell. Pure.
#[inline]
fn gen_cell(seed: u64, p: &GenParams, x: u32, y: u32) -> (Ground, Feature) {
    let n = fbm2(
        seed,
        STREAM_GROUND,
        x as f32 / p.water_scale,
        y as f32 / p.water_scale,
    );
    let ground = if n < p.water_level {
        Ground::Water
    } else {
        Ground::Soil
    };
    let rock_p = match ground {
        Ground::Soil => p.rock_on_soil,
        Ground::Water => p.rock_on_water,
    };
    let feature = if unit_f32(hash_cell(seed, STREAM_ROCK, x, y)) < rock_p {
        Feature::Rock
    } else {
        Feature::None
    };
    (ground, feature)
}

/// Two octaves of value noise in `[0, 1]`. Enough for lakes; swap for
/// simplex/perlin when terrain needs it.
fn fbm2(seed: u64, stream: u64, x: f32, y: f32) -> f32 {
    let a = value_noise(seed, stream, x, y);
    let b = value_noise(seed, stream ^ 0x5555, x * 2.0 + 17.0, y * 2.0 + 31.0);
    (a * 2.0 + b) / 3.0
}

/// Bilinear value noise: hashed lattice values, smoothstep-interpolated.
fn value_noise(seed: u64, stream: u64, x: f32, y: f32) -> f32 {
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = smoothstep(x - x0);
    let fy = smoothstep(y - y0);
    // Lattice coords are non-negative here (callers pass cell coords / scale).
    let (ix, iy) = (x0 as u32, y0 as u32);
    let l = |dx: u32, dy: u32| unit_f32(hash_cell(seed, stream, ix + dx, iy + dy));
    let top = lerp(l(0, 0), l(1, 0), fx);
    let bot = lerp(l(0, 1), l(1, 1), fx);
    lerp(top, bot, fy)
}

#[inline]
fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_threads<R: Send>(n: usize, f: impl FnOnce() -> R + Send) -> R {
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .unwrap()
            .install(f)
    }

    #[test]
    fn generate_is_deterministic_across_thread_counts() {
        let p = GenParams::default();
        let a = with_threads(1, || generate(301, 173, 42, &p).checksum());
        let b = with_threads(8, || generate(301, 173, 42, &p).checksum());
        assert_eq!(a, b);
    }

    #[test]
    fn generate_matches_per_cell_rule() {
        // Chunking must not leak into results: recompute every cell sequentially.
        let p = GenParams::default();
        let s = generate(97, 53, 7, &p);
        for y in 0..53 {
            for x in 0..97 {
                let i = s.idx(super::super::Pos::new(x, y)).usize();
                assert_eq!(
                    (s.ground[i], s.feature[i]),
                    gen_cell(7, &p, x, y),
                    "cell {x},{y}"
                );
            }
        }
    }

    #[test]
    fn different_seeds_differ_and_have_all_tile_kinds() {
        let p = GenParams::default();
        let a = generate(128, 128, 1, &p);
        let b = generate(128, 128, 2, &p);
        assert_ne!(a.checksum(), b.checksum());
        let water = a.ground.iter().filter(|g| **g == Ground::Water).count();
        let rocks = a.feature.iter().filter(|f| **f == Feature::Rock).count();
        assert!(water > 0 && water < a.len());
        assert!(rocks > 0);
        // Rocks appear on both grounds.
        let on_water = a
            .ground
            .iter()
            .zip(&a.feature)
            .any(|(g, f)| *g == Ground::Water && *f == Feature::Rock);
        assert!(on_water);
    }

    #[test]
    fn noise_stays_in_unit_range() {
        for y in 0..64 {
            for x in 0..64 {
                let n = fbm2(3, STREAM_GROUND, x as f32 / 5.0, y as f32 / 5.0);
                assert!((0.0..=1.0).contains(&n), "{n}");
            }
        }
    }
}
