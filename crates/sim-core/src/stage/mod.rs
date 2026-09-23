//! The Stage: an unbounded 2D grid of cells that actors stand on, stored as
//! fixed-size **chunks**.
//!
//! ```text
//! world cell (x, y): i32          chunk coord = (x >> CHUNK_BITS, y >> CHUNK_BITS)
//!                                 local index = (y & MASK) * CHUNK_SIZE + (x & MASK)
//! ```
//!
//! A chunk is [`CHUNK_SIZE`]² cells with one contiguous array per layer
//! ([`ChunkCells`]). It is, at the same time:
//! - the **parallel work unit**: a phase is `cells.par_iter_mut()` over the slab;
//! - the **streaming unit**: loaded around the camera, unloaded far away;
//! - the **save unit**: one file per modified chunk (see `crate::store`).
//!
//! Loaded chunks live in a slab (`Vec`) addressed by slot. Slot numbers depend
//! on load history, so **nothing observable may depend on slot order**: every
//! sequential merge and the checksum walk [`Stage::active`], which is kept
//! sorted by chunk coordinate. Per-chunk metadata ([`ChunkMeta`]) is a
//! separate slab from the cell data so a later double-buffered phase can read
//! all of `cells` immutably while writing a `cells_next` slab.
//!
//! Layers today:
//! - `ground`: what the cell *is* ([`Ground`]).
//! - `feature`: what sits *on* the ground but is not an actor ([`Feature`]).
//! - `occupant`: which actor stands here, at most one ([`ActorId`]).
//!
//! Adding a per-cell scalar (moisture, heat, mana): add an array to
//! [`ChunkCells`], fold it into `hash`, encode it in `store`, render it in `app`.

pub mod worldgen;

use std::collections::HashMap;

use bytemuck::{CheckedBitPattern, NoUninit, Pod, Zeroable};
use rayon::prelude::*;

use crate::rng::splitmix64;

/// log2 of the chunk side. 64² = 4096 cells: 4 KiB per byte layer, 16 KiB
/// for the occupant layer, one rayon task. Tune with `make bench`; changing
/// it changes save files (bump `store::FORMAT_VERSION`) but never sim results.
pub const CHUNK_BITS: u32 = 6;
/// Chunk side length in cells.
pub const CHUNK_SIZE: i32 = 1 << CHUNK_BITS;
/// Cells per chunk.
pub const CHUNK_CELLS: usize = (CHUNK_SIZE * CHUNK_SIZE) as usize;
const MASK: i32 = CHUNK_SIZE - 1;

/// What a cell fundamentally is. Exactly one per cell.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, NoUninit, CheckedBitPattern)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, NoUninit, CheckedBitPattern)]
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

/// World cell coordinate. Unbounded; `y` grows downward. The initial map
/// occupies `[0, w) x [0, h)`, everything else is generated on demand.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Pod, Zeroable)]
pub struct Pos {
    pub x: i32,
    pub y: i32,
}

impl Pos {
    #[inline]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// `self + (dx, dy)`, `None` only on i32 overflow (the edge of the world).
    #[inline]
    pub fn offset(self, dx: i32, dy: i32) -> Option<Pos> {
        Some(Pos::new(self.x.checked_add(dx)?, self.y.checked_add(dy)?))
    }

    /// Von Neumann neighbours in fixed N, E, S, W order.
    pub fn neighbors4(self) -> impl Iterator<Item = Pos> {
        const D: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];
        D.into_iter()
            .filter_map(move |(dx, dy)| self.offset(dx, dy))
    }

    /// Chunk containing this cell and the cell's index inside it.
    #[inline]
    pub const fn split(self) -> (ChunkCoord, usize) {
        let cc = ChunkCoord::new(self.x >> CHUNK_BITS, self.y >> CHUNK_BITS);
        let local = ((self.y & MASK) * CHUNK_SIZE + (self.x & MASK)) as usize;
        (cc, local)
    }
}

/// Chunk coordinate = cell coordinate `>> CHUNK_BITS`. Ordered row-major
/// (`y` first) so a sorted list of chunks walks the world top-down, left-right.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Pod, Zeroable)]
pub struct ChunkCoord {
    pub x: i32,
    pub y: i32,
}

impl ChunkCoord {
    #[inline]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// World position of this chunk's top-left cell.
    #[inline]
    pub const fn origin(self) -> Pos {
        Pos::new(self.x << CHUNK_BITS, self.y << CHUNK_BITS)
    }

    /// World position of local cell `i`.
    #[inline]
    pub fn cell(self, i: usize) -> Pos {
        let i = i32::try_from(i).expect("local index fits i32");
        let o = self.origin();
        Pos::new(o.x + (i & MASK), o.y + (i >> CHUNK_BITS))
    }

    #[inline]
    fn key(self) -> (i32, i32) {
        (self.y, self.x)
    }
}

/// The cell data of one chunk. Structure of arrays, row-major inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkCells {
    pub ground: [Ground; CHUNK_CELLS],
    pub feature: [Feature; CHUNK_CELLS],
    pub occupant: [ActorId; CHUNK_CELLS],
}

impl Default for ChunkCells {
    fn default() -> Self {
        Self {
            ground: [Ground::Soil; CHUNK_CELLS],
            feature: [Feature::None; CHUNK_CELLS],
            occupant: [ActorId::NONE; CHUNK_CELLS],
        }
    }
}

impl ChunkCells {
    /// Terrain permits standing on local cell `i` (ignores occupants).
    #[inline]
    pub fn walkable(&self, i: usize) -> bool {
        self.ground[i].walkable() && !self.feature[i].blocks()
    }

    /// Content hash, independent of where the chunk lives in memory.
    pub fn hash(&self) -> u64 {
        let h = fnv1a(0xCBF2_9CE4_8422_2325, bytemuck::cast_slice(&self.ground));
        let h = fnv1a(h, bytemuck::cast_slice(&self.feature));
        fnv1a(h, bytemuck::cast_slice(&self.occupant))
    }
}

/// Bookkeeping for one slab slot.
#[derive(Debug, Clone, Copy)]
pub struct ChunkMeta {
    pub coord: ChunkCoord,
    /// Slot holds a live chunk (free slots keep stale cell data).
    pub loaded: bool,
    /// Modified since it was generated or loaded from disk. Clean chunks are
    /// never written: they can be regenerated from the seed.
    pub dirty: bool,
    /// World tick the chunk's state was current at when it entered the slab
    /// (the tick it was generated, or `last_ticked` from its save file).
    /// While loaded the live value is `World::tick`; `World` refreshes this
    /// whenever the chunk is written. Not part of the checksum.
    pub last_ticked: u64,
}

/// Everything a cell holds, copied out. For convenience APIs, not hot loops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ground: Ground,
    pub feature: Feature,
    pub occupant: ActorId,
}

/// The set of loaded chunks.
#[derive(Debug, Default)]
pub struct Stage {
    /// Slab of cell data; index = slot. Hot data, `par_iter_mut` over it.
    pub cells: Vec<ChunkCells>,
    /// Slab of metadata, same slot numbering.
    pub meta: Vec<ChunkMeta>,
    free: Vec<u32>,
    /// Lookup only. Never iterated (order is random).
    index: HashMap<ChunkCoord, u32>,
    /// Loaded slots sorted by chunk coord: the only iteration order that may
    /// influence results.
    active: Vec<u32>,
}

impl Stage {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- chunk management -----------------------------------------------------------

    /// Slot of a loaded chunk.
    #[inline]
    pub fn slot(&self, c: ChunkCoord) -> Option<u32> {
        self.index.get(&c).copied()
    }

    #[inline]
    pub fn is_loaded(&self, c: ChunkCoord) -> bool {
        self.index.contains_key(&c)
    }

    /// Number of loaded chunks.
    pub fn loaded_count(&self) -> usize {
        self.active.len()
    }

    /// Loaded slots in chunk-coordinate order. Use this for any sequential
    /// pass whose result may depend on order.
    #[inline]
    pub fn active(&self) -> &[u32] {
        &self.active
    }

    /// Loaded chunk coords in canonical order.
    pub fn loaded_coords(&self) -> impl Iterator<Item = ChunkCoord> + '_ {
        self.active.iter().map(|&s| self.meta[s as usize].coord)
    }

    /// Add a chunk. Panics if already loaded. Returns its slot.
    pub fn insert(&mut self, coord: ChunkCoord, cells: ChunkCells, dirty: bool) -> u32 {
        let slot = self.insert_blank(coord, dirty);
        self.cells[slot as usize] = cells;
        slot
    }

    /// Claim a slot for `coord` **without writing its cells**: they hold
    /// whatever the slot held before (stale data or `Default`). The caller
    /// must fill every layer before the chunk is observed. Used by generation
    /// to write each chunk exactly once, straight into the slab.
    pub fn insert_blank(&mut self, coord: ChunkCoord, dirty: bool) -> u32 {
        assert!(!self.is_loaded(coord), "chunk {coord:?} already loaded");
        let meta = ChunkMeta {
            coord,
            loaded: true,
            dirty,
            last_ticked: 0,
        };
        let slot = if let Some(s) = self.free.pop() {
            self.meta[s as usize] = meta;
            s
        } else {
            self.cells.push(ChunkCells::default());
            self.meta.push(meta);
            u32::try_from(self.cells.len() - 1).expect("slot fits u32")
        };
        self.index.insert(coord, slot);
        let at = self
            .active
            .binary_search_by_key(&coord.key(), |&s| self.meta[s as usize].coord.key())
            .unwrap_err();
        self.active.insert(at, slot);
        slot
    }

    /// Make room for `n` more chunks without reallocating the slabs mid-batch.
    pub fn reserve(&mut self, n: usize) {
        let extra = n.saturating_sub(self.free.len());
        self.cells.reserve(extra);
        self.meta.reserve(extra);
        self.index.reserve(n);
        self.active.reserve(n);
    }

    /// Disjoint mutable borrows of the given slots, for a parallel pass over
    /// a subset of chunks (`.into_par_iter()` the result). `slots` must be
    /// strictly increasing. Does not touch dirty flags.
    pub fn cells_mut_at(&mut self, slots: &[u32]) -> Vec<&mut ChunkCells> {
        let mut out = Vec::with_capacity(slots.len());
        let mut rest: &mut [ChunkCells] = &mut self.cells;
        let mut base = 0usize;
        for &s in slots {
            let s = s as usize;
            assert!(s >= base, "slots must be strictly increasing");
            let (head, tail) = std::mem::take(&mut rest).split_at_mut(s - base + 1);
            out.push(&mut head[s - base]);
            rest = tail;
            base = s + 1;
        }
        out
    }

    /// Remove a chunk, handing back its cells and whether they were dirty.
    /// The slot is recycled; the cell data is cloned out (24 KiB), which is
    /// fine at streaming rates.
    pub fn remove(&mut self, coord: ChunkCoord) -> Option<(ChunkCells, bool)> {
        let slot = self.index.remove(&coord)?;
        let m = &mut self.meta[slot as usize];
        m.loaded = false;
        let dirty = m.dirty;
        m.dirty = false;
        let at = self
            .active
            .binary_search_by_key(&coord.key(), |&s| self.meta[s as usize].coord.key())
            .expect("active list out of sync");
        self.active.remove(at);
        self.free.push(slot);
        Some((self.cells[slot as usize].clone(), dirty))
    }

    /// Cells of a loaded chunk.
    #[inline]
    pub fn chunk(&self, c: ChunkCoord) -> Option<&ChunkCells> {
        self.slot(c).map(|s| &self.cells[s as usize])
    }

    /// Mutable cells of a loaded chunk. Marks it dirty.
    #[inline]
    pub fn chunk_mut(&mut self, c: ChunkCoord) -> Option<&mut ChunkCells> {
        let s = self.slot(c)? as usize;
        self.meta[s].dirty = true;
        Some(&mut self.cells[s])
    }

    /// Mark every loaded chunk dirty. Call after a phase that wrote to all
    /// chunks via the slab directly.
    pub fn mark_all_dirty(&mut self) {
        for &s in &self.active {
            self.meta[s as usize].dirty = true;
        }
    }

    // ---- cell access (convenience; hot loops work on `ChunkCells` directly) ------------

    /// `None` if the chunk is not loaded.
    #[inline]
    pub fn get(&self, p: Pos) -> Option<Cell> {
        let (cc, i) = p.split();
        let c = self.chunk(cc)?;
        Some(Cell {
            ground: c.ground[i],
            feature: c.feature[i],
            occupant: c.occupant[i],
        })
    }

    /// Terrain permits standing here. `None` if not loaded.
    #[inline]
    pub fn walkable(&self, p: Pos) -> Option<bool> {
        let (cc, i) = p.split();
        self.chunk(cc).map(|c| c.walkable(i))
    }

    /// Terrain permits standing here and nobody is here. `None` if not loaded.
    #[inline]
    pub fn free(&self, p: Pos) -> Option<bool> {
        let (cc, i) = p.split();
        self.chunk(cc)
            .map(|c| c.walkable(i) && c.occupant[i].is_none())
    }

    // ---- integrity ---------------------------------------------------------------------

    /// Checksum of every loaded chunk. Hashed per chunk in parallel, combined
    /// sequentially in coordinate order, so it is independent of thread count
    /// and of slot assignment.
    pub fn checksum(&self) -> u64 {
        let per_chunk: Vec<u64> = self
            .active
            .par_iter()
            .map(|&s| {
                let m = &self.meta[s as usize];
                splitmix64(
                    self.cells[s as usize].hash()
                        ^ ((m.coord.x as u32 as u64) << 32 | m.coord.y as u32 as u64),
                )
            })
            .collect();
        per_chunk
            .iter()
            .fold(0x5EED_5EED, |acc, &h| splitmix64(acc ^ h))
    }
}

fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(seed, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01B3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_cell_roundtrip_including_negatives() {
        for p in [
            Pos::new(0, 0),
            Pos::new(63, 63),
            Pos::new(64, 0),
            Pos::new(-1, -1),
            Pos::new(-64, 5),
            Pos::new(-65, -130),
            Pos::new(1_000_003, -777_777),
        ] {
            let (cc, i) = p.split();
            assert!(i < CHUNK_CELLS);
            assert_eq!(cc.cell(i), p, "{p:?}");
        }
        assert_eq!(Pos::new(-1, -1).split().0, ChunkCoord::new(-1, -1));
        assert_eq!(Pos::new(-1, -1).split().1, CHUNK_CELLS - 1);
        assert_eq!(ChunkCoord::new(-1, 2).origin(), Pos::new(-64, 128));
    }

    #[test]
    fn neighbors_are_ordered() {
        let n: Vec<_> = Pos::new(0, 0).neighbors4().collect();
        assert_eq!(
            n,
            vec![
                Pos::new(0, -1),
                Pos::new(1, 0),
                Pos::new(0, 1),
                Pos::new(-1, 0)
            ]
        );
        assert_eq!(Pos::new(i32::MAX, 0).offset(1, 0), None);
    }

    #[test]
    fn insert_remove_keeps_active_sorted_and_recycles_slots() {
        let mut s = Stage::new();
        let a = s.insert(ChunkCoord::new(1, 1), ChunkCells::default(), false);
        let b = s.insert(ChunkCoord::new(-3, 0), ChunkCells::default(), false);
        let c = s.insert(ChunkCoord::new(0, 1), ChunkCells::default(), true);
        assert_eq!((a, b, c), (0, 1, 2));
        let order: Vec<_> = s.loaded_coords().collect();
        assert_eq!(
            order,
            vec![
                ChunkCoord::new(-3, 0),
                ChunkCoord::new(0, 1),
                ChunkCoord::new(1, 1)
            ]
        );
        let (_, dirty) = s.remove(ChunkCoord::new(0, 1)).unwrap();
        assert!(dirty);
        assert_eq!(s.loaded_count(), 2);
        assert!(!s.is_loaded(ChunkCoord::new(0, 1)));
        // Recycled slot.
        let d = s.insert(ChunkCoord::new(9, 9), ChunkCells::default(), false);
        assert_eq!(d, 2);
        assert_eq!(s.remove(ChunkCoord::new(42, 42)), None);
    }

    #[test]
    fn cells_mut_at_gives_disjoint_borrows() {
        let mut s = Stage::new();
        for i in 0..5 {
            s.insert_blank(ChunkCoord::new(i, 0), false);
        }
        let mut views = s.cells_mut_at(&[0, 2, 4]);
        for (k, c) in views.iter_mut().enumerate() {
            c.ground[0] = if k == 1 { Ground::Water } else { Ground::Soil };
        }
        assert_eq!(s.cells[2].ground[0], Ground::Water);
        assert_eq!(s.cells[0].ground[0], Ground::Soil);
        assert_eq!(s.cells[4].ground[0], Ground::Soil);
        assert!(s.cells_mut_at(&[4]).len() == 1);
        assert!(s.cells_mut_at(&[]).is_empty());
    }

    #[test]
    fn checksum_is_independent_of_slot_order() {
        let mut cells = ChunkCells::default();
        cells.ground[5] = Ground::Water;
        let mut a = Stage::new();
        a.insert(ChunkCoord::new(0, 0), cells.clone(), false);
        a.insert(ChunkCoord::new(1, 0), ChunkCells::default(), false);
        let mut b = Stage::new();
        b.insert(ChunkCoord::new(1, 0), ChunkCells::default(), false);
        b.insert(ChunkCoord::new(0, 0), cells, false);
        assert_eq!(a.checksum(), b.checksum());
        // But it does see position and content.
        let mut c = Stage::new();
        c.insert(ChunkCoord::new(0, 0), ChunkCells::default(), false);
        c.insert(ChunkCoord::new(1, 0), ChunkCells::default(), false);
        assert_ne!(a.checksum(), c.checksum());
        let mut d = Stage::new();
        d.insert(ChunkCoord::new(0, 1), ChunkCells::default(), false);
        d.insert(ChunkCoord::new(1, 0), ChunkCells::default(), false);
        assert_ne!(c.checksum(), d.checksum());
    }

    #[test]
    fn cell_queries_and_dirty_tracking() {
        let mut s = Stage::new();
        s.insert(ChunkCoord::new(0, 0), ChunkCells::default(), false);
        let p = Pos::new(3, 4);
        assert_eq!(s.free(p), Some(true));
        assert_eq!(s.get(Pos::new(64, 0)), None);
        let (cc, i) = p.split();
        s.chunk_mut(cc).unwrap().feature[i] = Feature::Rock;
        assert!(s.meta[0].dirty);
        assert_eq!(s.walkable(p), Some(false));
        s.chunk_mut(cc).unwrap().feature[i] = Feature::None;
        s.chunk_mut(cc).unwrap().ground[i] = Ground::Water;
        assert_eq!(s.walkable(p), Some(false));
        s.chunk_mut(cc).unwrap().ground[i] = Ground::Soil;
        s.chunk_mut(cc).unwrap().occupant[i] = ActorId(7);
        assert_eq!((s.walkable(p), s.free(p)), (Some(true), Some(false)));
        assert_eq!(s.get(p).unwrap().occupant, ActorId(7));
    }
}
