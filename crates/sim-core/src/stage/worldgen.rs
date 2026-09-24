//! Deterministic chunk generation.
//!
//! Every cell is a **pure function of `(seed, x, y)`**: ground comes from
//! thresholded value noise, rocks from a per-cell hash. No sequential state,
//! so a chunk's content never depends on when, in which order, or on how many
//! threads it was generated. This is what makes unloading a clean chunk free:
//! it can always be regenerated.

use super::{CHUNK_CELLS, ChunkCells, ChunkCoord, Feature, Ground};
use crate::par::par_zip_mut;
use crate::rng::{hash_cell, unit_f32};

/// Hash streams used by generation. Never reuse a value elsewhere.
pub const STREAM_GROUND: u64 = 0x0001;
pub const STREAM_ROCK: u64 = 0x0002;

/// Knobs. Defaults give a soil map with a few lakes and scattered rocks.
/// Stored in the save file: changing them changes every unsaved chunk.
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

/// Fill `out` with chunk `coord`. Sequential inside the chunk; callers
/// parallelise across chunks (see [`generate_many`]).
pub fn generate_chunk(seed: u64, params: &GenParams, coord: ChunkCoord, out: &mut ChunkCells) {
    let ChunkCells {
        ground, feature, ..
    } = out;
    for (i, (g, f)) in ground.iter_mut().zip(feature.iter_mut()).enumerate() {
        let p = coord.cell(i);
        (*g, *f) = gen_cell(seed, params, p.x, p.y);
    }
    out.occupant = [super::ActorId::NONE; CHUNK_CELLS];
}

/// Generate many chunks in parallel, results in input order. Each task
/// writes its chunks straight into their final slots (disjoint slices of
/// the output), so nothing is copied afterwards.
pub fn generate_many(seed: u64, params: &GenParams, coords: &[ChunkCoord]) -> Vec<ChunkCells> {
    let mut out = vec![ChunkCells::default(); coords.len()];
    par_zip_mut(coords, &mut out, GEN_BATCH, |&c, cells| {
        generate_chunk(seed, params, c, cells);
    });
    out
}

/// Chunks per generation task. Measured (`make bench`, `generate_many`):
/// 1 and 4 within noise of each other at 32x32 chunks; 1 keeps the small
/// streaming batches (5 chunks) fully parallel.
const GEN_BATCH: usize = 1;

/// The whole rule set for one cell. Pure.
#[inline]
fn gen_cell(seed: u64, p: &GenParams, x: i32, y: i32) -> (Ground, Feature) {
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
/// Works for negative coordinates (`floor`, not truncation).
fn value_noise(seed: u64, stream: u64, x: f32, y: f32) -> f32 {
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = smoothstep(x - x0);
    let fy = smoothstep(y - y0);
    let (ix, iy) = (x0 as i32, y0 as i32);
    let l = |dx: i32, dy: i32| unit_f32(hash_cell(seed, stream, ix + dx, iy + dy));
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
    use crate::stage::Pos;

    fn grid(r: i32) -> Vec<ChunkCoord> {
        (-r..r)
            .flat_map(|y| (-r..r).map(move |x| ChunkCoord::new(x, y)))
            .collect()
    }

    /// Parallel generation equals serial generation, chunk for chunk. The
    /// thread-count half of the gate is `crates/app/tests/determinism.rs`
    /// (`WMC_THREADS=1` vs default on the real binary).
    #[test]
    fn generate_many_matches_serial_generation() {
        crate::par::init_task_pool();
        let p = GenParams::default();
        let coords = grid(3);
        let par: Vec<u64> = generate_many(42, &p, &coords)
            .iter()
            .map(ChunkCells::hash)
            .collect();
        let serial: Vec<u64> = coords
            .iter()
            .map(|&c| {
                let mut cells = ChunkCells::default();
                generate_chunk(42, &p, c, &mut cells);
                cells.hash()
            })
            .collect();
        assert_eq!(par, serial);
    }

    #[test]
    fn chunk_matches_per_cell_rule_and_is_order_free() {
        let p = GenParams::default();
        let coord = ChunkCoord::new(-2, 3);
        let mut cells = ChunkCells::default();
        generate_chunk(7, &p, coord, &mut cells);
        for i in 0..CHUNK_CELLS {
            let Pos { x, y } = coord.cell(i);
            assert_eq!(
                (cells.ground[i], cells.feature[i]),
                gen_cell(7, &p, x, y),
                "{x},{y}"
            );
        }
        // Regenerating into a dirty buffer gives the same bytes.
        let mut again = ChunkCells::default();
        again.occupant[3] = super::super::ActorId(1);
        generate_chunk(7, &p, coord, &mut again);
        assert_eq!(cells.hash(), again.hash());
    }

    #[test]
    fn different_seeds_differ_and_have_all_tile_kinds() {
        crate::par::init_task_pool();
        let p = GenParams::default();
        let a = generate_many(1, &p, &grid(2));
        let b = generate_many(2, &p, &grid(2));
        assert_ne!(a[0].hash(), b[0].hash());
        let cells = a.iter().flat_map(|c| c.ground.iter().zip(&c.feature));
        let (mut water, mut rocks, mut rock_on_water, mut n) = (0, 0, 0, 0);
        for (g, f) in cells {
            n += 1;
            water += usize::from(*g == Ground::Water);
            rocks += usize::from(*f == Feature::Rock);
            rock_on_water += usize::from(*g == Ground::Water && *f == Feature::Rock);
        }
        assert!(water > 0 && water < n);
        assert!(rocks > 0 && rock_on_water > 0);
    }

    #[test]
    fn noise_stays_in_unit_range() {
        for y in -64..64 {
            for x in -64..64 {
                let n = fbm2(3, STREAM_GROUND, x as f32 / 5.0, y as f32 / 5.0);
                assert!((0.0..=1.0).contains(&n), "{n}");
            }
        }
    }
}
