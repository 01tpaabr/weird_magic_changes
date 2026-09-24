//! The actor phases of a tick (`docs/ACTORS.md` §1, §6):
//!
//! ```text
//! Think    par  every due actor runs its program against the tick-start world;
//!               writes its own mind and one Intent per actor into the chunk's Intents
//! Apply    par  own chunk: sort intents by key, claim target cells (min key), apply:
//!               die, become, drink, in-chunk move/spawn winners; write result codes;
//!               clear WAKE; cross-chunk move/spawn go to the Outbox
//! Migrate  seq  every Outbox in stage.active() order, sorted by key: move/spawn into
//!               another chunk's cell if it is free and untouched this tick
//! Compact  par  swap-remove dead rows, repair occupant, reset the claim table
//! ```
//!
//! Think reads any chunk's cells and public rows (nothing writes them in
//! this phase, so the live state is the tick-start snapshot) and writes only
//! its own chunk's minds and intents. Apply and Compact touch only the chunk
//! they are handed; Migrate is the one sequential phase, because it touches
//! two chunks at once. Every order is sorted-key order; every key is a
//! function of state (`uid`, tick), never of a slot or a thread. A cell
//! whose occupancy changed this tick (vacated, claimed, filled) is not
//! enterable until the next tick, so moves never chain.

use bevy_ecs::prelude::*;

use crate::actors::{
    ActorMind, ActorPub, ActorsMut, ChunkActors, ChunkMinds, MEM_SLOTS, NEED_SLOTS, flags,
};
use crate::rng::{hash_cell, splitmix64};
use crate::rules::vm::{self, Action, Ctx, Halo, event, pred, result};
use crate::rules::{Kinds, Remap};
use crate::sim::{SimConfig, Tick};
use crate::stage::worldgen::STREAM_UID;
use crate::stage::{ActorId, CHUNK_CELLS, ChunkCells, ChunkCoord, ChunkMeta, Ground, Stage};

/// One actor's decision this tick, waiting for the resolve phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intent {
    pub slot: u16,
    /// Conflict key: `splitmix64(uid ^ splitmix64(tick))`. Lowest wins.
    pub key: u64,
    pub action: Action,
    /// Operand kind of `Become` / `Spawn`; `1` for a validated `Drink`.
    pub kind: u16,
    /// Operand offset of `Spawn` / `Move` (a unit step) / `Drink`.
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

/// A cross-chunk effect, from a source chunk's actor to a cell of `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Effect {
    pub key: u64,
    /// Row in the source chunk.
    pub slot: u16,
    pub what: EffectKind,
    pub to: ChunkCoord,
    /// Local cell in `to`.
    pub cell: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    Move,
    Spawn(u16),
}

/// Cross-chunk effects a chunk's actors asked for this tick. Filled by
/// Apply, drained by Migrate.
#[derive(Component, Debug, Default)]
pub struct Outbox {
    pub list: Vec<Effect>,
}

/// Migrate's working list: every outbox of the tick, sorted by key.
#[derive(Resource, Debug, Default)]
pub struct MigrateScratch {
    list: Vec<(Entity, Effect)>,
}

/// Per-chunk resolve scratch: the claim table (`UNCLAIMED` = nobody wants
/// the cell and nothing changed there this tick, `TOUCHED` = its occupancy
/// changed this tick, else the lowest key that claimed it), the cells to
/// reset, and how many rows died.
#[derive(Component, Debug)]
pub struct Scratch {
    claim: Vec<u64>,
    touched: Vec<u16>,
    pub deaths: u32,
}

const UNCLAIMED: u64 = u64::MAX;
const TOUCHED: u64 = u64::MAX - 1;

impl Default for Scratch {
    fn default() -> Self {
        Self {
            claim: vec![UNCLAIMED; CHUNK_CELLS],
            touched: Vec::new(),
            deaths: 0,
        }
    }
}

impl Scratch {
    /// `claim[cell] = min(claim[cell], key)`.
    fn claim(&mut self, cell: usize, key: u64) {
        let c = &mut self.claim[cell];
        if *c == UNCLAIMED {
            self.touched.push(cell as u16);
        }
        *c = (*c).min(key);
    }

    /// The cell's occupancy changed this tick: nobody else enters it.
    fn touch(&mut self, cell: usize) {
        if self.claim[cell] == UNCLAIMED {
            self.touched.push(cell as u16);
        }
        self.claim[cell] = TOUCHED;
    }

    /// Free at tick start and untouched since: a cross-chunk mover may enter.
    fn enterable(&self, cell: usize) -> bool {
        self.claim[cell] == UNCLAIMED
    }

    fn reset(&mut self) {
        for &c in &self.touched {
            self.claim[usize::from(c)] = UNCLAIMED;
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
fn halo_of<'a>(
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

/// One actor's think: decay, die at zero, else run the program and turn
/// its outcome into an intent. Events read by this think are cleared
/// afterwards; a trap sets `FUEL` for the next.
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
    let (lx, ly) = local_xy(cell);
    match out.action {
        Action::Move => {
            let (dx, dy) = step_toward(halo, lx, ly, i32::from(out.dx), i32::from(out.dy));
            intent.dx = dx as i8;
            intent.dy = dy as i8;
        }
        Action::Drink => {
            let water = pred::ground(Ground::Water as u8);
            let valid = halo.matches(lx, ly, i32::from(out.dx), i32::from(out.dy), water);
            intent.kind = u16::from(valid);
        }
        _ => {}
    }
    intent
}

#[inline]
fn clear_events(mind: &mut ActorMind) {
    mind.events = 0;
    mind.hurt = 0;
    mind.hurt_dir = 0;
}

#[inline]
fn local_xy(cell: usize) -> (i32, i32) {
    ((cell as i32) & 63, (cell as i32) >> 6)
}

/// The eight directions clockwise from north.
const DIRS8: [(i32, i32); 8] = [
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
];

/// Reduce a move to one step: `(sign dx, sign dy)`, and if that cell is not
/// free at tick start, the 45-degree neighbour clockwise, then the one
/// counter-clockwise. None free: the straight step, which Apply will
/// report as BLOCKED. `(0, 0)` stays `(0, 0)`.
pub fn step_toward(halo: &Halo<'_>, lx: i32, ly: i32, dx: i32, dy: i32) -> (i32, i32) {
    let s = (dx.signum(), dy.signum());
    if s == (0, 0) {
        return s;
    }
    let i = DIRS8.iter().position(|&d| d == s).expect("a unit step");
    for j in [i, (i + 1) % 8, (i + 7) % 8] {
        let (cx, cy) = DIRS8[j];
        if halo.free(lx, ly, cx, cy) {
            return (cx, cy);
        }
    }
    s
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
        &mut Outbox,
    )>,
) {
    let (tick, seed, kinds) = (tick.0, cfg.seed, &*kinds);
    q.par_iter_mut().for_each(
        |(coord, mut cells, mut pubs, mut minds, mut intents, mut scratch, mut outbox)| {
            outbox.list.clear();
            if intents.list.is_empty() {
                return;
            }
            // Every order below is a function of state, never of slot history.
            intents.list.sort_unstable_by_key(|i| (i.key, i.slot));

            // Claims: in-chunk targets free at tick start, lowest key wins.
            for it in &intents.list {
                if !matches!(it.action, Action::Move | Action::Spawn) {
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
                let from = usize::from(pubs.rows[slot].cell);
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
                    Action::Drink => {
                        let kind = kinds.def(actors.pubs[slot].kind);
                        match (it.kind, kind.need_named("water")) {
                            (1, Some(i)) => {
                                actors.minds[slot].needs[i] = kind.needs[i].max;
                                result::OK
                            }
                            _ => result::REFUSED,
                        }
                    }
                    Action::Move => match target_of(*coord, from, it.dx, it.dy) {
                        Where::Here(cell) if scratch.claim[cell] == it.key => {
                            let kind = actors.pubs[slot].kind;
                            actors.occupant[from] = ActorId::NONE;
                            scratch.touch(from);
                            actors.occupant[cell] = ActorId::pack(kind, it.slot);
                            actors.pubs[slot].cell = cell as u16;
                            result::OK
                        }
                        Where::Here(_) => result::BLOCKED,
                        Where::Elsewhere(to, cell) => {
                            outbox.list.push(Effect {
                                key: it.key,
                                slot: it.slot,
                                what: EffectKind::Move,
                                to,
                                cell: cell as u16,
                            });
                            result::NONE // Migrate decides
                        }
                    },
                    Action::Spawn => match target_of(*coord, from, it.dx, it.dy) {
                        Where::Here(cell)
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
                        Where::Here(_) => result::BLOCKED,
                        Where::Elsewhere(to, cell) if usize::from(it.kind) < kinds.len() => {
                            outbox.list.push(Effect {
                                key: it.key,
                                slot: it.slot,
                                what: EffectKind::Spawn(it.kind),
                                to,
                                cell: cell as u16,
                            });
                            result::NONE
                        }
                        Where::Elsewhere(..) => result::REFUSED,
                    },
                };
                let m = &mut minds.rows[slot];
                m.events = (m.events & !result::MASK) | res;
                pubs.rows[slot].flags &= !flags::WAKE;
                traps += u32::from(it.trapped);
            }
            intents.traps += traps;
        },
    );
}

/// Where `(dx, dy)` from `cell` lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Where {
    Here(usize),
    Elsewhere(ChunkCoord, usize),
}

#[inline]
fn target_of(coord: ChunkCoord, cell: usize, dx: i8, dy: i8) -> Where {
    let ((ox, oy), local) = vm::offset_cell(cell, i32::from(dx), i32::from(dy));
    if (ox, oy) == (0, 0) {
        Where::Here(local)
    } else {
        Where::Elsewhere(ChunkCoord::new(coord.x + ox, coord.y + oy), local)
    }
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
    actors.occupant[usize::from(row.cell)] = ActorId::pack(to, slot as u16);
    result::OK
}

// ---- Migrate ----------------------------------------------------------------------------

type ChunkQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut ChunkCells,
        &'static mut ChunkActors,
        &'static mut ChunkMinds,
        &'static mut Scratch,
        &'static mut ChunkMeta,
    ),
>;

/// Cross-chunk moves and spawns, one thread, every outbox in `stage.active()`
/// order and then every effect in key order. A target cell must be loaded,
/// walkable, empty and untouched this tick; an in-chunk winner from Apply
/// therefore always beats a cross-chunk one (home advantage, decision 30).
pub fn migrate(
    tick: Res<Tick>,
    cfg: Res<SimConfig>,
    kinds: Res<Kinds>,
    stage: Res<Stage>,
    mut work: ResMut<MigrateScratch>,
    mut outboxes: Query<&mut Outbox>,
    mut chunks: ChunkQuery,
) {
    let (tick, seed) = (tick.0, cfg.seed);
    work.list.clear();
    for &(_, e) in stage.active() {
        if let Ok(mut ob) = outboxes.get_mut(e) {
            work.list.extend(ob.list.drain(..).map(|fx| (e, fx)));
        }
    }
    if work.list.is_empty() {
        return;
    }
    work.list.sort_unstable_by_key(|(_, fx)| (fx.key, fx.slot));
    for &(src_e, fx) in &work.list {
        let slot = usize::from(fx.slot);
        let cell = usize::from(fx.cell);
        let Some(dst_e) = stage.entity(fx.to) else {
            // Not loaded: a wall.
            set_result(&mut chunks, src_e, slot, result::BLOCKED);
            continue;
        };
        let Ok([mut src, mut dst]) = chunks.get_many_mut([src_e, dst_e]) else {
            set_result(&mut chunks, src_e, slot, result::BLOCKED);
            continue;
        };
        let (dst_cells, dst_pubs, dst_minds, dst_scratch, dst_meta) = &mut dst;
        let ok = dst_cells.walkable(cell)
            && dst_cells.occupant[cell].is_none()
            && dst_scratch.enterable(cell);
        if !ok {
            set_result_in(&mut src.2, slot, result::BLOCKED);
            continue;
        }
        let (src_cells, src_pubs, src_minds, src_scratch, src_meta) = &mut src;
        let mut to = ActorsMut {
            pubs: &mut dst_pubs.rows,
            minds: &mut dst_minds.rows,
            occupant: &mut dst_cells.occupant,
        };
        match fx.what {
            EffectKind::Move => {
                let row = src_pubs.rows[slot];
                let mut mind = src_minds.rows[slot];
                mind.events = (mind.events & !result::MASK) | result::OK;
                let new = to.push(cell, row.kind, mind);
                let moved = &mut to.pubs[usize::from(new)];
                moved.signal = row.signal;
                moved.look = row.look;
                let mut from = ActorsMut {
                    pubs: &mut src_pubs.rows,
                    minds: &mut src_minds.rows,
                    occupant: &mut src_cells.occupant,
                };
                from.kill(slot);
                src_scratch.deaths += 1;
                src_scratch.touch(usize::from(row.cell));
            }
            EffectKind::Spawn(kind) => {
                let pos = fx.to.cell(cell);
                let child = newborn(
                    &kinds,
                    kind,
                    hash_cell(seed, STREAM_UID, pos.x, pos.y) ^ splitmix64(tick),
                    tick,
                );
                to.push(cell, kind, child);
                set_result_in(src_minds, slot, result::OK);
            }
        }
        dst_scratch.touch(cell);
        dst_meta.dirty = true;
        src_meta.dirty = true;
    }
}

fn set_result(chunks: &mut ChunkQuery, e: Entity, slot: usize, res: u8) {
    if let Ok((_, _, mut minds, _, _)) = chunks.get_mut(e) {
        set_result_in(&mut minds, slot, res);
    }
}

#[inline]
fn set_result_in(minds: &mut ChunkMinds, slot: usize, res: u8) {
    let m = &mut minds.rows[slot];
    m.events = (m.events & !result::MASK) | res;
}

// ---- Compact ----------------------------------------------------------------------------

pub fn compact(
    mut q: Query<(
        &mut ChunkCells,
        &mut ChunkActors,
        &mut ChunkMinds,
        &mut Intents,
        &mut Scratch,
        &mut Outbox,
    )>,
) {
    q.par_iter_mut().for_each(
        |(mut cells, mut pubs, mut minds, mut intents, mut scratch, mut outbox)| {
            intents.list.clear();
            outbox.list.clear();
            scratch.reset();
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

    fn halo_one<'a>(cells: &'a ChunkCells, actors: &'a ChunkActors) -> Halo<'a> {
        let mut chunks = [None; 9];
        chunks[4] = Some((cells, actors));
        Halo { chunks }
    }

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
        assert_eq!(a.occupant[5], ActorId::pack(TREE, 0));
        let m = a.minds[0];
        assert_eq!(m.needs[0], 100, "water carries over by name");
        assert_eq!(m.needs[1], 100, "health is points: reset to the tree's max");
        assert_eq!(m.mem[0], 0, "`lit` has no namesake in tree");
        assert_eq!((m.born, m.state, m.uid), (2000, 0, 42));
        assert_eq!(change_kind(&kinds, 2000, &mut a, 0, 99), result::REFUSED);
        assert_eq!(a.pubs[0].kind, TREE);
        assert_eq!(local_target(63, 1, 0), None);
        assert_eq!(local_target(63, -1, 1), Some(62 + 64));
        let c = ChunkCoord::new(2, 3);
        assert_eq!(
            target_of(c, 63, 1, 0),
            Where::Elsewhere(ChunkCoord::new(3, 3), 0)
        );
        assert_eq!(
            target_of(c, 0, -1, -1),
            Where::Elsewhere(ChunkCoord::new(1, 2), CHUNK_CELLS - 1)
        );
        assert_eq!(target_of(c, 70, 1, 1), Where::Here(70 + 65));
    }

    #[test]
    fn a_step_slides_around_a_blocked_cell() {
        let mut cells = ChunkCells::default();
        let actors = ChunkActors::default();
        // Actor at (10, 10); rock straight east at (11, 10).
        cells.feature[10 * 64 + 11] = crate::stage::Feature::Rock;
        assert_eq!(
            step_toward(&halo_one(&cells, &actors), 10, 10, 5, 0),
            (1, 1),
            "clockwise first: south-east"
        );
        cells.occupant[11 * 64 + 11] = ActorId::pack(0, 0);
        assert_eq!(
            step_toward(&halo_one(&cells, &actors), 10, 10, 5, 0),
            (1, -1),
            "then counter-clockwise"
        );
        cells.ground[9 * 64 + 11] = Ground::Water;
        let halo = halo_one(&cells, &actors);
        assert_eq!(
            step_toward(&halo, 10, 10, 5, 0),
            (1, 0),
            "nothing free: the straight step"
        );
        assert_eq!(step_toward(&halo, 10, 10, -3, 7), (-1, 1));
        assert_eq!(step_toward(&halo, 10, 10, 0, 0), (0, 0));
        // Off the halo's edge counts as blocked.
        assert_eq!(step_toward(&halo, 0, 5, -1, 0), (-1, 0));
    }
}
