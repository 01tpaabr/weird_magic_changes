//! Deterministic chunk generation.
//!
//! Every cell is a **pure function of `(seed, x, y)`** and the scenario:
//! ground comes from thresholded value noise, rocks from a per-cell hash,
//! both under the scenario's drawn map where it has one ([`Terrain`]), and
//! so is every actor row worldgen places. A scenario's explicit start
//! takes its cell; every other walkable cell draws one placement hash,
//! and the scenario's `start K n / d` shares cut `0..PLACE_ONE` into one
//! interval per kind, in the order written ([`Placement::placed`]). A row's `uid`
//! is hashed from its position. No sequential state, so a chunk's content
//! never depends on when, in which order, or on how many threads it was
//! generated. Rows are pushed in cell order, so slot order is pure too.
//! What is *not* pure is the tick a row was born at: `sim::load_chunks`
//! stamps it, and marks an inhabited chunk dirty so it is saved rather
//! than regenerated.

use super::{CHUNK_CELLS, ChunkCoord, ChunkData, Feature, Ground};
use crate::actors::ActorMind;
use crate::par::par_zip_mut;
use crate::rng::{hash_cell, unit_f32};
use crate::scenario::{DrawnMap, Placed, Placement};
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

/// A world's ground: its seed's noise, under a scenario's drawn map where
/// it has one. Pure.
#[derive(Debug, Clone, Copy)]
pub struct Terrain<'a> {
    pub seed: u64,
    pub params: &'a GenParams,
    pub map: Option<&'a DrawnMap>,
}

impl Terrain<'_> {
    /// The ground and feature of cell `(x, y)`.
    #[inline]
    pub fn cell(&self, x: i32, y: i32) -> (Ground, Feature) {
        match self.map.and_then(|m| m.at(x, y)) {
            Some(c) => c,
            None => gen_cell(self.seed, self.params, x, y),
        }
    }
}

/// Fill `out` with chunk `coord`: cells, then the rows `placement` puts on
/// them. Sequential inside the chunk; callers parallelise across chunks
/// (see [`generate_many`]). Rows come out with `born` and `last_think` zero.
pub fn generate_chunk(
    terrain: &Terrain,
    placement: &Placement,
    coord: ChunkCoord,
    out: &mut ChunkData,
) {
    let seed = terrain.seed;
    let cells = &mut out.cells;
    let ground = cells.ground.iter_mut().zip(cells.feature.iter_mut());
    // No map: the noise alone, without asking a map about every cell.
    match terrain.map {
        None => {
            for (i, (g, f)) in ground.enumerate() {
                let p = coord.cell(i);
                (*g, *f) = gen_cell(seed, terrain.params, p.x, p.y);
            }
        }
        Some(_) => {
            for (i, (g, f)) in ground.enumerate() {
                let p = coord.cell(i);
                (*g, *f) = terrain.cell(p.x, p.y);
            }
        }
    }
    cells.occupant = [super::ActorId::NONE; CHUNK_CELLS];
    cells.cover = [super::ActorId::NONE; CHUNK_CELLS];
    cells.scent = [[0; CHUNK_CELLS]; super::SCENT_CHANNELS];
    out.actors.rows.clear();
    out.minds.rows.clear();
    let explicit = placement.explicit_in(coord);
    if explicit.is_empty() && !placement.has_shares() {
        return;
    }
    // Explicit starts are sorted by cell: merge them into the cell walk.
    let mut explicit = explicit.iter().peekable();
    for i in 0..CHUNK_CELLS {
        let p = coord.cell(i);
        let here = match explicit.next_if(|e| usize::from(e.cell) == i) {
            Some(e) => Some(e.placed),
            None if out.cells.walkable(i) => placement.placed(placement_hash(seed, p.x, p.y)),
            None => None,
        };
        if let Some(Placed { kind, cover }) = here {
            let mind = ActorMind {
                uid: hash_cell(seed, STREAM_UID, p.x, p.y),
                ..ActorMind::zeroed()
            };
            if cover {
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
    terrain: &Terrain,
    placement: &Placement,
    coords: &[ChunkCoord],
) -> Vec<ChunkData> {
    let mut out = vec![ChunkData::default(); coords.len()];
    par_zip_mut(coords, &mut out, GEN_BATCH, |&c, data| {
        generate_chunk(terrain, placement, c, data);
    });
    out
}

/// Chunks per generation task. Measured (`make bench`, `generate_many`):
/// 1 and 4 within noise of each other at 32x32 chunks; 1 keeps the small
/// streaming batches (5 chunks) fully parallel.
const GEN_BATCH: usize = 1;

/// The terrain of one cell, from the seed's noise alone. Pure.
#[inline]
pub fn gen_cell(seed: u64, p: &GenParams, x: i32, y: i32) -> (Ground, Feature) {
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
    use crate::rules::{CHICKEN, GRASS, Kinds, SEED};
    use crate::scenario::{Scenario, Start};
    use crate::stage::{ActorId, Pos};
    use bytemuck::Zeroable;

    const P: GenParams = GenParams {
        water_scale: 12.0,
        water_level: 0.30,
        rock_on_soil: 0.04,
        rock_on_water: 0.01,
    };

    /// The seed's noise, no map.
    fn noise(seed: u64) -> Terrain<'static> {
        Terrain {
            seed,
            params: &P,
            map: None,
        }
    }

    /// The built-in scenario's shares against the built-in rules.
    fn builtin(seed: u64) -> Placement {
        let s = Scenario::builtin();
        Placement::resolve(&s.starts, &Kinds::builtin(), &noise(seed)).unwrap()
    }

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
        let pl = builtin(42);
        let coords = grid(3);
        let par: Vec<u64> = generate_many(&noise(42), &pl, &coords)
            .iter()
            .map(ChunkData::hash)
            .collect();
        let serial: Vec<u64> = coords
            .iter()
            .map(|&c| {
                let mut data = ChunkData::default();
                generate_chunk(&noise(42), &pl, c, &mut data);
                data.hash()
            })
            .collect();
        assert_eq!(par, serial);
    }

    #[test]
    fn chunk_matches_per_cell_rule_and_is_order_free() {
        let p = GenParams::default();
        let kinds = Kinds::builtin();
        let pl = builtin(7);
        let coord = ChunkCoord::new(-2, 3);
        let mut data = ChunkData::default();
        generate_chunk(&noise(7), &pl, coord, &mut data);
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
                pl.placed(placement_hash(7, x, y)).map(|p| p.kind)
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
        generate_chunk(&noise(7), &pl, coord, &mut again);
        assert_eq!(data.hash(), again.hash());
        assert_eq!(data, again);
        // A scenario that starts nobody leaves the same terrain, bare.
        let mut empty = ChunkData::default();
        generate_chunk(&noise(7), &Placement::default(), coord, &mut empty);
        assert!(empty.actors.rows.is_empty());
        assert_eq!(empty.cells.ground, data.cells.ground);
    }

    /// An explicit start takes its cell (whatever the shares would have put
    /// there), in its kind's layer, and rows stay in cell order.
    #[test]
    fn explicit_starts_take_their_cell_in_cell_order() {
        let kinds = Kinds::builtin();
        let coord = ChunkCoord::new(1, 0);
        let mut shares_only = ChunkData::default();
        generate_chunk(&noise(7), &builtin(7), coord, &mut shares_only);
        // A cell the shares fill with a seed, and an empty walkable one.
        let seeded = (0..CHUNK_CELLS)
            .find(|&i| {
                shares_only.cells.occupant[i]
                    .unpack()
                    .is_some_and(|(k, _)| k == SEED)
            })
            .unwrap();
        let empty = (0..CHUNK_CELLS)
            .rev()
            .find(|&i| {
                shares_only.cells.walkable(i)
                    && shares_only.cells.occupant[i].is_none()
                    && shares_only.cells.cover[i].is_none()
            })
            .unwrap();
        let at = |kind: &str, i: usize| {
            let q = coord.cell(i);
            Start::at(kind, q.x, q.y)
        };
        let mut starts = Scenario::builtin().starts;
        starts.extend([at("grass", seeded), at("chicken", empty)]);
        let pl = Placement::resolve(&starts, &kinds, &noise(7)).unwrap();
        let mut data = ChunkData::default();
        generate_chunk(&noise(7), &pl, coord, &mut data);
        assert!(data.cells.occupant[seeded].is_none(), "the seed gave way");
        assert_eq!(
            data.cells.cover[seeded].unpack().map(|(k, _)| k),
            Some(GRASS)
        );
        assert_eq!(
            data.cells.occupant[empty].unpack().map(|(k, _)| k),
            Some(CHICKEN)
        );
        assert_eq!(data.actors.rows.len(), shares_only.actors.rows.len() + 1);
        let cells: Vec<u16> = data.actors.rows.iter().map(|r| r.cell).collect();
        assert!(cells.is_sorted(), "rows in cell order");
        let q = coord.cell(empty);
        let slot = data.cells.occupant[empty].unpack().unwrap().1;
        assert_eq!(
            data.minds.rows[usize::from(slot)].uid,
            hash_cell(7, STREAM_UID, q.x, q.y)
        );
        assert_eq!(data.validate(kinds.len()), Ok(()));
    }

    #[test]
    fn different_seeds_differ_and_have_all_tile_kinds() {
        crate::par::init_task_pool();
        let a = generate_many(&noise(1), &builtin(1), &grid(2));
        let b = generate_many(&noise(2), &builtin(2), &grid(2));
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

    /// A drawn map's cells replace the noise inside it and its `outside`
    /// fill beyond it; its kinds stand where they are drawn.
    #[test]
    fn a_drawn_map_lays_its_cells_and_starts() {
        assert_eq!(P, GenParams::default());
        let text = |outside: &str| {
            format!(
                "seed 7\n{outside}\nmap {{\n.#C\n~.'\n}}\nlegend {{\n  . soil\n  # rock\n  ~ water\n  C chicken\n  ' grass\n}}\n"
            )
        };
        let s = Scenario::parse("t", &text("outside water")).unwrap();
        let pl = Placement::resolve(&s.starts, &Kinds::builtin(), &s.terrain()).unwrap();
        let mut data = ChunkData::default();
        generate_chunk(&s.terrain(), &pl, ChunkCoord::new(0, 0), &mut data);
        let c = &data.cells;
        assert_eq!((c.ground[0], c.feature[0]), (Ground::Soil, Feature::None));
        assert_eq!(c.feature[1], Feature::Rock);
        assert_eq!(c.occupant[2].unpack().map(|(k, _)| k), Some(CHICKEN));
        assert_eq!(c.ground[64], Ground::Water);
        assert_eq!(c.cover[66].unpack().map(|(k, _)| k), Some(GRASS));
        assert_eq!(
            (c.ground[3], c.ground[64 * 63]),
            (Ground::Water, Ground::Water),
            "outside: water"
        );
        assert_eq!(data.actors.rows.len(), 2, "no shares: only what is drawn");
        // Without `outside`, the seed's noise beyond the map.
        let s = Scenario::parse("t", &text("")).unwrap();
        let pl = Placement::resolve(&s.starts, &Kinds::builtin(), &s.terrain()).unwrap();
        let mut noisy = ChunkData::default();
        generate_chunk(&s.terrain(), &pl, ChunkCoord::new(0, 0), &mut noisy);
        for i in [3, 64 * 63, 4095] {
            let q = ChunkCoord::new(0, 0).cell(i);
            let cell = (noisy.cells.ground[i], noisy.cells.feature[i]);
            assert_eq!(cell, gen_cell(7, &P, q.x, q.y));
        }
        assert_eq!(
            noisy.cells.feature[1],
            Feature::Rock,
            "the map still wins inside"
        );
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
