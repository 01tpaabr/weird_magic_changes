//! The Stage: a 2D grid of cells that actors stand on, a billion cells each
//! way from the origin ([`WORLD_EXTENT`]), stored as fixed-size **chunks**,
//! one entity per loaded chunk.
//!
//! ```text
//! world cell (x, y): i32          chunk coord = (x >> CHUNK_BITS, y >> CHUNK_BITS)
//!                                 local index = (y & MASK) * CHUNK_SIZE + (x & MASK)
//! ```
//!
//! A chunk is [`CHUNK_SIZE`]² cells with one contiguous array per layer
//! ([`ChunkCells`]). It is, at the same time:
//! - the **parallel work unit**: a phase is `Query<&mut ChunkCells>::par_iter_mut`;
//! - the **streaming unit**: loaded around the camera, unloaded far away;
//! - the **save unit**: one file per modified chunk (see `crate::store`).
//!
//! A loaded chunk is an entity with these components: [`ChunkCoord`] (where),
//! [`ChunkCells`] (the layers; Bevy keeps all of them in one dense table
//! column, i.e. a `Vec<ChunkCells>`), the actor rows, [`ChunkMeta`] (dirty
//! flag, last tick), and per-tick scratch (`Intents`, `Scratch`, `Outbox`).
//! Entity ids and table order depend on load history, so **nothing observable
//! may depend on them**: every sequential merge and the checksum walk
//! [`Stage::active`], the loaded set sorted by chunk coordinate.
//!
//! Layers today:
//! - `ground`: what the cell *is* ([`Ground`]).
//! - `feature`: what sits *on* the ground but is not an actor ([`Feature`]).
//! - `occupant`: which actor stands here, at most one ([`ActorId`]: the
//!   actor's kind and its row in the chunk's actor arrays).
//!
//! Actors are rows on the chunk entity too ([`ChunkActors`], [`ChunkMinds`];
//! see `crate::actors`). [`ChunkData`] is the bundle of everything a chunk
//! owns, the unit worldgen produces and the store reads and writes.
//!
//! Adding a per-cell scalar (moisture, heat, mana): add an array to
//! [`ChunkCells`], fold it into `hash`, encode it in `store`, render it in `app`.

pub mod scent;
pub mod worldgen;

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemParam;
use bytemuck::{CheckedBitPattern, NoUninit, Pod, Zeroable};

use crate::actors::{self, ChunkActors, ChunkMinds, Intents, Outbox, Scratch};
use crate::par::par_map;
use crate::rng::splitmix64;

/// log2 of the chunk side. 64² = 4096 cells: 4 KiB per byte layer, 16 KiB
/// per actor-id layer, one task. Tune with `make bench`. Changing it changes
/// save files (bump `store::FORMAT_VERSION`) **and sim results**: actors
/// resolve cross-chunk moves after in-chunk ones (home advantage, decision
/// 30), and a cell system's cadence is staggered by chunk coordinate, so
/// where the borders fall is part of a world's identity (the save header
/// records it). Worldgen alone does not depend on it.
pub const CHUNK_BITS: u32 = 6;
/// Chunk side length in cells.
pub const CHUNK_SIZE: i32 = 1 << CHUNK_BITS;
/// Cells per chunk.
pub const CHUNK_CELLS: usize = (CHUNK_SIZE * CHUNK_SIZE) as usize;
/// Scent channels per cell (`mark`, `sniff`): named by the rules, numbered
/// in first-appearance order.
pub const SCENT_CHANNELS: usize = 4;
const _: () = assert!(CHUNK_CELLS <= 1 << u16::BITS);

/// A local cell index, `0..CHUNK_CELLS`, as the `u16` that rows, effects
/// and hits keep.
#[inline]
#[allow(clippy::cast_possible_truncation)] // below CHUNK_CELLS, which fits
pub fn cell_u16(cell: usize) -> u16 {
    debug_assert!(cell < CHUNK_CELLS, "cell {cell} is outside the chunk");
    cell as u16
}
const MASK: i32 = CHUNK_SIZE - 1;

/// The world's extent: a cell's `x` and `y` each lie in
/// `[-WORLD_EXTENT, WORLD_EXTENT)`, whole chunks either way. Nothing past it
/// ever loads: streaming clamps its focus to it, and scenarios and saves
/// refuse positions past it. So to an actor at the edge the cells beyond
/// read as an unloaded chunk (rock, nobody) and a move or spawn there is
/// BLOCKED. Every chunk inside, and every neighbour of one, has
/// representable cells, and the difference of two positions inside fits an
/// `i32`. It is also where the app's camera stops.
pub const WORLD_EXTENT: i32 = 1_000_000_000;
/// [`WORLD_EXTENT`] in chunks: a chunk coordinate inside lies in
/// `[-CHUNK_EXTENT, CHUNK_EXTENT)`.
pub const CHUNK_EXTENT: i32 = WORLD_EXTENT / CHUNK_SIZE;
const _: () = assert!(WORLD_EXTENT % CHUNK_SIZE == 0 && WORLD_EXTENT <= 1 << 30);

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

/// What stands on a cell: the actor's `kind << 16 | slot`, where `slot` is
/// its row in the chunk's actor arrays (valid within a tick only; identity
/// across ticks is `ActorMind::uid`). `ActorId::NONE` marks an empty cell,
/// which reserves kind `0xFFFF`. Packing the kind here lets a vision scan
/// learn what is where from the occupant array alone.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Pod, Zeroable)]
pub struct ActorId(pub u32);

impl ActorId {
    pub const NONE: ActorId = ActorId(u32::MAX);

    #[inline]
    pub fn pack(kind: u16, slot: u16) -> ActorId {
        debug_assert!(kind != u16::MAX, "kind 0xFFFF is reserved");
        ActorId(u32::from(kind) << 16 | u32::from(slot))
    }

    /// `(kind, slot)` of a live id, `None` for an empty cell.
    #[inline]
    #[allow(clippy::cast_possible_truncation)] // the two halves: truncation is the point
    pub fn unpack(self) -> Option<(u16, u16)> {
        (!self.is_none()).then_some(((self.0 >> 16) as u16, self.0 as u16))
    }

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

/// World cell coordinate; `y` grows downward. The initial map occupies
/// `[0, w) x [0, h)`, everything else is generated on demand, up to
/// [`WORLD_EXTENT`] each way. A `Pos` may lie outside it: nothing is there.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Pod, Zeroable)]
pub struct Pos {
    pub x: i32,
    pub y: i32,
}

impl Pos {
    #[inline]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Inside the world's extent ([`WORLD_EXTENT`])?
    #[inline]
    pub const fn in_world(self) -> bool {
        -WORLD_EXTENT <= self.x
            && self.x < WORLD_EXTENT
            && -WORLD_EXTENT <= self.y
            && self.y < WORLD_EXTENT
    }

    /// The nearest position inside the world's extent.
    #[inline]
    pub fn clamp_to_world(self) -> Pos {
        let c = |v: i32| v.clamp(-WORLD_EXTENT, WORLD_EXTENT - 1);
        Pos::new(c(self.x), c(self.y))
    }

    /// `self + (dx, dy)`, `None` only on i32 overflow.
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
/// Also the component that says where a chunk entity is.
#[repr(C)]
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash, Pod, Zeroable)]
#[component(immutable)]
pub struct ChunkCoord {
    pub x: i32,
    pub y: i32,
}

impl ChunkCoord {
    #[inline]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Inside the world's extent ([`CHUNK_EXTENT`])?
    #[inline]
    pub const fn in_world(self) -> bool {
        -CHUNK_EXTENT <= self.x
            && self.x < CHUNK_EXTENT
            && -CHUNK_EXTENT <= self.y
            && self.y < CHUNK_EXTENT
    }

    /// World position of this chunk's top-left cell (for a chunk inside the
    /// world or next to it: further out, the shift wraps).
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

    /// Sort key: row-major over chunks.
    #[inline]
    pub fn key(self) -> (i32, i32) {
        (self.y, self.x)
    }

    /// The coordinate as 64 bits, for hashing.
    #[inline]
    fn bits(self) -> u64 {
        (u64::from(self.x as u32) << 32) | u64::from(self.y as u32)
    }
}

/// The cell data of one chunk. Structure of arrays, row-major inside.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct ChunkCells {
    pub ground: [Ground; CHUNK_CELLS],
    pub feature: [Feature; CHUNK_CELLS],
    /// Who stands here, at most one (blocks movement).
    pub occupant: [ActorId; CHUNK_CELLS],
    /// What grows on the ground here, at most one (a `cover` kind: never
    /// blocks, lies under whoever stands on the cell).
    pub cover: [ActorId; CHUNK_CELLS],
    /// Scent per channel: raised by `mark`, decayed by `scent_decay`.
    pub scent: [[u8; CHUNK_CELLS]; SCENT_CHANNELS],
}

impl Default for ChunkCells {
    fn default() -> Self {
        Self {
            ground: [Ground::Soil; CHUNK_CELLS],
            feature: [Feature::None; CHUNK_CELLS],
            occupant: [ActorId::NONE; CHUNK_CELLS],
            cover: [ActorId::NONE; CHUNK_CELLS],
            scent: [[0; CHUNK_CELLS]; SCENT_CHANNELS],
        }
    }
}

impl ChunkCells {
    /// Terrain permits standing on local cell `i` (ignores occupants).
    #[inline]
    pub fn walkable(&self, i: usize) -> bool {
        self.ground[i].walkable() && !self.feature[i].blocks()
    }

    /// The occupant layer, or the cover layer.
    #[inline]
    pub fn layer(&self, cover: bool) -> &[ActorId; CHUNK_CELLS] {
        if cover { &self.cover } else { &self.occupant }
    }

    /// [`ChunkCells::layer`], mutable.
    #[inline]
    pub fn layer_mut(&mut self, cover: bool) -> &mut [ActorId; CHUNK_CELLS] {
        if cover {
            &mut self.cover
        } else {
            &mut self.occupant
        }
    }

    /// Content hash, independent of where the chunk lives in memory.
    pub fn hash(&self) -> u64 {
        let h = fnv1a(0xCBF2_9CE4_8422_2325, bytemuck::cast_slice(&self.ground));
        let h = fnv1a(h, bytemuck::cast_slice(&self.feature));
        let h = fnv1a(h, bytemuck::cast_slice(&self.occupant));
        let h = fnv1a(h, bytemuck::cast_slice(&self.cover));
        fnv1a(h, bytemuck::cast_slice(&self.scent))
    }
}

/// Everything a chunk owns besides its coordinate and bookkeeping: the cell
/// layers and the actor rows. The unit worldgen fills and the store moves.
#[derive(Bundle, Debug, Clone, Default, PartialEq, Eq)]
pub struct ChunkData {
    pub cells: ChunkCells,
    pub actors: ChunkActors,
    pub minds: ChunkMinds,
}

impl ChunkData {
    /// Cells and actor rows as one mutable view.
    pub fn actors_mut(&mut self) -> actors::ActorsMut<'_> {
        actors::ActorsMut {
            pubs: &mut self.actors.rows,
            minds: &mut self.minds.rows,
            cells: &mut self.cells,
        }
    }

    /// Row invariants hold against a kind table of `kinds` entries.
    pub fn validate(&self, kinds: usize) -> Result<(), String> {
        actors::validate(&self.cells, &self.actors.rows, &self.minds.rows, kinds)
    }

    /// Content hash: cells then rows.
    pub fn hash(&self) -> u64 {
        splitmix64(self.cells.hash() ^ actors::hash(&self.actors.rows, &self.minds.rows))
    }
}

/// Bookkeeping for one chunk entity.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkMeta {
    /// Must be written before it is dropped: changed since it was generated
    /// or read, or inhabited (rows carry clocks, which a save file stamps
    /// with the tick it was written at). A clean chunk is empty and
    /// unchanged: the seed or its file brings it back as it is.
    pub dirty: bool,
}

/// The set of loaded chunks: coordinate -> entity, plus the canonical order.
/// Cell data lives on the entities; this resource is the directory.
#[derive(Resource, Debug, Default)]
pub struct Stage {
    /// Lookup only. Never iterated (order is random).
    index: HashMap<ChunkCoord, Entity>,
    /// Loaded chunks sorted by coordinate: the only iteration order that may
    /// influence results.
    active: Vec<(ChunkCoord, Entity)>,
}

impl Stage {
    /// Entity of a loaded chunk.
    #[inline]
    pub fn entity(&self, c: ChunkCoord) -> Option<Entity> {
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

    /// Loaded chunks in chunk-coordinate order. Use this for any sequential
    /// pass whose result may depend on order.
    #[inline]
    pub fn active(&self) -> &[(ChunkCoord, Entity)] {
        &self.active
    }

    /// Loaded chunk coords in canonical order.
    pub fn loaded_coords(&self) -> impl Iterator<Item = ChunkCoord> + '_ {
        self.active.iter().map(|&(c, _)| c)
    }

    fn add(&mut self, coord: ChunkCoord, entity: Entity) {
        assert!(
            self.index.insert(coord, entity).is_none(),
            "chunk {coord:?} already loaded"
        );
        let at = self
            .active
            .binary_search_by_key(&coord.key(), |&(c, _)| c.key())
            .unwrap_err();
        self.active.insert(at, (coord, entity));
    }

    fn take(&mut self, coord: ChunkCoord) -> Option<Entity> {
        let e = self.index.remove(&coord)?;
        let at = self
            .active
            .binary_search_by_key(&coord.key(), |&(c, _)| c.key())
            .expect("active list out of sync");
        self.active.remove(at);
        Some(e)
    }
}

// ---- chunk management (exclusive access) -------------------------------------------

/// Spawn a chunk entity. Panics if `coord` is already loaded. The world must
/// have a [`Stage`] resource (`sim::install`).
pub fn insert(world: &mut World, coord: ChunkCoord, data: ChunkData, dirty: bool) -> Entity {
    let e = world
        .spawn((
            coord,
            data,
            ChunkMeta { dirty },
            Intents::default(),
            Scratch::default(),
            Outbox::default(),
        ))
        .id();
    world.resource_mut::<Stage>().add(coord, e);
    e
}

/// Despawn a chunk, handing back its data and whether it was dirty. The data
/// is cloned out (24 KiB of cells plus the rows), fine at streaming rates.
pub fn remove(world: &mut World, coord: ChunkCoord) -> Option<(ChunkData, bool)> {
    let e = world.resource_mut::<Stage>().take(coord)?;
    let data = ChunkData {
        cells: world.get::<ChunkCells>(e).expect("chunk has cells").clone(),
        actors: world
            .get::<ChunkActors>(e)
            .expect("chunk has actors")
            .clone(),
        minds: world.get::<ChunkMinds>(e).expect("chunk has minds").clone(),
    };
    let dirty = world.get::<ChunkMeta>(e).expect("chunk has meta").dirty;
    world.despawn(e);
    Some((data, dirty))
}

/// Cells of a loaded chunk, from exclusive world access.
#[inline]
pub fn chunk(world: &World, c: ChunkCoord) -> Option<&ChunkCells> {
    let e = world.resource::<Stage>().entity(c)?;
    world.get::<ChunkCells>(e)
}

/// Mutable cells of a loaded chunk. Marks it dirty.
pub fn chunk_mut(world: &mut World, c: ChunkCoord) -> Option<Mut<'_, ChunkCells>> {
    let e = world.resource::<Stage>().entity(c)?;
    world.get_mut::<ChunkMeta>(e)?.dirty = true;
    world.get_mut::<ChunkCells>(e)
}

/// Checksum of every loaded chunk (cells and actor rows). Hashed per chunk
/// in parallel, combined sequentially in coordinate order, so it is
/// independent of thread count and of entity ids.
pub fn checksum(world: &mut World) -> u64 {
    let mut state = world.query::<(&ChunkCells, &ChunkActors, &ChunkMinds)>();
    let chunks = state.query(world);
    let stage = world.resource::<Stage>();
    let per_chunk = par_map(stage.active(), 16, |&(coord, e)| {
        let (c, a, m) = chunks.get(e).expect("active chunk has data");
        let rows = actors::hash(&a.rows, &m.rows);
        splitmix64(splitmix64(c.hash() ^ rows) ^ coord.bits())
    });
    per_chunk
        .iter()
        .fold(0x5EED_5EED, |acc, &h| splitmix64(acc ^ h))
}

// ---- cell access from systems -----------------------------------------------------------

/// Read-only view of the stage for systems: the directory plus the cells.
/// Convenience API; hot loops iterate `Query<&ChunkCells>` directly.
#[derive(SystemParam)]
pub struct StageCells<'w, 's> {
    stage: Res<'w, Stage>,
    cells: Query<'w, 's, &'static ChunkCells>,
}

impl std::fmt::Debug for StageCells<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StageCells({} chunks)", self.stage.loaded_count())
    }
}

impl StageCells<'_, '_> {
    pub fn loaded_count(&self) -> usize {
        self.stage.loaded_count()
    }

    /// Cells of a loaded chunk.
    #[inline]
    pub fn chunk(&self, c: ChunkCoord) -> Option<&ChunkCells> {
        let e = self.stage.entity(c)?;
        self.cells.get(e).ok()
    }
}

pub(crate) fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(seed, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01B3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::system::RunSystemOnce;

    fn world() -> World {
        crate::par::init_task_pool();
        let mut w = World::new();
        w.init_resource::<Stage>();
        w
    }

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

    /// The extent is whole chunks: a cell is inside iff its chunk is, and
    /// the chunks just past the edge still have representable cells.
    #[test]
    fn the_world_extent_is_whole_chunks() {
        let (lo, hi) = (-WORLD_EXTENT, WORLD_EXTENT - 1);
        for (p, inside) in [
            (Pos::new(0, 0), true),
            (Pos::new(lo, lo), true),
            (Pos::new(hi, hi), true),
            (Pos::new(hi + 1, 0), false),
            (Pos::new(0, lo - 1), false),
            (Pos::new(i32::MAX, i32::MIN), false),
        ] {
            assert_eq!(p.in_world(), inside, "{p:?}");
            let (cc, i) = p.split();
            assert_eq!(cc.in_world(), inside, "{p:?}");
            assert_eq!(cc.cell(i), p, "{p:?}");
        }
        assert_eq!(
            Pos::new(hi, 0).split().0,
            ChunkCoord::new(CHUNK_EXTENT - 1, 0)
        );
        assert_eq!(
            ChunkCoord::new(CHUNK_EXTENT, 0).origin(),
            Pos::new(WORLD_EXTENT, 0)
        );
        let past = ChunkCoord::new(-CHUNK_EXTENT - 1, CHUNK_EXTENT);
        assert_eq!(
            past.cell(CHUNK_CELLS - 1),
            Pos::new(lo - 1, WORLD_EXTENT + 63)
        );
        assert_eq!(
            Pos::new(i32::MAX, i32::MIN).clamp_to_world(),
            Pos::new(hi, lo)
        );
        assert_eq!(Pos::new(-5, 7).clamp_to_world(), Pos::new(-5, 7));
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
    fn insert_remove_keeps_active_sorted_and_entities_die() {
        let mut w = world();
        let a = insert(&mut w, ChunkCoord::new(1, 1), ChunkData::default(), false);
        let b = insert(&mut w, ChunkCoord::new(-3, 0), ChunkData::default(), false);
        let c = insert(&mut w, ChunkCoord::new(0, 1), ChunkData::default(), true);
        assert!(a != b && b != c);
        let order: Vec<_> = w.resource::<Stage>().loaded_coords().collect();
        assert_eq!(
            order,
            vec![
                ChunkCoord::new(-3, 0),
                ChunkCoord::new(0, 1),
                ChunkCoord::new(1, 1)
            ]
        );
        let (_, dirty) = remove(&mut w, ChunkCoord::new(0, 1)).unwrap();
        assert!(dirty);
        assert!(w.get_entity(c).is_err());
        let s = w.resource::<Stage>();
        assert_eq!(s.loaded_count(), 2);
        assert!(!s.is_loaded(ChunkCoord::new(0, 1)));
        assert_eq!(remove(&mut w, ChunkCoord::new(42, 42)), None);
        assert_eq!(w.query::<&ChunkCells>().iter(&w).count(), 2);
    }

    #[test]
    fn checksum_is_independent_of_insertion_order() {
        let mut cells = ChunkData::default();
        cells.cells.ground[5] = Ground::Water;
        let mut a = world();
        insert(&mut a, ChunkCoord::new(0, 0), cells.clone(), false);
        insert(&mut a, ChunkCoord::new(1, 0), ChunkData::default(), false);
        let mut b = world();
        insert(&mut b, ChunkCoord::new(1, 0), ChunkData::default(), false);
        insert(&mut b, ChunkCoord::new(0, 0), cells, false);
        assert_eq!(checksum(&mut a), checksum(&mut b));
        // But it does see position and content.
        let mut c = world();
        insert(&mut c, ChunkCoord::new(0, 0), ChunkData::default(), false);
        insert(&mut c, ChunkCoord::new(1, 0), ChunkData::default(), false);
        assert_ne!(checksum(&mut a), checksum(&mut c));
        let mut d = world();
        insert(&mut d, ChunkCoord::new(0, 1), ChunkData::default(), false);
        insert(&mut d, ChunkCoord::new(1, 0), ChunkData::default(), false);
        assert_ne!(checksum(&mut c), checksum(&mut d));
        // Many chunks: exercises the batched parallel path.
        let mut e = world();
        for y in 0..10 {
            for x in 0..10 {
                insert(&mut e, ChunkCoord::new(x, y), ChunkData::default(), false);
            }
        }
        let first = checksum(&mut e);
        assert_eq!(first, checksum(&mut e));
    }

    #[test]
    fn cell_queries_and_dirty_tracking() {
        let mut w = world();
        let cc = ChunkCoord::new(0, 0);
        insert(&mut w, cc, ChunkData::default(), false);
        let p = Pos::new(3, 4);
        let (_, i) = p.split();
        // (walkable, occupant) at `p` through the system param; None if not loaded.
        let at = |w: &mut World, p: Pos| {
            let (cc, i) = p.split();
            w.run_system_once(move |s: StageCells| {
                s.chunk(cc).map(|c| (c.walkable(i), c.occupant[i]))
            })
            .unwrap()
        };
        assert_eq!(at(&mut w, p), Some((true, ActorId::NONE)));
        assert_eq!(at(&mut w, Pos::new(64, 0)), None);
        chunk_mut(&mut w, cc).unwrap().feature[i] = Feature::Rock;
        let e = w.resource::<Stage>().entity(cc).unwrap();
        assert!(w.get::<ChunkMeta>(e).unwrap().dirty);
        assert_eq!(at(&mut w, p), Some((false, ActorId::NONE)));
        chunk_mut(&mut w, cc).unwrap().feature[i] = Feature::None;
        chunk_mut(&mut w, cc).unwrap().ground[i] = Ground::Water;
        assert_eq!(at(&mut w, p), Some((false, ActorId::NONE)));
        chunk_mut(&mut w, cc).unwrap().ground[i] = Ground::Soil;
        chunk_mut(&mut w, cc).unwrap().occupant[i] = ActorId(7);
        assert_eq!(at(&mut w, p), Some((true, ActorId(7))));
        assert_eq!(chunk(&w, cc).unwrap().occupant[i], ActorId(7));
        assert!(chunk(&w, ChunkCoord::new(5, 5)).is_none());
    }
}
