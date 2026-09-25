//! Actors: rows inside the chunk they stand on (`docs/ACTORS.md` §2).
//!
//! An actor is not an entity. It is row `i` of two parallel arrays on its
//! chunk entity: [`ChunkActors`] holds the 12-byte public record every chunk
//! may read while thinking, [`ChunkMinds`] the 88-byte private record only
//! the owning chunk touches. The chunk's `occupant` layer points back:
//! `occupant[row.cell] == ActorId::pack(row.kind, i)`. [`ActorsMut`] is the
//! one place that invariant is maintained; everything that adds, kills or
//! compacts rows goes through it.
//!
//! Slots (row indices) are chunk-local and valid only within a tick: rows are
//! appended during a tick and removed only by [`ActorsMut::compact`] at its
//! end. Identity across ticks is [`ActorMind::uid`]. Both records are Pod so
//! a chunk's actors save as one memcpy per array (`store`).
//!
//! The kind table and the programs live in `crate::rules`; the systems that
//! run them each tick in [`systems`].

pub mod systems;

use bevy_ecs::prelude::*;
use bytemuck::{Pod, Zeroable};

use crate::stage::{ActorId, CHUNK_CELLS};

pub use systems::{CrossScratch, Effect, Hit, Intent, Intents, Outbox, Scratch};

/// Need counters per actor, named per kind by its rules file.
pub const NEED_SLOTS: usize = 4;
/// Persistent memory slots per actor, named per kind by its rules file.
pub const MEM_SLOTS: usize = 12;
/// Rows reserved per chunk when it is loaded. Growth past this is chunk-level
/// and amortised (a `Vec` doubling), never a per-actor allocation.
pub const RESERVE: usize = 256;

/// Bits of [`ActorPub::flags`].
pub mod flags {
    /// Killed this tick; removed by `compact` at the end of it. A saved
    /// chunk never holds a dead row.
    pub const DEAD: u8 = 1 << 0;
    /// Think next tick regardless of cadence (hurt, taken from).
    pub const WAKE: u8 = 1 << 1;
}

/// The public record: what any chunk may read about an actor while thinking.
/// Written only by the owning chunk, and never during Think.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct ActorPub {
    /// Local cell index (`0..CHUNK_CELLS`).
    pub cell: u16,
    /// Index into the kind table. `0xFFFF` is reserved by `ActorId::NONE`.
    pub kind: u16,
    /// Cadence phase: the low bits of `uid`, so a flock never moves in lockstep.
    pub stagger: u16,
    /// Rule-written, readable by others (`signal_of`). What an actor broadcasts.
    pub signal: i16,
    /// Rule-written appearance byte: palette variant and the `kind:look` predicate.
    pub look: u8,
    /// See [`flags`].
    pub flags: u8,
    pub _pad: u16,
}

/// The private record: everything only the actor's own rules see.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct ActorMind {
    /// Identity across ticks and chunks. Worldgen rows: `hash_cell(seed,
    /// STREAM_UID, x, y)`; run-time spawns fold in the tick.
    pub uid: u64,
    /// Tick the actor came to be (wrapping); `age = tick - born`.
    pub born: u32,
    /// Tick of the last think (wrapping); needs decay by the difference.
    pub last_think: u32,
    /// Ticks-until-empty, or points for `decay 0` needs. Named per kind.
    pub needs: [i32; NEED_SLOTS],
    /// The program's whole persistent memory. Named per kind.
    pub mem: [i32; MEM_SLOTS],
    /// Current `state` block.
    pub state: u8,
    /// Latched event bits, read by the next think.
    pub events: u8,
    /// Damage taken since the last think (saturating sum).
    pub hurt: u8,
    /// Direction of the lowest-key attacker.
    pub hurt_dir: u8,
    pub _pad: u32,
}

const _: () = assert!(size_of::<ActorPub>() == 12);
const _: () = assert!(size_of::<ActorMind>() == 88);

/// Public rows of one chunk. Same length and order as its [`ChunkMinds`].
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct ChunkActors {
    pub rows: Vec<ActorPub>,
}

/// Private rows of one chunk. Same length and order as its [`ChunkActors`].
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct ChunkMinds {
    pub rows: Vec<ActorMind>,
}

impl ChunkActors {
    pub fn with_reserve() -> Self {
        Self {
            rows: Vec::with_capacity(RESERVE),
        }
    }
}

impl ChunkMinds {
    pub fn with_reserve() -> Self {
        Self {
            rows: Vec::with_capacity(RESERVE),
        }
    }
}

/// Mutable view of one chunk's actor storage. Keeps the three arrays in
/// agreement: every row has `occupant[cell] == pack(kind, slot)` and no
/// two rows share a cell.
#[derive(Debug)]
pub struct ActorsMut<'a> {
    pub pubs: &'a mut Vec<ActorPub>,
    pub minds: &'a mut Vec<ActorMind>,
    pub occupant: &'a mut [ActorId; CHUNK_CELLS],
}

impl ActorsMut<'_> {
    /// Append a live row on a free cell and point the cell at it. Returns the
    /// slot. Panics if the cell is taken: callers claim cells first.
    pub fn push(&mut self, cell: usize, kind: u16, mind: ActorMind) -> u16 {
        assert!(self.occupant[cell].is_none(), "cell {cell} is occupied");
        let slot = u16::try_from(self.pubs.len()).expect("fewer rows than cells");
        self.pubs.push(ActorPub {
            cell: cell as u16,
            kind,
            stagger: mind.uid as u16,
            signal: 0,
            look: 0,
            flags: 0,
            _pad: 0,
        });
        self.minds.push(mind);
        self.occupant[cell] = ActorId::pack(kind, slot);
        slot
    }

    /// Flag a row dead and free its cell. The row stays until [`compact`].
    ///
    /// [`compact`]: ActorsMut::compact
    pub fn kill(&mut self, slot: usize) {
        let p = &mut self.pubs[slot];
        if p.flags & flags::DEAD == 0 {
            p.flags |= flags::DEAD;
            self.occupant[usize::from(p.cell)] = ActorId::NONE;
        }
    }

    /// Remove every dead row by swap-remove, walking from the last row down
    /// so a row swapped into a hole has already been checked, and re-point
    /// the moved row's cell. Never clears a cell (kills already did).
    pub fn compact(&mut self) {
        debug_assert_eq!(self.pubs.len(), self.minds.len());
        for i in (0..self.pubs.len()).rev() {
            if self.pubs[i].flags & flags::DEAD == 0 {
                continue;
            }
            self.pubs.swap_remove(i);
            self.minds.swap_remove(i);
            if i < self.pubs.len() {
                let moved = self.pubs[i];
                self.occupant[usize::from(moved.cell)] = ActorId::pack(moved.kind, i as u16);
            }
        }
    }
}

/// Check the invariants a saved or generated chunk must satisfy before it
/// enters the world: equal row counts, no dead rows, every kind known, every
/// row on a cell that points back at it, and every occupied cell owning a row.
pub fn validate(
    occupant: &[ActorId; CHUNK_CELLS],
    pubs: &[ActorPub],
    minds: &[ActorMind],
    kinds: usize,
) -> Result<(), String> {
    if pubs.len() != minds.len() {
        return Err(format!(
            "{} public rows but {} private rows",
            pubs.len(),
            minds.len()
        ));
    }
    if pubs.len() > CHUNK_CELLS {
        return Err(format!("{} rows for {CHUNK_CELLS} cells", pubs.len()));
    }
    for (slot, p) in pubs.iter().enumerate() {
        if p.flags & flags::DEAD != 0 {
            return Err(format!("row {slot} is dead"));
        }
        if usize::from(p.kind) >= kinds {
            return Err(format!("row {slot} has unknown kind {}", p.kind));
        }
        let cell = usize::from(p.cell);
        if cell >= CHUNK_CELLS {
            return Err(format!("row {slot} is on cell {cell}, outside the chunk"));
        }
        let want = ActorId::pack(p.kind, slot as u16);
        if occupant[cell] != want {
            return Err(format!(
                "cell {cell} holds {:?}, row {slot} expects {want:?}",
                occupant[cell]
            ));
        }
    }
    let occupied = occupant.iter().filter(|o| !o.is_none()).count();
    if occupied != pubs.len() {
        return Err(format!("{occupied} occupied cells but {} rows", pubs.len()));
    }
    Ok(())
}

/// Content hash of a chunk's rows, order-sensitive (row order is part of the
/// state: it is the slot order every intent of the next tick refers to).
pub fn hash(pubs: &[ActorPub], minds: &[ActorMind]) -> u64 {
    let h = crate::stage::fnv1a(0x9E37_79B9_7F4A_7C15, bytemuck::cast_slice(pubs));
    crate::stage::fnv1a(h, bytemuck::cast_slice(minds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::SEED;
    use crate::stage::ChunkData;

    fn mind(uid: u64) -> ActorMind {
        ActorMind {
            uid,
            ..ActorMind::zeroed()
        }
    }

    #[test]
    fn records_are_pod_sized_as_documented() {
        assert_eq!(size_of::<ActorPub>(), 12);
        assert_eq!(size_of::<ActorMind>(), 88);
        assert_eq!(align_of::<ActorMind>(), 8);
        let m = mind(7);
        let bytes: &[u8] = bytemuck::bytes_of(&m);
        assert_eq!(bytes.len(), 88);
        assert_eq!(bytes[0], 7);
    }

    #[test]
    fn push_points_the_cell_at_the_row_and_takes_the_uid_stagger() {
        let mut d = ChunkData::default();
        assert_eq!(d.actors_mut().push(10, SEED, mind(0x1_2345)), 0);
        assert_eq!(d.actors_mut().push(20, SEED, mind(9)), 1);
        let o = &d.cells.occupant;
        assert_eq!(o[10], ActorId::pack(SEED, 0));
        assert_eq!(o[20], ActorId::pack(SEED, 1));
        assert_eq!(d.actors.rows[0].stagger, 0x2345);
        assert_eq!(d.actors.rows[1].cell, 20);
        assert_eq!(d.actors.rows[1].flags, 0);
        assert_eq!(d.validate(5), Ok(()));
        assert_eq!(ActorId::pack(3, 4).unpack(), Some((3, 4)));
        assert_eq!(ActorId::NONE.unpack(), None);
    }

    #[test]
    #[should_panic(expected = "occupied")]
    fn push_onto_a_taken_cell_panics() {
        let mut d = ChunkData::default();
        d.actors_mut().push(5, SEED, mind(1));
        d.actors_mut().push(5, SEED, mind(2));
    }

    #[test]
    fn kill_then_compact_keeps_every_survivor_reachable() {
        let mut d = ChunkData::default();
        for i in 0..6u64 {
            d.actors_mut().push(100 + i as usize, SEED, mind(i));
        }
        let mut a = d.actors_mut();
        a.kill(0);
        a.kill(3);
        a.kill(5);
        a.kill(3); // twice is harmless
        let o = &d.cells.occupant;
        assert!(o[100].is_none() && o[103].is_none() && o[105].is_none());
        assert_eq!(d.actors.rows.len(), 6);
        d.actors_mut().compact();
        assert_eq!(d.actors.rows.len(), 3);
        assert_eq!(d.minds.rows.len(), 3);
        let uids: Vec<u64> = d.minds.rows.iter().map(|x| x.uid).collect();
        // Survivors 1, 2, 4 in whatever slots swap-remove left them in,
        // deterministic for the same kill sequence.
        assert_eq!(uids, vec![4, 1, 2]);
        for (slot, row) in d.actors.rows.iter().enumerate() {
            assert_eq!(
                d.cells.occupant[usize::from(row.cell)],
                ActorId::pack(row.kind, slot as u16)
            );
            assert_eq!(d.minds.rows[slot].uid, u64::from(row.cell - 100));
        }
        assert_eq!(d.validate(5), Ok(()));
        // Compacting again is a no-op; killing everything empties it.
        d.actors_mut().compact();
        assert_eq!(d.actors.rows.len(), 3);
        for i in 0..3 {
            d.actors_mut().kill(i);
        }
        d.actors_mut().compact();
        assert!(d.actors.rows.is_empty() && d.minds.rows.is_empty());
        assert!(d.cells.occupant.iter().all(|x| x.is_none()));
    }

    #[test]
    fn validate_catches_every_broken_invariant() {
        let mut d = ChunkData::default();
        d.actors_mut().push(1, SEED, mind(1));
        d.actors_mut().push(2, SEED, mind(2));
        let (o, p, m) = (&d.cells.occupant, &d.actors.rows, &d.minds.rows);
        assert_eq!(validate(o, p, m, 5), Ok(()));
        assert!(validate(o, p, m, 0).unwrap_err().contains("unknown kind"));
        assert!(
            validate(o, p, &m[..1], 5)
                .unwrap_err()
                .contains("private rows")
        );
        let mut bad = p.clone();
        bad[1].flags = flags::DEAD;
        assert!(validate(o, &bad, m, 5).unwrap_err().contains("dead"));
        let mut bad = p.clone();
        bad[1].cell = 3;
        assert!(validate(o, &bad, m, 5).unwrap_err().contains("expects"));
        let mut bad = p.clone();
        bad[1].cell = u16::MAX;
        assert!(validate(o, &bad, m, 5).unwrap_err().contains("outside"));
        let mut bad_o = *o;
        bad_o[7] = ActorId::pack(SEED, 0);
        assert!(
            validate(&bad_o, p, m, 5)
                .unwrap_err()
                .contains("occupied cells")
        );
    }

    #[test]
    fn hash_sees_rows_order_and_minds() {
        let mut d = ChunkData::default();
        let empty = hash(&[], &[]);
        d.actors_mut().push(1, SEED, mind(1));
        d.actors_mut().push(2, SEED, mind(2));
        let h = hash(&d.actors.rows, &d.minds.rows);
        assert_ne!(h, empty);
        assert_eq!(h, hash(&d.actors.rows, &d.minds.rows));
        d.minds.rows[0].mem[3] = 5;
        assert_ne!(h, hash(&d.actors.rows, &d.minds.rows));
        d.minds.rows[0].mem[3] = 0;
        d.actors.rows.swap(0, 1);
        d.minds.rows.swap(0, 1);
        assert_ne!(h, hash(&d.actors.rows, &d.minds.rows));
    }
}
