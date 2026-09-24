//! The actor phases of a tick (`docs/ACTORS.md` §1, §6):
//!
//! ```text
//! Think    par  every due actor runs its program against the tick-start world;
//!               writes its own mind and one Intent per actor into the chunk's Intents
//! Apply    par  own chunk: sort intents by key, claim spawn cells (min key), apply:
//!               die, become, spawn winners; write result codes; clear WAKE
//! Compact  par  swap-remove dead rows, repair occupant
//! ```
//!
//! Think reads any chunk's cells and public rows (nothing writes them in
//! this phase, so the live state is the tick-start snapshot) and writes only
//! its own chunk's minds and intents. Apply and Compact touch only the chunk
//! they are handed. Every order inside a chunk is sorted-key order; every
//! key is a function of state (`uid`, tick), never of a slot or a thread.
//! Cross-chunk work (moves, hits, transfers) arrives with later steps as
//! sequential phases between Apply and Compact.

use bevy_ecs::prelude::*;

use crate::actors::{
    ActorMind, ActorPub, ActorsMut, ChunkActors, ChunkMinds, MEM_SLOTS, NEED_SLOTS, flags,
};
use crate::rng::{hash_cell, splitmix64};
use crate::rules::vm::{self, Action, Ctx, Halo, event, result};
use crate::rules::{Kinds, Remap};
use crate::sim::{SimConfig, Tick};
use crate::stage::worldgen::STREAM_UID;
use crate::stage::{CHUNK_CELLS, ChunkCells, ChunkCoord, ChunkMeta, Stage};

/// One actor's decision this tick, waiting for the resolve phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intent {
    pub slot: u16,
    /// Conflict key: `splitmix64(uid ^ splitmix64(tick))`. Lowest wins.
    pub key: u64,
    pub action: Action,
    /// Operand kind of `Become` / `Spawn`.
    pub kind: u16,
    /// Operand offset of `Spawn`.
    pub dx: i8,
    pub dy: i8,
    /// The think trapped (fuel or a fault) and was turned into `Idle`.
    pub trapped: bool,
}

/// The intents of one chunk's actors this tick. Scratch: cleared by Think,
/// consumed by Apply. `traps` counts trapped thinks since load, for the
/// status line.
#[derive(Component, Debug, Default)]
pub struct Intents {
    pub list: Vec<Intent>,
    pub traps: u32,
}

/// Per-chunk resolve scratch: the claim table (`u64::MAX` = unclaimed),
/// the cells claimed this tick (to reset them), and how many rows died.
#[derive(Component, Debug)]
pub struct Scratch {
    claim: Vec<u64>,
    touched: Vec<u16>,
    pub deaths: u32,
}

impl Default for Scratch {
    fn default() -> Self {
        Self {
            claim: vec![u64::MAX; CHUNK_CELLS],
            touched: Vec::new(),
            deaths: 0,
        }
    }
}

impl Scratch {
    /// `claim[cell] = min(claim[cell], key)`.
    fn claim(&mut self, cell: usize, key: u64) {
        let c = &mut self.claim[cell];
        if *c == u64::MAX {
            self.touched.push(cell as u16);
        }
        *c = (*c).min(key);
    }

    fn reset(&mut self) {
        for &c in &self.touched {
            self.claim[usize::from(c)] = u64::MAX;
        }
        self.touched.clear();
    }
}

/// Conflict key of an actor at a tick: a pure function of state.
#[inline]
pub fn intent_key(uid: u64, tick: u64) -> u64 {
    splitmix64(uid ^ splitmix64(tick))
}

/// Is an actor due to think at `tick`?
#[inline]
pub fn due(tick: u64, row: &ActorPub, cadence: u64) -> bool {
    row.flags & flags::WAKE != 0 || tick.wrapping_add(u64::from(row.stagger)) & (cadence - 1) == 0
}

/// The 3x3 halo around `c`, from the read-only queries of the Think phase.
fn halo<'a>(
    stage: &Stage,
    cells: &'a Query<&ChunkCells>,
    pubs: &'a Query<&ChunkActors>,
    c: ChunkCoord,
) -> Halo<'a> {
    let mut chunks = [None; 9];
    for (i, slot) in chunks.iter_mut().enumerate() {
        let (ox, oy) = ((i % 3) as i32 - 1, (i / 3) as i32 - 1);
        let Some(e) = stage.entity(ChunkCoord::new(c.x + ox, c.y + oy)) else {
            continue;
        };
        if let (Ok(cells), Ok(pubs)) = (cells.get(e), pubs.get(e)) {
            *slot = Some((cells, pubs));
        }
    }
    Halo { chunks }
}

// ---- Think ------------------------------------------------------------------------------

pub fn think(
    tick: Res<Tick>,
    cfg: Res<SimConfig>,
    kinds: Res<Kinds>,
    stage: Res<Stage>,
    cells: Query<&ChunkCells>,
    pubs: Query<&ChunkActors>,
    mut own: Query<(
        Entity,
        &ChunkCoord,
        &mut ChunkMinds,
        &mut Intents,
        &mut ChunkMeta,
    )>,
) {
    let (tick, seed) = (tick.0, cfg.seed);
    let (kinds, stage, cells, pubs) = (&*kinds, &*stage, &cells, &pubs);
    own.par_iter_mut()
        .for_each(|(e, coord, mut minds, mut intents, mut meta)| {
            intents.list.clear();
            let Ok(my) = pubs.get(e) else {
                return;
            };
            if my.rows.is_empty() {
                return;
            }
            // Built once per chunk per tick, and only if someone is due.
            let mut halo = None;
            for (slot, row) in my.rows.iter().enumerate() {
                let kind = kinds.def(row.kind);
                if !due(tick, row, kind.cadence()) {
                    continue;
                }
                let halo = halo.get_or_insert_with(|| halo_of(stage, cells, pubs, *coord));
                let mind = &mut minds.rows[slot];
                let intent = think_one(kinds, tick, seed, halo, *coord, slot as u16, row, mind);
                intents.list.push(intent);
            }
            if !intents.list.is_empty() {
                meta.dirty = true;
            }
        });
}

fn halo_of<'a>(
    stage: &Stage,
    cells: &'a Query<&ChunkCells>,
    pubs: &'a Query<&ChunkActors>,
    c: ChunkCoord,
) -> Halo<'a> {
    halo(stage, cells, pubs, c)
}

/// One actor's think: decay, die at zero, else run the program. Events read
/// by this think are cleared afterwards; a trap sets `FUEL` for the next.
fn think_one(
    kinds: &Kinds,
    tick: u64,
    seed: u64,
    halo: &Halo<'_>,
    coord: ChunkCoord,
    slot: u16,
    row: &ActorPub,
    mind: &mut ActorMind,
) -> Intent {
    let kind = kinds.def(row.kind);
    let key = intent_key(mind.uid, tick);
    let mut intent = Intent {
        slot,
        key,
        action: Action::Idle,
        kind: 0,
        dx: 0,
        dy: 0,
        trapped: false,
    };
    if vm::decay(kind, mind, tick) {
        clear_events(mind);
        intent.action = Action::Die;
        return intent;
    }
    let cell = usize::from(row.cell);
    let ctx = Ctx {
        halo,
        kind,
        cell,
        pos: coord.cell(cell),
        tick,
        rng: vm::rng_base(seed, tick, mind.uid),
        look: row.look,
        signal: row.signal,
    };
    let out = vm::think(kinds, ctx, mind);
    clear_events(mind);
    if out.trap.is_some() {
        mind.events |= event::FUEL;
    }
    if let Some(s) = out.next {
        mind.state = s;
    }
    intent.action = out.action;
    intent.kind = out.kind;
    intent.dx = out.dx;
    intent.dy = out.dy;
    intent.trapped = out.trap.is_some();
    intent
}

#[inline]
fn clear_events(mind: &mut ActorMind) {
    mind.events = 0;
    mind.hurt = 0;
    mind.hurt_dir = 0;
}

// ---- Apply ------------------------------------------------------------------------------

pub fn apply(
    tick: Res<Tick>,
    cfg: Res<SimConfig>,
    kinds: Res<Kinds>,
    mut q: Query<(
        &ChunkCoord,
        &mut ChunkCells,
        &mut ChunkActors,
        &mut ChunkMinds,
        &mut Intents,
        &mut Scratch,
    )>,
) {
    let (tick, seed, kinds) = (tick.0, cfg.seed, &*kinds);
    q.par_iter_mut().for_each(
        |(coord, mut cells, mut pubs, mut minds, mut intents, mut scratch)| {
            if intents.list.is_empty() {
                return;
            }
            // Every order below is a function of state, never of slot history.
            intents.list.sort_unstable_by_key(|i| (i.key, i.slot));

            // Claims: spawn targets that are free at tick start, lowest key wins.
            for it in &intents.list {
                if it.action != Action::Spawn {
                    continue;
                }
                let from = usize::from(pubs.rows[usize::from(it.slot)].cell);
                if let Some(cell) = local_target(from, it.dx, it.dy)
                    && cells.walkable(cell)
                    && cells.occupant[cell].is_none()
                {
                    scratch.claim(cell, it.key);
                }
            }

            let mut traps = 0;
            for it in &intents.list {
                let slot = usize::from(it.slot);
                let mut actors = ActorsMut {
                    pubs: &mut pubs.rows,
                    minds: &mut minds.rows,
                    occupant: &mut cells.occupant,
                };
                let res = match it.action {
                    Action::Idle => result::OK,
                    Action::Die => {
                        actors.kill(slot);
                        scratch.deaths += 1;
                        result::OK
                    }
                    Action::Become => change_kind(kinds, tick, &mut actors, slot, it.kind),
                    Action::Spawn => {
                        let from = usize::from(actors.pubs[slot].cell);
                        match local_target(from, it.dx, it.dy) {
                            Some(cell)
                                if usize::from(it.kind) < kinds.len()
                                    && scratch.claim[cell] == it.key =>
                            {
                                let pos = coord.cell(cell);
                                let child = newborn(
                                    kinds,
                                    it.kind,
                                    hash_cell(seed, STREAM_UID, pos.x, pos.y) ^ splitmix64(tick),
                                    tick,
                                );
                                actors.push(cell, it.kind, child);
                                result::OK
                            }
                            Some(_) => result::BLOCKED,
                            // Another chunk: until Migrate exists, a wall.
                            None => result::BLOCKED,
                        }
                    }
                };
                let m = &mut minds.rows[slot];
                m.events = (m.events & !result::MASK) | res;
                pubs.rows[slot].flags &= !flags::WAKE;
                traps += u32::from(it.trapped);
            }
            intents.traps += traps;
            scratch.reset();
        },
    );
}

/// Local index of `(dx, dy)` from `cell`, or `None` if it leaves the chunk.
#[inline]
fn local_target(cell: usize, dx: i8, dy: i8) -> Option<usize> {
    let ((ox, oy), local) = vm::offset_cell(cell, i32::from(dx), i32::from(dy));
    ((ox, oy) == (0, 0)).then_some(local)
}

/// A fresh mind of `kind`: every need at its max, memory clear, born now.
pub fn newborn(kinds: &Kinds, kind: u16, uid: u64, tick: u64) -> ActorMind {
    let def = kinds.def(kind);
    let mut needs = [0i32; NEED_SLOTS];
    for (n, d) in needs.iter_mut().zip(&def.needs) {
        *n = d.max;
    }
    ActorMind {
        uid,
        born: tick as u32,
        last_think: tick as u32,
        needs,
        mem: [0; MEM_SLOTS],
        state: 0,
        events: 0,
        hurt: 0,
        hurt_dir: 0,
        _pad: 0,
    }
}

/// Change a row's kind in place: consumable needs carry over by name
/// (clamped to the new max), point needs and needs the new kind adds start
/// at max, memory carries by name, state resets, born is now.
fn change_kind(kinds: &Kinds, tick: u64, actors: &mut ActorsMut<'_>, slot: usize, to: u16) -> u8 {
    if usize::from(to) >= kinds.len() {
        return result::REFUSED;
    }
    let from = actors.pubs[slot].kind;
    let remap = kinds.remap(from, to);
    let def = kinds.def(to);
    let old = actors.minds[slot];
    let m = &mut actors.minds[slot];
    for i in 0..NEED_SLOTS {
        m.needs[i] = match def.needs.get(i) {
            None => 0,
            Some(n) => match remap.needs[i] {
                Remap::NONE => n.max,
                src if n.decays => old.needs[usize::from(src)].clamp(0, n.max),
                _ => n.max,
            },
        };
    }
    for i in 0..MEM_SLOTS {
        m.mem[i] = match remap.mems[i] {
            Remap::NONE => 0,
            src => old.mem[usize::from(src)],
        };
    }
    m.state = 0;
    m.born = tick as u32;
    let row = &mut actors.pubs[slot];
    row.kind = to;
    actors.occupant[usize::from(row.cell)] = crate::stage::ActorId::pack(to, slot as u16);
    result::OK
}

// ---- Compact ----------------------------------------------------------------------------

pub fn compact(
    mut q: Query<(
        &mut ChunkCells,
        &mut ChunkActors,
        &mut ChunkMinds,
        &mut Intents,
        &mut Scratch,
    )>,
) {
    q.par_iter_mut().for_each(
        |(mut cells, mut pubs, mut minds, mut intents, mut scratch)| {
            intents.list.clear();
            if scratch.deaths == 0 {
                return;
            }
            ActorsMut {
                pubs: &mut pubs.rows,
                minds: &mut minds.rows,
                occupant: &mut cells.occupant,
            }
            .compact();
            scratch.deaths = 0;
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{SEED, TREE};

    #[test]
    fn due_follows_cadence_stagger_and_wake() {
        let mut row = ActorPub {
            cell: 0,
            kind: 0,
            stagger: 3,
            signal: 0,
            look: 0,
            flags: 0,
            _pad: 0,
        };
        assert!(due(5, &row, 8)); // (5 + 3) % 8 == 0
        assert!(!due(6, &row, 8));
        assert!(due(13, &row, 8));
        assert!(due(6, &row, 1)); // cadence 1: every tick
        row.flags = flags::WAKE;
        assert!(due(6, &row, 8));
    }

    #[test]
    fn keys_depend_on_uid_and_tick_only() {
        assert_eq!(intent_key(7, 100), intent_key(7, 100));
        assert_ne!(intent_key(7, 100), intent_key(8, 100));
        assert_ne!(intent_key(7, 100), intent_key(7, 101));
    }

    #[test]
    fn newborn_starts_full_and_become_carries_by_name() {
        let kinds = Kinds::builtin();
        let mut d = crate::stage::ChunkData::default();
        let m = newborn(&kinds, SEED, 42, 1000);
        assert_eq!(m.needs[0], kinds.def(SEED).needs[0].max);
        assert_eq!(m.needs[1], 1);
        assert_eq!((m.born, m.last_think, m.uid), (1000, 1000, 42));
        d.actors_mut().push(5, SEED, m);
        d.minds.rows[0].needs[0] = 100; // some water left
        d.minds.rows[0].mem[0] = 25; // lit
        let mut a = d.actors_mut();
        assert_eq!(change_kind(&kinds, 2000, &mut a, 0, TREE), result::OK);
        assert_eq!(a.pubs[0].kind, TREE);
        assert_eq!(a.occupant[5], crate::stage::ActorId::pack(TREE, 0));
        let m = a.minds[0];
        assert_eq!(m.needs[0], 100, "water carries over by name");
        assert_eq!(m.needs[1], 100, "health is points: reset to the tree's max");
        assert_eq!(m.mem[0], 0, "`lit` has no namesake in tree");
        assert_eq!((m.born, m.state, m.uid), (2000, 0, 42));
        assert_eq!(change_kind(&kinds, 2000, &mut a, 0, 99), result::REFUSED);
        assert_eq!(a.pubs[0].kind, TREE);
        assert_eq!(local_target(63, 1, 0), None);
        assert_eq!(local_target(63, -1, 1), Some(62 + 64));
    }
}
