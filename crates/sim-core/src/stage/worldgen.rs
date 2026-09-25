//! Deterministic chunk generation.
//!
//! Every cell is a **pure function of `(seed, x, y)`**: ground comes from
//! thresholded value noise, rocks from a per-cell hash, and so is every
//! actor row worldgen places: each walkable cell draws one placement hash,
//! and the rules' `place N / D` shares cut `0..PLACE_ONE` into one interval
//! per kind, in kind order (`Kinds::placed`); the row's `uid` is hashed
//! from its position. No
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
// 0x0003 and 0x0005 were per-kind placement streams; retired, never reuse.
pub const STREAM_UID: u64 = 0x0004;
pub const STREAM_PLACE: u64 = 0x0006;

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
    cells.cover = [super::ActorId::NONE; CHUNK_CELLS];
    cells.scent = [[0; CHUNK_CELLS]; super::SCENT_CHANNELS];
    out.actors.rows.clear();
    out.minds.rows.clear();
    if !kinds.places_any() {
        return;
    }
    for i in 0..CHUNK_CELLS {
        if !out.cells.walkable(i) {
            continue;
        }
        let p = coord.cell(i);
        if let Some(kind) = kinds.placed(placement_hash(seed, p.x, p.y)) {
            let mind = ActorMind {
                uid: hash_cell(seed, STREAM_UID, p.x, p.y),
                ..ActorMind::zeroed()
            };
            if kinds.def(kind).cover {
                out.actors_mut().push_cover(i, kind, mind);
            } else {
                out.actors_mut().push(i, kind, mind);
            }
        }
    }
}

/// A cell's placement draw in `0..PLACE_ONE` (the top 24 bits). Pure.
#[inline]
pub fn placement_hash(seed: u64, x: i32, y: i32) -> u32 {
    (hash_cell(seed, STREAM_PLACE, x, y) >> 40) as u32
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
        let mut rows = 0;
        let mut per_kind = vec![0usize; kinds.len()];
        for i in 0..CHUNK_CELLS {
            let Pos { x, y } = coord.cell(i);
            assert_eq!(
                (cells.ground[i], cells.feature[i]),
                gen_cell(7, &p, x, y),
                "{x},{y}"
            );
            let want = if cells.walkable(i) {
                kinds.placed(placement_hash(7, x, y))
            } else {
                None
            };
            let here = if cells.cover[i].is_none() {
                cells.occupant[i]
            } else {
                cells.cover[i]
            };
            assert_eq!(here.unpack().map(|(k, _)| k), want, "{x},{y}");
            if let Some((kind, slot)) = here.unpack() {
                per_kind[usize::from(kind)] += 1;
                assert_eq!(usize::from(slot), rows, "rows are in cell order");
                let mind = data.minds.rows[rows];
                assert_eq!(mind.uid, hash_cell(7, STREAM_UID, x, y));
                assert_eq!((mind.born, mind.last_think), (0, 0));
                rows += 1;
            }
        }
        assert!(
            per_kind[usize::from(SEED)] > per_kind[usize::from(CHICKEN)]
                && per_kind[usize::from(CHICKEN)] > 0,
            "the built-in shares place seeds and chickens: {per_kind:?}"
        );
        assert_eq!(
            per_kind[usize::from(crate::rules::TREE)],
            0,
            "trees are grown, not placed"
        );
        assert_eq!(data.validate(kinds.len()), Ok(()));
        // Regenerating into a dirty buffer gives the same bytes.
        let mut again = ChunkData::default();
        again.cells.occupant[3] = ActorId(1);
        again.actors_mut().push(9, SEED, ActorMind::zeroed());
        generate_chunk(7, &p, &kinds, coord, &mut again);
        assert_eq!(data.hash(), again.hash());
        assert_eq!(data, again);
        // Rules that place nobody leave the same terrain, bare.
        let mut empty = ChunkData::default();
        generate_chunk(7, &p, &kinds.clone().without_placement(), coord, &mut empty);
        assert!(empty.actors.rows.is_empty());
        assert_eq!(empty.cells.ground, data.cells.ground);
    }

    #[test]
    fn placement_shares_split_the_range_in_kind_order() {
        let k = crate::rules::compile(
            "t",
            "kind a { place 1 / 4 } kind b { } kind c { place 1 / 2 }",
        )
        .unwrap();
        let one = crate::rules::PLACE_ONE;
        assert_eq!(k.placed(0), Some(0));
        assert_eq!(k.placed(one / 4 - 1), Some(0));
        assert_eq!(k.placed(one / 4), Some(2));
        assert_eq!(k.placed(one * 3 / 4 - 1), Some(2));
        assert_eq!(k.placed(one * 3 / 4), None);
        assert!(
            crate::rules::compile("t", "kind a { place 3 / 4 } kind b { place 1 / 2 }").is_err()
        );
        assert!(crate::rules::compile("t", "kind a { place 5 / 4 }").is_err());
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
