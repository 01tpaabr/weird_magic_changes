//! The Stage: a fixed `width x height` grid of cells that actors stand on.
//!
//! Layout: **structure of arrays, row-major.** Cell `(x, y)` lives at index
//! `y * width + x` in every layer. Layers are independent flat `Vec`s so a
//! system that only cares about ground never touches occupancy memory, and so
//! any layer can be handed to a Zig kernel as a plain byte slice.
//!
//! Parallel work is split into **row bands** of [`CHUNK_ROWS`] rows
//! (`Stage::chunk_len` cells). Bands are contiguous in memory (so
//! `par_chunks_mut(stage.chunk_len())` works on any layer directly) and their
//! boundaries fall on row boundaries (so a 2D neighbour system needs exactly a
//! one-row halo above and below). Chunk `i` always covers the same rows
//! regardless of thread count.
//!
//! Layers today:
//! - `ground`: what the cell *is* ([`Ground`]).
//! - `feature`: what sits *on* the ground but is not an actor ([`Feature`]).
//! - `occupant`: which actor stands here, at most one ([`ActorId`]).
//!
//! Adding a per-cell scalar (moisture, heat, mana): add a `Vec<T>` here, size
//! it in `Stage::new`, fold it into `checksum`, render it in `app`. Nothing
//! else changes.

pub mod worldgen;

use bytemuck::{NoUninit, Pod, Zeroable};
use rayon::prelude::*;

use crate::rng::splitmix64;

/// Rows per parallel work unit. 16 rows x 1024 cols x 1 byte = 16 KiB per
/// layer per chunk: comfortably in L1 for a few layers at once. Tune with
/// `make bench`; changing it must not change any result.
pub const CHUNK_ROWS: u32 = 16;

/// What a cell fundamentally is. Exactly one per cell.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, NoUninit)]
pub enum Ground {
    #[default]
    Soil = 0,
    Water = 1,
}

impl Ground {
    /// Can an unaided actor stand on this ground?
    #[inline]
    pub fn walkable(self) -> bool {
        match self {
            Ground::Soil => true,
            Ground::Water => false,
        }
    }
}

/// Something resting on the ground that is not an actor. At most one per cell.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, NoUninit)]
pub enum Feature {
    #[default]
    None = 0,
    Rock = 1,
}

impl Feature {
    /// Does this feature stop an actor from entering the cell?
    #[inline]
    pub fn blocks(self) -> bool {
        match self {
            Feature::None => false,
            Feature::Rock => true,
        }
    }
}

/// Dense actor id. `ActorId::NONE` marks an empty cell in the occupancy layer.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Pod, Zeroable)]
pub struct ActorId(pub u32);

impl ActorId {
    pub const NONE: ActorId = ActorId(u32::MAX);

    #[inline]
    pub fn is_none(self) -> bool {
        self == Self::NONE
    }
}

impl Default for ActorId {
    fn default() -> Self {
        Self::NONE
    }
}

/// Flat index of a cell: `y * width + x`. Only meaningful for the stage that
/// produced it.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Pod, Zeroable)]
pub struct CellIdx(pub u32);

impl CellIdx {
    #[inline]
    pub fn usize(self) -> usize {
        self.0 as usize
    }
}

/// Grid coordinate. `(0, 0)` is the top-left corner; `y` grows downward.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Pod, Zeroable)]
pub struct Pos {
    pub x: u32,
    pub y: u32,
}

impl Pos {
    #[inline]
    pub const fn new(x: u32, y: u32) -> Self {
        Self { x, y }
    }
}

/// The grid. Fields are `pub` on purpose: systems slice them directly.
#[derive(Debug, Clone)]
pub struct Stage {
    width: u32,
    height: u32,
    pub ground: Vec<Ground>,
    pub feature: Vec<Feature>,
    pub occupant: Vec<ActorId>,
}

impl Stage {
    /// An all-soil, empty stage. For a generated one see [`worldgen::generate`].
    pub fn new(width: u32, height: u32) -> Self {
        assert!(width > 0 && height > 0, "stage must be non-empty");
        let n = width as usize * height as usize;
        assert!(
            n <= u32::MAX as usize,
            "stage too large for u32 cell indices"
        );
        Self {
            width,
            height,
            ground: vec![Ground::Soil; n],
            feature: vec![Feature::None; n],
            occupant: vec![ActorId::NONE; n],
        }
    }

    #[inline]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[inline]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Number of cells.
    #[inline]
    pub fn len(&self) -> usize {
        self.ground.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ground.is_empty()
    }

    // ---- chunking ------------------------------------------------------------

    /// Cells per parallel work unit (a band of [`CHUNK_ROWS`] rows). Use as the
    /// argument to `par_chunks` / `par_chunks_mut` on any layer.
    #[inline]
    pub fn chunk_len(&self) -> usize {
        self.width as usize * CHUNK_ROWS as usize
    }

    /// Number of chunks (the last one may be shorter).
    #[inline]
    pub fn chunk_count(&self) -> usize {
        self.height.div_ceil(CHUNK_ROWS) as usize
    }

    /// First row covered by chunk `chunk`.
    #[inline]
    pub fn chunk_first_row(&self, chunk: usize) -> u32 {
        u32::try_from(chunk).expect("chunk index fits u32") * CHUNK_ROWS
    }

    // ---- coordinates -----------------------------------------------------------

    /// Index of an in-bounds position. Debug-asserts bounds; hot loops should
    /// stay in index space and never call this per cell.
    #[inline]
    pub fn idx(&self, p: Pos) -> CellIdx {
        debug_assert!(p.x < self.width && p.y < self.height, "{p:?} out of bounds");
        CellIdx(p.y * self.width + p.x)
    }

    #[inline]
    pub fn pos(&self, c: CellIdx) -> Pos {
        Pos::new(c.0 % self.width, c.0 / self.width)
    }

    #[inline]
    pub fn contains(&self, p: Pos) -> bool {
        p.x < self.width && p.y < self.height
    }

    /// `p + (dx, dy)` if it stays on the stage.
    #[inline]
    pub fn offset(&self, p: Pos, dx: i32, dy: i32) -> Option<Pos> {
        let x = p.x.checked_add_signed(dx)?;
        let y = p.y.checked_add_signed(dy)?;
        (x < self.width && y < self.height).then_some(Pos::new(x, y))
    }

    /// Von Neumann neighbours (N, E, S, W order), clipped at the edges.
    /// Allocation-free; the order is fixed so callers stay deterministic.
    pub fn neighbors4(&self, p: Pos) -> impl Iterator<Item = Pos> + '_ {
        const D: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];
        D.into_iter()
            .filter_map(move |(dx, dy)| self.offset(p, dx, dy))
    }

    // ---- queries -----------------------------------------------------------------

    /// Terrain permits standing here (ignores occupants).
    #[inline]
    pub fn walkable(&self, c: CellIdx) -> bool {
        let i = c.usize();
        self.ground[i].walkable() && !self.feature[i].blocks()
    }

    /// Terrain permits standing here and nobody is here.
    #[inline]
    pub fn free(&self, c: CellIdx) -> bool {
        self.walkable(c) && self.occupant[c.usize()].is_none()
    }

    // ---- integrity ----------------------------------------------------------------

    /// Order-independent-of-threads, order-dependent-on-data checksum of every
    /// layer. Hashed per chunk in parallel, combined sequentially in chunk order.
    pub fn checksum(&self) -> u64 {
        let n = self.chunk_len();
        let g = self
            .ground
            .par_chunks(n)
            .map(|c| fnv1a(bytemuck::cast_slice(c)));
        let f = self
            .feature
            .par_chunks(n)
            .map(|c| fnv1a(bytemuck::cast_slice(c)));
        let o = self
            .occupant
            .par_chunks(n)
            .map(|c| fnv1a(bytemuck::cast_slice(c)));
        let partial: Vec<u64> = g.chain(f).chain(o).collect();
        partial.iter().fold(
            splitmix64(u64::from(self.width) << 32 | u64::from(self.height)),
            |acc, &h| splitmix64(acc ^ h),
        )
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xCBF2_9CE4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01B3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_roundtrip_and_bounds() {
        let s = Stage::new(7, 5);
        for y in 0..5 {
            for x in 0..7 {
                let p = Pos::new(x, y);
                assert_eq!(s.pos(s.idx(p)), p);
                assert!(s.contains(p));
            }
        }
        assert!(!s.contains(Pos::new(7, 0)));
        assert!(!s.contains(Pos::new(0, 5)));
        assert_eq!(s.offset(Pos::new(0, 0), -1, 0), None);
        assert_eq!(s.offset(Pos::new(6, 4), 1, 0), None);
        assert_eq!(s.offset(Pos::new(3, 3), -1, 1), Some(Pos::new(2, 4)));
    }

    #[test]
    fn neighbors_are_clipped_and_ordered() {
        let s = Stage::new(3, 3);
        let corner: Vec<_> = s.neighbors4(Pos::new(0, 0)).collect();
        assert_eq!(corner, vec![Pos::new(1, 0), Pos::new(0, 1)]);
        let mid: Vec<_> = s.neighbors4(Pos::new(1, 1)).collect();
        assert_eq!(
            mid,
            vec![
                Pos::new(1, 0),
                Pos::new(2, 1),
                Pos::new(1, 2),
                Pos::new(0, 1)
            ]
        );
    }

    #[test]
    fn chunks_tile_the_stage_exactly() {
        for h in [
            1,
            CHUNK_ROWS - 1,
            CHUNK_ROWS,
            CHUNK_ROWS + 1,
            3 * CHUNK_ROWS + 5,
        ] {
            let s = Stage::new(10, h);
            let n = s.ground.chunks(s.chunk_len()).count();
            assert_eq!(n, s.chunk_count());
            assert!(s.chunk_first_row(n - 1) < h);
        }
    }

    #[test]
    fn walkability_rules() {
        let mut s = Stage::new(2, 2);
        let c = s.idx(Pos::new(0, 0));
        assert!(s.free(c));
        s.feature[c.usize()] = Feature::Rock;
        assert!(!s.walkable(c));
        s.feature[c.usize()] = Feature::None;
        s.ground[c.usize()] = Ground::Water;
        assert!(!s.walkable(c));
        s.ground[c.usize()] = Ground::Soil;
        s.occupant[c.usize()] = ActorId(3);
        assert!(s.walkable(c) && !s.free(c));
    }

    #[test]
    fn checksum_sees_every_layer() {
        let base = Stage::new(20, 40);
        let h0 = base.checksum();
        let mut s = base.clone();
        s.ground[777] = Ground::Water;
        assert_ne!(s.checksum(), h0);
        let mut s = base.clone();
        s.feature[777] = Feature::Rock;
        assert_ne!(s.checksum(), h0);
        let mut s = base.clone();
        s.occupant[777] = ActorId(0);
        assert_ne!(s.checksum(), h0);
        assert_ne!(Stage::new(40, 20).checksum(), h0);
    }
}
