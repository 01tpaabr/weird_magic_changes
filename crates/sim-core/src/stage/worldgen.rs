//! Deterministic chunk generation.
//!
//! Every cell is a **pure function of `(seed, x, y)`**: ground comes from
//! thresholded value noise, rocks from a per-cell hash, and so is every
//! actor row worldgen places (a `seed` on a walkable cell when its hash
//! falls under `seed_density`, a `chicken` under `animal_density`, with a
//! `uid` hashed from its position; a kind the rule set lacks is not placed). No
//! sequential state, so a chunk's content never depends on when, in which
//! order, or on how many threads it was generated. Rows are pushed in cell
//! order, so slot order is pure too. What is *not* pure is the tick a row
//! was born at: `sim::load_chunks` stamps it, and marks an inhabited chunk
//! dirty so it is saved rather than regenerated.

use super::{CHUNK_CELLS, ChunkCoord, ChunkData, Feature, Ground};
use crate::actors::ActorMind;
use crate::par::par_zip_mut;
use crate::rng::{hash_cell, unit_f32};
use crate::rules::Kinds;
use bytemuck::Zeroable;

/// Hash streams used by generation. Never reuse a value elsewhere.
pub const STREAM_GROUND: u64 = 0x0001;
pub const STREAM_ROCK: u64 = 0x0002;
pub const STREAM_SEED: u64 = 0x0003;
pub const STREAM_UID: u64 = 0x0004;
pub const STREAM_ANIMAL: u64 = 0x0005;

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
    /// Probability that a walkable cell starts with a seed on it.
    pub seed_density: f32,
    /// Probability that a walkable cell without a seed starts with a chicken.
    pub animal_density: f32,
}

impl Default for GenParams {
    fn default() -> Self {
        Self {
            water_scale: 12.0,
            water_level: 0.30,
            rock_on_soil: 0.04,
            rock_on_water: 0.01,
            seed_density: 0.01,
            animal_density: 0.002,
        }
    }
}

/// Fill `out` with chunk `coord`: cells, then the rows worldgen places on
/// them. Sequential inside the chunk; callers parallelise across chunks
/// (see [`generate_many`]). Rows come out with `born` and `last_think` zero.
pub fn generate_chunk(
    seed: u64,
    params: &GenParams,
    kinds: &Kinds,
    coord: ChunkCoord,
    out: &mut ChunkData,
) {
    let cells = &mut out.cells;
    for (i, (g, f)) in cells
        .ground
        .iter_mut()
        .zip(cells.feature.iter_mut())
        .enumerate()
    {
        let p = coord.cell(i);
        (*g, *f) = gen_cell(seed, params, p.x, p.y);
    }
    cells.occupant = [super::ActorId::NONE; CHUNK_CELLS];
    out.actors.rows.clear();
    out.minds.rows.clear();
    let seed_kind = kinds.by_name("seed").map(|k| k.id);
    let animal_kind = kinds.by_name("chicken").map(|k| k.id);
    for i in 0..CHUNK_CELLS {
        let p = coord.cell(i);
        if !out.cells.walkable(i) {
            continue;
        }
        let kind = if seed_here(seed, params, p.x, p.y) {
            seed_kind
        } else if animal_here(seed, params, p.x, p.y) {
            animal_kind
        } else {
            None
        };
        if let Some(kind) = kind {
            let mind = ActorMind {
                uid: hash_cell(seed, STREAM_UID, p.x, p.y),
                ..ActorMind::zeroed()
            };
            out.actors_mut().push(i, kind, mind);
        }
    }
}

/// Generate many chunks in parallel, results in input order. Each task
/// writes its chunks straight into their final slots (disjoint slices of
/// the output), so nothing is copied afterwards.
pub fn generate_many(
    seed: u64,
    params: &GenParams,
    kinds: &Kinds,
    coords: &[ChunkCoord],
) -> Vec<ChunkData> {
    let mut out = vec![ChunkData::default(); coords.len()];
    par_zip_mut(coords, &mut out, GEN_BATCH, |&c, data| {
        generate_chunk(seed, params, kinds, c, data);
    });
    out
}

/// Does a seed start on this (walkable) cell? Pure.
#[inline]
fn seed_here(seed: u64, p: &GenParams, x: i32, y: i32) -> bool {
    unit_f32(hash_cell(seed, STREAM_SEED, x, y)) < p.seed_density
}

/// Does a chicken start on this (walkable, seedless) cell? Pure.
#[inline]
fn animal_here(seed: u64, p: &GenParams, x: i32, y: i32) -> bool {
    unit_f32(hash_cell(seed, STREAM_ANIMAL, x, y)) < p.animal_density
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
    use crate::actors::ActorMind;
    use crate::rules::{CHICKEN, SEED};
    use crate::stage::{ActorId, Pos};
    use bytemuck::Zeroable;

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
        let kinds = Kinds::builtin();
        let coords = grid(3);
        let par: Vec<u64> = generate_many(42, &p, &kinds, &coords)
            .iter()
            .map(ChunkData::hash)
            .collect();
        let serial: Vec<u64> = coords
            .iter()
            .map(|&c| {
                let mut data = ChunkData::default();
                generate_chunk(42, &p, &kinds, c, &mut data);
                data.hash()
            })
            .collect();
        assert_eq!(par, serial);
    }

    #[test]
    fn chunk_matches_per_cell_rule_and_is_order_free() {
        let p = GenParams::default();
        let kinds = Kinds::builtin();
        let coord = ChunkCoord::new(-2, 3);
        let mut data = ChunkData::default();
        generate_chunk(7, &p, &kinds, coord, &mut data);
        let cells = &data.cells;
        let mut expect_rows = 0;
        let mut chickens = 0;
        for i in 0..CHUNK_CELLS {
            let Pos { x, y } = coord.cell(i);
            assert_eq!(
                (cells.ground[i], cells.feature[i]),
                gen_cell(7, &p, x, y),
                "{x},{y}"
            );
            let seeded = cells.walkable(i) && seed_here(7, &p, x, y);
            let animal = cells.walkable(i) && !seeded && animal_here(7, &p, x, y);
            assert_eq!(!cells.occupant[i].is_none(), seeded || animal, "{x},{y}");
            if seeded || animal {
                let (kind, slot) = cells.occupant[i].unpack().unwrap();
                assert_eq!(kind, if seeded { SEED } else { CHICKEN });
                chickens += usize::from(animal);
                assert_eq!(usize::from(slot), expect_rows, "rows are in cell order");
                let mind = data.minds.rows[expect_rows];
                assert_eq!(mind.uid, hash_cell(7, STREAM_UID, x, y));
                assert_eq!((mind.born, mind.last_think), (0, 0));
                expect_rows += 1;
            }
        }
        assert!(
            expect_rows > chickens && chickens > 0,
            "default densities place seeds and chickens"
        );
        assert_eq!(data.validate(kinds.len()), Ok(()));
        // Regenerating into a dirty buffer gives the same bytes.
        let mut again = ChunkData::default();
        again.cells.occupant[3] = ActorId(1);
        again.actors_mut().push(9, SEED, ActorMind::zeroed());
        generate_chunk(7, &p, &kinds, coord, &mut again);
        assert_eq!(data.hash(), again.hash());
        assert_eq!(data, again);
        // Density 0 is a stage with nobody on it; a rule set without the
        // kind places none of it either.
        let bare = GenParams {
            seed_density: 0.0,
            animal_density: 0.0,
            ..p
        };
        let mut empty = ChunkData::default();
        generate_chunk(7, &bare, &kinds, coord, &mut empty);
        assert!(empty.actors.rows.is_empty());
        let plants_only = crate::rules::compile("p", crate::rules::builtin::FILES[1].1).unwrap();
        let mut no_animals = ChunkData::default();
        generate_chunk(7, &p, &plants_only, coord, &mut no_animals);
        assert_eq!(no_animals.actors.rows.len(), expect_rows - chickens);
        assert_eq!(empty.cells.ground, data.cells.ground);
    }

    #[test]
    fn different_seeds_differ_and_have_all_tile_kinds() {
        crate::par::init_task_pool();
        let p = GenParams::default();
        let kinds = Kinds::builtin();
        let a = generate_many(1, &p, &kinds, &grid(2));
        let b = generate_many(2, &p, &kinds, &grid(2));
        assert_ne!(a[0].hash(), b[0].hash());
        let cells = a
            .iter()
            .flat_map(|c| c.cells.ground.iter().zip(&c.cells.feature));
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
